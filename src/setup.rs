//! Host setup. Planning and applying are separate so callers can inspect changes.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const HOSTS: &[&str] = &[
    "claude",
    "codex",
    "copilot",
    "pi",
    "omp",
    "opencode",
    "kilo",
    "kilocode",
    "cursor",
    "gemini",
    "vscode",
    "droid",
    "windsurf",
    "cline",
    "roo",
    "antigravity",
    "kimi",
    "hermes",
    "vibe",
    "openclaw",
];
const INSTRUCTIONS: &str = "<!-- retok managed instructions v1 -->\n# Retok\n\nInstalled executable (JSON string): __RETOK_EXECUTABLE__\nIf retok is not on PATH, invoke this absolute executable using your shell's\nliteral path quoting, followed by the same arguments shown below.\n\nFor final model-facing output of suitable non-interactive commands, use\n`retok run -- COMMAND ARG...` explicitly. This is optional agent guidance,\nnot automatic output compression. Keep ordinary commands for programmatic\npipelines, redirected data, interactive sessions, and scripts that parse output.\nNever change approval or sandbox rules to use Retok.\n<!-- /retok managed instructions v1 -->\n";

/// Explicit roots keep tests independent of the process environment and real homes.
#[derive(Clone, Debug)]
pub struct Roots {
    pub home: PathBuf,
    pub project: PathBuf,
    pub config: PathBuf,
    pub claude: PathBuf,
    /// Pi and OMP agent directories (the override already includes `agent`).
    pub pi: PathBuf,
    pub omp: PathBuf,
    pub codex: PathBuf,
    pub copilot: PathBuf,
    pub factory: PathBuf,
    pub kimi: PathBuf,
    pub hermes: PathBuf,
    pub vibe: PathBuf,
    pub openclaw: PathBuf,
    pub openclaw_config: Option<PathBuf>,
    pub executable: PathBuf,
}
impl Roots {
    pub fn new(home: PathBuf, project: PathBuf) -> Self {
        Self {
            config: home.join(".config"),
            claude: home.join(".claude"),
            pi: home.join(".pi/agent"),
            omp: home.join(".omp/agent"),
            codex: home.join(".codex"),
            copilot: home.join(".copilot"),
            factory: home.join(".factory"),
            kimi: home.join(".kimi-code"),
            hermes: home.join(".hermes"),
            vibe: home.join(".vibe"),
            openclaw: home.join(".openclaw"),
            openclaw_config: None,
            executable: home.join("bin/retok"),
            home,
            project,
        }
    }
    fn environment() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .context("home directory unavailable")?;
        let mut roots = Self::new(home.into(), std::env::current_dir()?);
        roots.executable = std::env::current_exe()?;
        for (name, path) in [
            ("XDG_CONFIG_HOME", &mut roots.config),
            ("CLAUDE_CONFIG_DIR", &mut roots.claude),
            ("PI_CODING_AGENT_DIR", &mut roots.pi),
            ("PI_CODING_AGENT_DIR", &mut roots.omp),
            ("CODEX_HOME", &mut roots.codex),
            ("COPILOT_HOME", &mut roots.copilot),
            ("KIMI_CODE_HOME", &mut roots.kimi),
            ("HERMES_HOME", &mut roots.hermes),
            ("VIBE_HOME", &mut roots.vibe),
            ("OPENCLAW_STATE_DIR", &mut roots.openclaw),
        ] {
            if let Some(value) = std::env::var_os(name).filter(|v| !v.is_empty()) {
                *path = value.into();
            }
        }
        if let Some(home) = std::env::var_os("FACTORY_HOME_OVERRIDE").filter(|v| !v.is_empty()) {
            roots.factory = PathBuf::from(home).join(".factory");
        }
        roots.openclaw_config = std::env::var_os("OPENCLAW_CONFIG_PATH")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        Ok(roots)
    }
}

#[derive(Default)]
struct Options {
    help: bool,
    agent: Option<String>,
    all: bool,
    project: bool,
    replace: bool,
    dry: bool,
    uninstall: bool,
    show: bool,
    instructions_only: bool,
}
fn options(args: &[OsString]) -> Result<Options> {
    let mut out = Options::default();
    let mut scope = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str().context("setup options must be UTF-8")? {
            "--agent" => {
                ensure!(out.agent.is_none(), "use --agent once");
                let host = args
                    .next()
                    .and_then(|s| s.to_str())
                    .context("--agent needs a host")?;
                ensure!(
                    HOSTS.contains(&host),
                    "unknown agent {host}; choose {}",
                    HOSTS.join(", ")
                );
                out.agent = Some(if host == "kilocode" {
                    "kilo".into()
                } else {
                    host.into()
                });
            }
            "--help" | "-h" => out.help = true,
            "--all" => out.all = true,
            "--project" => {
                ensure!(scope != Some(false), "choose --project or --global");
                scope = Some(true);
                out.project = true;
            }
            "--global" | "-g" => {
                ensure!(scope != Some(true), "choose --project or --global");
                scope = Some(false);
            }
            "--replace-rtk" => out.replace = true,
            "--dry-run" => out.dry = true,
            "--uninstall" => out.uninstall = true,
            "--show" => out.show = true,
            "--instructions-only" => out.instructions_only = true,
            other => bail!("unknown setup option: {other}"),
        }
    }
    ensure!(!(out.all && out.agent.is_some()), "choose --agent or --all");
    ensure!(
        !(out.replace && out.uninstall),
        "--replace-rtk and --uninstall cannot be combined"
    );
    ensure!(
        !out.instructions_only || out.agent.is_some(),
        "--instructions-only requires --agent HOST"
    );
    Ok(out)
}

#[derive(Clone)]
struct Host {
    home: PathBuf,
    executable: PathBuf,
    name: &'static str,
    root: PathBuf,
    settings: Vec<PathBuf>,
    output: Option<PathBuf>,
    instructions: Option<PathBuf>,
    dedicated: bool,
    plugin: Option<PathBuf>,
    limitation: Option<&'static str>,
}
fn host(name: &'static str, r: &Roots, project: bool) -> Host {
    let base = if project { &r.project } else { &r.home };
    let root = match (name, project) {
        ("claude", false) => r.claude.clone(),
        ("pi", false) => r.pi.clone(),
        ("omp", false) => r.omp.clone(),
        ("codex", false) => r.codex.clone(),
        ("copilot" | "vscode", false) => r.copilot.clone(),
        ("droid", false) => r.factory.clone(),
        ("kimi", false) => r.kimi.clone(),
        ("hermes", false) => r.hermes.clone(),
        ("vibe", false) => r.vibe.clone(),
        ("openclaw", false) => r.openclaw.clone(),
        ("kimi" | "hermes" | "openclaw", true) => r.project.clone(),
        ("opencode" | "kilo", false) => r.config.join(name),
        ("windsurf", false) => r.home.join(".codeium/windsurf"),
        ("antigravity", false) => r.home.join(".gemini"),
        ("copilot" | "vscode", true) => base.join(".github"),
        ("droid", true) => base.join(".factory"),
        ("cline", _) => base.join(".clinerules"),
        ("antigravity", true) => base.join(".agents"),
        _ => base.join(format!(".{name}")),
    };
    let mut h = Host {
        home: r.home.clone(),
        executable: r.executable.clone(),
        name,
        root,
        settings: vec![],
        output: None,
        instructions: None,
        dedicated: false,
        plugin: None,
        limitation: None,
    };
    match name {
        "claude" => {
            h.output = Some(h.root.join("settings.json"));
            h.settings.push(h.root.join("settings.json"));
            if project {
                h.settings.push(h.root.join("settings.local.json"));
            }
            h.limitation = Some("requires Claude Code >=2.1.121 (version not probed)");
        }
        "codex" => {
            h.output = Some(h.root.join("hooks.json"));
            h.settings.push(h.root.join("hooks.json"));
            h.instructions = Some(if project {
                base.join("AGENTS.md")
            } else {
                h.root.join("AGENTS.md")
            });
            h.limitation = Some(
                "requires host hook support and /hooks trust review; host loading and approval behavior not probed",
            );
        }
        "copilot" | "vscode" => {
            h.output = Some(h.root.join("hooks/retok.json"));
        }
        "cursor" => {
            h.output = Some(h.root.join("hooks.json"));
            h.settings.push(h.root.join("hooks.json"));
            if project {
                h.instructions = Some(h.root.join("rules/retok.mdc"));
                h.dedicated = true;
            }
        }
        "gemini" => {
            h.output = Some(h.root.join("settings.json"));
            h.settings.push(h.root.join("settings.json"));
            h.instructions = Some(if project {
                base.join("GEMINI.md")
            } else {
                h.root.join("GEMINI.md")
            });
        }
        "droid" => {
            h.settings = vec![
                h.root.join("hooks.json"),
                h.root.join("hooks/hooks.json"),
                h.root.join("settings.json"),
            ];
            h.instructions = Some(if project {
                base.join("AGENTS.md")
            } else {
                h.root.join("AGENTS.md")
            });
        }
        "windsurf" => {
            h.settings.push(h.root.join("hooks.json"));
            h.instructions = Some(if project {
                h.root.join("rules/retok.md")
            } else {
                h.root.join("memories/global_rules.md")
            });
            h.dedicated = project;
        }
        "cline" => {
            if project {
                h.instructions = Some(h.root.join("retok.md"));
                h.dedicated = true;
            } else {
                h.limitation = Some("global rule directory is not qualified; select --project");
            }
        }
        "roo" => {
            h.instructions = Some(h.root.join("rules/retok.md"));
            h.dedicated = true;
        }
        "antigravity" => {
            h.instructions = Some(if project {
                h.root.join("rules/retok.md")
            } else {
                h.root.join("GEMINI.md")
            });
            h.dedicated = project;
        }
        "kimi" | "vibe" => {
            h.instructions = Some(if name == "vibe" && project {
                base.join("AGENTS.md")
            } else {
                h.root.join("AGENTS.md")
            });
            h.limitation = Some("guidance only; no neutral native output replacement is available");
        }
        "openclaw" => {
            if project {
                h.instructions = Some(base.join("AGENTS.md"));
                h.limitation = Some("guidance only; RTK plugins require manual migration");
            } else {
                h.limitation = Some("run --project in OpenClaw workspace");
            }
        }
        "hermes" => {
            h.instructions = Some(if project {
                [".hermes.md", "HERMES.md"]
                    .into_iter()
                    .map(|file| base.join(file))
                    .find(|path| path.exists())
                    .unwrap_or_else(|| base.join("AGENTS.md"))
            } else {
                h.root.join("SOUL.md")
            });
            h.limitation =
                Some("project guidance only; global setup supports native post-output compaction");
        }
        "pi" | "omp" => {
            h.plugin = Some(h.root.join("extensions/retok.ts"));
        }
        "opencode" => {
            h.plugin = Some(h.root.join("plugins/retok.ts"));
        }
        "kilo" => {
            h.plugin = Some(h.root.join("plugin/retok.ts"));
            h.limitation =
                Some("current-generation Kilo plugin; legacy extension rules are unchanged");
        }
        "kilocode" => unreachable!(),
        _ => unreachable!(),
    }
    h
}
fn mode(h: &Host) -> &'static str {
    if h.plugin.is_some() {
        "native post-output plugin"
    } else if h.output.is_some() && matches!(h.name, "copilot" | "vscode") {
        "native CLI post-output hook; VS Code native rewrite unavailable"
    } else if h.output.is_some() && h.name != "claude" {
        "native pre-execution hook"
    } else if h.output.is_some() {
        "native post-output hook"
    } else {
        "instructions only (agent must choose Retok explicitly)"
    }
}
fn prehook_unavailable(name: &str) -> Option<&'static str> {
    match name {
        "gemini" | "vscode" => Some(
            "native rewrite unavailable: rewritten commands do not preserve host deny/ask policy",
        ),
        "cursor" => Some(
            "native rewrite unavailable: hook schema and permission behavior are not qualified",
        ),
        "droid" => Some("native rewrite unavailable: host execution policy is not qualified"),
        "codex" | "vibe" if cfg!(windows) => {
            Some("native pre-execution adapter is POSIX-only; Windows passes through")
        }
        _ => None,
    }
}

/// Resolve ordinary managed-file/directory symlinks once, pinning their exact
/// destination in the plan. Dangling links and loops remain errors.
fn config_target(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(_) => fs::canonicalize(path).with_context(|| format!("resolving {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().context("configuration path has no parent")?;
            Ok(config_target(parent)?.join(
                path.file_name()
                    .context("configuration path has no filename")?,
            ))
        }
        Err(e) => Err(e.into()),
    }
}
fn check_config_target(path: &Path) -> Result<()> {
    config_target(path).map(|_| ())
}
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    check_config_target(path)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}
// Parse maps explicitly: Value's arbitrary-precision visitor gives a special
// meaning to an ordinary key named "$serde_json::private::Number".
struct RawObject(Vec<(String, Box<serde_json::value::RawValue>)>);
impl<'de> serde::Deserialize<'de> for RawObject {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> serde::de::Visitor<'de> for ObjectVisitor {
            type Value = RawObject;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object without duplicate keys")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<RawObject, A::Error> {
                let mut seen = std::collections::HashSet::new();
                let mut fields = vec![];
                while let Some(key) = a.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(serde::de::Error::custom("duplicate object key"));
                    }
                    fields.push((key, a.next_value()?));
                }
                Ok(RawObject(fields))
            }
        }
        d.deserialize_map(ObjectVisitor)
    }
}
fn raw_value(raw: &serde_json::value::RawValue, depth: usize) -> Result<Value> {
    ensure!(depth < 128, "JSON nesting limit exceeded");
    match raw.get().as_bytes().first() {
        Some(b'{') => {
            let object: RawObject = serde_json::from_str(raw.get())?;
            let mut fields = serde_json::Map::new();
            for (key, value) in object.0 {
                fields.insert(key, raw_value(&value, depth + 1)?);
            }
            Ok(Value::Object(fields))
        }
        Some(b'[') => {
            let values: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(raw.get())?;
            Ok(Value::Array(
                values
                    .iter()
                    .map(|v| raw_value(v, depth + 1))
                    .collect::<Result<_>>()?,
            ))
        }
        _ => Ok(serde_json::from_str(raw.get())?),
    }
}
fn json_file(path: &Path, bytes: Option<&[u8]>) -> Result<Value> {
    let value = match bytes {
        Some(bytes) => (|| -> Result<Value> {
            let raw: Box<serde_json::value::RawValue> =
                serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))?;
            raw_value(&raw, 0)
        })()
        .with_context(|| {
            format!(
                "invalid or duplicate-key JSON; unchanged: {}",
                path.display()
            )
        })?,
        None => json!({}),
    };
    ensure!(
        value.is_object(),
        "expected JSON object; unchanged: {}",
        path.display()
    );
    Ok(value)
}

// Recognize full invocations, never arbitrary substrings, shell pipelines or scripts elsewhere.
fn rtk_entry(entry: &Value, h: &Host, stock_hashes: &[(&str, usize, &str)]) -> Result<bool> {
    let source = if h.name == "vscode" {
        "copilot"
    } else {
        h.name
    };
    let mut stock = 0;
    let mut custom = 0;
    if let Some(exec) = entry.get("exec") {
        if exec.as_str() == Some("rtk") && entry.get("args") == Some(&json!(["hook", source])) {
            stock += 1;
        } else {
            custom += 1;
        }
    } else if entry.get("args").is_some() {
        custom += 1;
    }
    for key in ["command", "bash", "powershell"] {
        let Some(value) = entry.get(key) else {
            continue;
        };
        let Some(command) = value.as_str() else {
            custom += 1;
            continue;
        };
        let script_name = if h.name == "gemini" {
            "rtk-hook-gemini.sh"
        } else {
            "rtk-rewrite.sh"
        };
        let script = h.root.join("hooks").join(script_name);
        let relative = format!("~/.{}/hooks/{script_name}", h.name);
        let invocation_matches = |path: &str| {
            [
                path.to_string(),
                format!("\"{path}\""),
                format!("bash {path}"),
                format!("bash \"{path}\""),
                format!("sh {path}"),
                format!("sh \"{path}\""),
            ]
            .iter()
            .any(|known| known == command)
        };
        let tilde = invocation_matches(&relative);
        let known_script = script.to_str().is_some_and(invocation_matches) || tilde;
        if command == format!("rtk hook {source}") {
            stock += 1;
        } else if known_script {
            ensure!(
                !tilde || h.root == h.home.join(format!(".{}", h.name)),
                "RTK script references another scope/home; manual migration required; no files changed"
            );
            let bytes = read(&script)?.with_context(|| {
                format!(
                    "{}: RTK script is missing; manual migration required; no files changed",
                    script.display()
                )
            })?;
            ensure!(
                stock_match(&format!("script-{}", h.name), &bytes, stock_hashes),
                "{}: modified or unknown RTK script; manual migration required; no files changed",
                script.display()
            );
            stock += 1;
        } else {
            custom += 1;
        }
    }
    // OS overrides may carry a different command. Do not erase them implicitly.
    custom += ["windows", "linux", "osx"]
        .iter()
        .filter(|key| entry.get(**key).is_some())
        .count();
    ensure!(
        stock == 0 || custom == 0,
        "mixed RTK and custom hook invocations; manual migration required; no files changed"
    );
    Ok(stock > 0)
}
fn other_invocations(entry: &Value, h: &Host) -> bool {
    let keys: &[&str] = if h.name == "copilot" {
        &["command", "bash", "powershell", "windows", "linux", "osx"]
    } else {
        &[
            "exec",
            "args",
            "bash",
            "powershell",
            "windows",
            "linux",
            "osx",
        ]
    };
    keys.iter().any(|key| entry.get(*key).is_some())
}
fn legacy_owned_entry(entry: &Value, h: &Host) -> bool {
    if other_invocations(entry, h) {
        return false;
    }
    if h.name == "copilot" {
        entry.get("exec").and_then(Value::as_str) == Some("retok")
            && entry.get("args") == Some(&json!(["hook", "copilot"]))
    } else {
        entry.get("command").and_then(Value::as_str)
            == Some(format!("retok hook {}", h.name).as_str())
    }
}
fn owned_entry(entry: &Value, h: &Host) -> bool {
    if other_invocations(entry, h) {
        return false;
    }
    if legacy_owned_entry(entry, h) {
        return true;
    }
    let Some(executable) = entry_executable(entry, h) else {
        return false;
    };
    // Accept exactly what our writer emits, varying only the absolute path.
    // Re-rendering rejects shell suffixes, alternate quoting and edited fields.
    let Ok(previous) = native_entry_at(h, &executable) else {
        return false;
    };
    if flat_hook(h) {
        entry == &previous
    } else {
        entry == &previous["hooks"][0]
    }
}
fn entry_executable(entry: &Value, h: &Host) -> Option<PathBuf> {
    let executable = if h.name == "vibe" {
        let command = entry.get("command")?.as_str()?;
        if cfg!(windows) {
            command
                .strip_prefix('"')?
                .strip_suffix("\" hook vibe")?
                .to_owned()
        } else {
            command
                .strip_prefix('\'')?
                .strip_suffix("' hook vibe")?
                .replace("'\"'\"'", "'")
        }
    } else if h.name == "copilot" {
        let path = entry.get("exec").and_then(Value::as_str)?;
        path.to_owned()
    } else {
        let command = entry.get("command").and_then(Value::as_str)?;
        let prefix = if cfg!(windows) { "& '" } else { "'" };
        let suffix = format!("' hook {}", h.name);
        let escaped = command.strip_prefix(prefix)?.strip_suffix(&suffix)?;
        if cfg!(windows) {
            escaped.replace("''", "'")
        } else {
            escaped.replace("'\\''", "'")
        }
    };
    Some(PathBuf::from(executable))
}
fn executable_available(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}
/// Check effective event, matcher, generated entry and its recorded executable.
/// This deliberately does not claim that a host loaded or trusted the file.
fn native_status(v: &Value, h: &Host, path: &Path) -> Result<(Vec<&'static str>, Vec<String>)> {
    if h.output.as_deref() != Some(path) && h.name == "droid" {
        return Ok((vec![], vec![]));
    }
    let mut copy = v.clone();
    let Some(events) = hook_map(&mut copy, h, path)? else {
        return Ok((vec![], vec![]));
    };
    let names: &[&'static str] = if matches!(h.name, "copilot" | "vscode") {
        &["copilot", "vscode"]
    } else {
        &[h.name]
    };
    let mut active = vec![];
    let mut messages = vec![];
    for &name in names {
        let mut route = h.clone();
        route.name = name;
        let desired = native_entry(&route)?;
        for (event, entries) in events.iter() {
            let Some(entries) = entries.as_array() else {
                continue;
            };
            for group in entries {
                let candidates = if flat_hook(&route) {
                    std::slice::from_ref(group)
                } else {
                    group
                        .get("hooks")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                };
                for entry in candidates {
                    if !owned_entry(entry, &route) {
                        continue;
                    }
                    let correct = event == native_event(&route)
                        && (flat_hook(&route)
                            || (group.get("matcher") == desired.get("matcher")
                                && owned_group(Some(group), &route)));
                    let executable = entry_executable(entry, &route);
                    let available = executable.as_deref().is_some_and(executable_available);
                    if correct
                        && available
                        && prehook_unavailable(name).is_none()
                        && !active.contains(&name)
                    {
                        active.push(name);
                    }
                    messages.push(format!(
                        "{name}: {}: event/matcher {}; executable {}{}",
                        path.display(),
                        if correct { "valid" } else { "wrong" },
                        if available {
                            "available"
                        } else {
                            "missing, not executable, or unresolved PATH entry"
                        },
                        executable
                            .map(|p| format!(" ({})", p.display()))
                            .unwrap_or_default()
                    ));
                }
            }
        }
    }
    Ok((active, messages))
}
fn owned_group(group: Option<&Value>, h: &Host) -> bool {
    group.is_none_or(|g| {
        native_entry(h).is_ok_and(|desired| g.get("matcher") == desired.get("matcher"))
            && g.as_object()
                .is_some_and(|g| g.keys().all(|k| k == "matcher" || k == "hooks"))
    })
}
fn hook_map<'a>(
    v: &'a mut Value,
    h: &Host,
    path: &Path,
) -> Result<Option<&'a mut serde_json::Map<String, Value>>> {
    let standalone = h.name == "droid" && path.file_name().is_some_and(|f| f == "hooks.json");
    if standalone {
        return Ok(v.as_object_mut());
    }
    if let Some(hooks) = v.get("hooks") {
        ensure!(
            hooks.is_object(),
            "hooks must be an object: {}",
            path.display()
        );
    }
    Ok(v.get_mut("hooks").and_then(Value::as_object_mut))
}
/// Only hook event arrays are traversed; settings and permissions are never searched or changed.
fn remove_entries(
    v: &mut Value,
    h: &Host,
    path: &Path,
    rtk: bool,
    own: bool,
    stock: &[(&str, usize, &str)],
) -> Result<usize> {
    filter_hooks(v, h, path, |item, group| {
        let shared_owned = if own && matches!(h.name, "copilot" | "vscode") {
            let mut sibling = h.clone();
            sibling.name = if h.name == "copilot" {
                "vscode"
            } else {
                "copilot"
            };
            owned_entry(item, &sibling)
        } else {
            false
        };
        Ok((rtk && rtk_entry(item, h, stock)?)
            || (own
                && owned_entry(item, h)
                && (legacy_owned_entry(item, h) || owned_group(group, h)))
            || shared_owned)
    })
}
fn filter_hooks(
    v: &mut Value,
    h: &Host,
    path: &Path,
    matches: impl Fn(&Value, Option<&Value>) -> Result<bool>,
) -> Result<usize> {
    let Some(events) = hook_map(v, h, path)? else {
        return Ok(0);
    };
    let mut count = 0;
    for entries in events.values_mut() {
        let list = entries
            .as_array_mut()
            .context("hook event must be an array; settings unchanged")?;
        let mut kept = Vec::with_capacity(list.len());
        for mut entry in std::mem::take(list) {
            ensure!(
                entry.is_object(),
                "hook entries must be objects; settings unchanged"
            );
            let group = entry.clone();
            if let Some(nested) = entry.get_mut("hooks") {
                let nested = nested
                    .as_array_mut()
                    .context("nested hooks must be an array; settings unchanged")?;
                let before = nested.len();
                ensure!(
                    nested.iter().all(Value::is_object),
                    "nested hooks must be objects; settings unchanged"
                );
                for index in (0..nested.len()).rev() {
                    if matches(&nested[index], Some(&group))? {
                        nested.remove(index);
                    }
                }
                count += before - nested.len();
                // Remove only a group made empty by this operation, with ordinary matcher keys.
                if before > 0
                    && nested.is_empty()
                    && entry
                        .as_object()
                        .unwrap()
                        .keys()
                        .all(|k| k == "hooks" || k == "matcher")
                {
                    continue;
                }
            } else if matches(&entry, None)? {
                count += 1;
                continue;
            }
            kept.push(entry);
        }
        *list = kept;
    }
    Ok(count)
}
pub fn native_shell_command(path: &str, host: &str, windows: bool) -> String {
    if windows {
        format!("& '{}' hook {host}", path.replace('\'', "''"))
    } else {
        format!("'{}' hook {host}", path.replace('\'', "'\\''"))
    }
}
fn native_event(h: &Host) -> &'static str {
    match h.name {
        "claude" => "PostToolUse",
        "copilot" => "postToolUse",
        "cursor" => "preToolUse",
        "gemini" => "BeforeTool",
        _ => "PreToolUse",
    }
}
fn flat_hook(h: &Host) -> bool {
    matches!(h.name, "copilot" | "vscode" | "cursor")
}
fn native_entry(h: &Host) -> Result<Value> {
    native_entry_at(h, &h.executable)
}
fn native_entry_at(h: &Host, executable: &Path) -> Result<Value> {
    ensure!(
        executable.is_absolute(),
        "hook executable path must be absolute"
    );
    let path = executable
        .to_str()
        .context("hook executable path must be UTF-8")?;
    if h.name == "copilot" {
        return Ok(
            json!({"type":"command","matcher":"bash|powershell","exec":path,"args":["hook","copilot"],"timeoutSec":10}),
        );
    }
    let command = native_shell_command(path, h.name, cfg!(windows));
    if h.name == "cursor" {
        return Ok(json!({"command":command,"matcher":"Shell"}));
    }
    let mut hook = json!({"type":"command","command":command});
    if cfg!(windows) {
        hook["shell"] = json!("powershell");
    }
    if h.name == "vscode" {
        hook["timeout"] = json!(10);
        return Ok(hook);
    }
    let matcher = match h.name {
        "claude" => "Bash|PowerShell",
        "codex" => "Bash",
        "gemini" => "run_shell_command",
        "droid" => "Execute",
        _ => unreachable!(),
    };
    Ok(json!({"matcher":matcher, "hooks":[hook]}))
}
fn install_native(v: &mut Value, h: &Host) -> Result<()> {
    if matches!(h.name, "copilot" | "vscode") {
        // Keep the qualified CLI completion route. The VS Code adapter currently
        // passes through, so remove only our unedited obsolete registration.
        for name in ["copilot", "vscode"] {
            let mut route = h.clone();
            route.name = name;
            if prehook_unavailable(name).is_some() {
                filter_hooks(v, &route, h.output.as_ref().unwrap(), |item, group| {
                    Ok(owned_entry(item, &route) && owned_group(group, &route))
                })?;
            } else {
                install_native_route(v, &route)?;
            }
        }
        Ok(())
    } else {
        install_native_route(v, h)
    }
}
fn install_native_route(v: &mut Value, h: &Host) -> Result<()> {
    let desired = native_entry(h)?;
    let desired_hook = if flat_hook(h) {
        &desired
    } else {
        &desired["hooks"][0]
    };
    // Upgrade generated commands while preserving edited matcher groups.
    // Repeating this leaves an identical JSON value.
    filter_hooks(v, h, h.output.as_ref().unwrap(), |item, group| {
        Ok(owned_entry(item, h) && (legacy_owned_entry(item, h) || owned_group(group, h)))
    })?;
    let top = v.as_object_mut().unwrap();
    if matches!(h.name, "copilot" | "vscode" | "cursor") {
        ensure!(
            top.get("version").is_none_or(|v| v == &json!(1)),
            "unsupported hook version; unchanged"
        );
        top.insert("version".into(), json!(1));
    }
    let standalone = h.name == "droid"
        && h.output
            .as_ref()
            .unwrap()
            .file_name()
            .is_some_and(|f| f == "hooks.json");
    let hooks = if standalone {
        top
    } else {
        top.entry("hooks")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("hooks must be an object")?
    };
    let entries = hooks
        .entry(native_event(h))
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("hook event must be an array")?;
    let present = if flat_hook(h) {
        entries.contains(&desired)
    } else {
        entries.iter().any(|group| {
            group.get("matcher") == desired.get("matcher")
                && group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hooks| hooks.contains(desired_hook))
        })
    };
    if !present {
        entries.push(desired);
    }
    Ok(())
}

fn droid_output(h: &Host) -> Result<PathBuf> {
    let canonical = h.root.join("hooks.json");
    let legacy = h.root.join("hooks/hooks.json");
    let settings = h.root.join("settings.json");
    let live = if read(&canonical)?.is_some() {
        Some(canonical.clone())
    } else if read(&legacy)?.is_some() {
        Some(legacy)
    } else {
        None
    };
    if let Some(path) = &live
        && json_file(path, read(path)?.as_deref())?
            .get("PreToolUse")
            .is_some()
    {
        return Ok(path.clone());
    }
    if json_file(&settings, read(&settings)?.as_deref())?
        .get("hooks")
        .and_then(|v| v.get("PreToolUse"))
        .is_some()
    {
        return Ok(settings);
    }
    Ok(live.unwrap_or(canonical))
}

#[derive(Debug)]
pub struct Change {
    pub path: PathBuf,
    target: PathBuf,
    // Pin the link itself as well as its resolved contents. Deletion removes
    // this directory entry, leaving the external target available for recovery.
    link: Option<(PathBuf, PathBuf)>,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}
fn file_link(path: &Path) -> Result<Option<(PathBuf, PathBuf)>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => Ok(Some((
            config_target(path.parent().context("configuration path has no parent")?)?.join(
                path.file_name()
                    .context("configuration path has no filename")?,
            ),
            fs::read_link(path)?,
        ))),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
impl Change {
    fn check(&self) -> Result<()> {
        ensure!(
            config_target(&self.path)? == self.target
                && file_link(&self.path)? == self.link
                && read(&self.target)? == self.before,
            "configuration changed since planning: {}",
            self.path.display()
        );
        Ok(())
    }
    fn removed_link(&self) -> Option<&(PathBuf, PathBuf)> {
        self.link.as_ref().filter(|_| self.after.is_none())
    }
    fn rollback(&self) -> Result<()> {
        if let Some((path, destination)) = self.removed_link() {
            // Symlink creation is exclusive: a concurrent replacement is never overwritten.
            #[cfg(unix)]
            std::os::unix::fs::symlink(destination, path)?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(destination, path)?;
            Ok(())
        } else {
            replace(&self.target, self.after.as_deref(), self.before.as_deref())
        }
    }
}
#[derive(Default, Debug)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub messages: Vec<String>,
}
impl Plan {
    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        let target = config_target(path)?;
        let link = file_link(path)?;
        match self
            .changes
            .iter()
            .find(|c| c.target == target && (c.removed_link().is_none() || c.link == link))
        {
            Some(change) => Ok(change.after.clone()),
            None => read(path),
        }
    }
    fn change(
        &mut self,
        path: PathBuf,
        before: Option<Vec<u8>>,
        after: Option<Vec<u8>>,
    ) -> Result<()> {
        if before == after {
            return Ok(());
        }
        let target = config_target(&path)?;
        let link = file_link(&path)?;
        if let Some(old) = self
            .changes
            .iter_mut()
            .find(|c| c.target == target && (c.removed_link().is_none() || c.link == link))
        {
            if old.before == before && old.after == after {
                return Ok(());
            }
            ensure!(
                old.after == before,
                "conflicting changes to {}",
                path.display()
            );
            old.after = after;
        } else {
            self.changes.push(Change {
                path,
                target,
                link,
                before,
                after,
            });
        }
        Ok(())
    }
    /// Compare every original before committing, then each file again immediately before replacement.
    /// Rollback only files that still contain our own write; preserve concurrent user edits.
    pub fn apply(&self) -> Result<Vec<PathBuf>> {
        for c in &self.changes {
            c.check()?;
        }
        let mut applied: Vec<&Change> = vec![];
        let mut backups = vec![];
        for c in &self.changes {
            let result = (|| -> Result<()> {
                c.check()?;
                if let Some(before) = &c.before {
                    let (backup, mut file) = unique_file(&c.target, "backup")?;
                    file.write_all(before)?;
                    file.sync_all()?;
                    backups.push(backup);
                }
                if let Some((path, _)) = c.removed_link() {
                    c.check()?;
                    fs::remove_file(path)?;
                    Ok(())
                } else {
                    replace(&c.target, c.before.as_deref(), c.after.as_deref())
                }
            })();
            if let Err(error) = result {
                let mut failures = vec![];
                for done in applied.into_iter().rev() {
                    if let Err(e) = done.rollback() {
                        failures.push(format!("{}: {e}", done.path.display()));
                    }
                }
                bail!(
                    "{error:#}; earlier writes rolled back where unchanged; backups: {}; rollback issues: {}",
                    backups
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    failures.join(", ")
                );
            }
            applied.push(c);
        }
        Ok(backups)
    }
}
static SERIAL: AtomicU64 = AtomicU64::new(0);
fn unique_file(path: &Path, suffix: &str) -> Result<(PathBuf, fs::File)> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    for _ in 0..100 {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(
            ".retok-{suffix}-{stamp}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let candidate = PathBuf::from(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    bail!("could not allocate unique backup/temp file")
}
fn replace(path: &Path, expected: Option<&[u8]>, next: Option<&[u8]>) -> Result<()> {
    ensure!(
        read(path)?.as_deref() == expected,
        "concurrent modification: {}",
        path.display()
    );
    if let Some(bytes) = next {
        let parent = path.parent().context("configuration path has no parent")?;
        fs::create_dir_all(parent)?;
        check_config_target(path)?;
        let (temp, mut file) = unique_file(path, "tmp")?;
        let result = (|| -> Result<()> {
            file.write_all(bytes)?;
            if expected.is_some() {
                file.set_permissions(fs::metadata(path)?.permissions())?;
            }
            file.sync_all()?;
            drop(file);
            ensure!(
                read(path)?.as_deref() == expected,
                "concurrent modification: {}",
                path.display()
            );
            if expected.is_none() {
                fs::hard_link(&temp, path)?;
                fs::remove_file(&temp)?;
            } else {
                fs::rename(&temp, path)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    } else {
        if expected.is_some() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

fn instruction_template(h: &Host) -> String {
    if h.name == "cursor" {
        format!(
            "---\ndescription: Retok explicit output compaction\nalwaysApply: true\n---\n{INSTRUCTIONS}"
        )
    } else if h.name == "windsurf" && h.dedicated {
        format!("---\ntrigger: always_on\n---\n{INSTRUCTIONS}")
    } else {
        INSTRUCTIONS.into()
    }
}
fn instruction_text(h: &Host) -> Result<String> {
    ensure!(
        h.executable.is_absolute(),
        "instruction executable path must be absolute"
    );
    let path = h
        .executable
        .to_str()
        .context("instruction executable path must be UTF-8")?;
    Ok(instruction_template(h).replace("__RETOK_EXECUTABLE__", &serde_json::to_string(path)?))
}
fn instruction_range(text: &str, h: &Host) -> Option<std::ops::Range<usize>> {
    let template = instruction_template(h);
    let (prefix, suffix) = template.split_once("__RETOK_EXECUTABLE__")?;
    for (start, _) in text.match_indices(prefix) {
        let rest = &text[start + prefix.len()..];
        let mut strings = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let Some(Ok(path)) = strings.next() else {
            continue;
        };
        let length = strings.byte_offset();
        if !Path::new(&path).is_absolute()
            || !serde_json::to_string(&path).is_ok_and(|literal| literal == rest[..length])
            || !rest[length..].starts_with(suffix)
        {
            continue;
        }
        let end = start + prefix.len() + length + suffix.len();
        if !h.dedicated || (start == 0 && end == text.len()) {
            return Some(start..end);
        }
    }
    None
}
fn instruction_change(plan: &mut Plan, h: &Host, uninstall: bool) -> Result<bool> {
    let Some(path) = &h.instructions else {
        return Ok(false);
    };
    let before = plan.read(path)?;
    let text = before
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .context("instructions are not UTF-8; unchanged")?
        .unwrap_or("");
    let range = instruction_range(text, h);
    let present = range.is_some();
    let after = if uninstall {
        match range {
            Some(_) if h.dedicated => None,
            Some(range) => {
                Some(format!("{}{}", &text[..range.start], &text[range.end..]).into_bytes())
            }
            None => before.clone(),
        }
    } else {
        let managed = instruction_text(h)?;
        if let Some(range) = range {
            Some(format!("{}{}{}", &text[..range.start], managed, &text[range.end..]).into_bytes())
        } else if h.dedicated {
            ensure!(
                before.is_none(),
                "instruction file edited or unowned; unchanged: {}",
                path.display()
            );
            Some(managed.into_bytes())
        } else {
            ensure!(
                !text.contains("<!-- retok managed instructions"),
                "Retok instruction block was edited; unchanged: {}",
                path.display()
            );
            Some(
                format!(
                    "{text}{}{managed}",
                    if text.is_empty() || text.ends_with('\n') {
                        ""
                    } else {
                        "\n"
                    }
                )
                .into_bytes(),
            )
        }
    };
    plan.change(path.clone(), before, after)?;
    Ok(present)
}
fn vibe_entry(executable: &Path) -> Result<toml_edit::Table> {
    ensure!(
        executable.is_absolute(),
        "hook executable path must be absolute"
    );
    let path = executable
        .to_str()
        .context("hook executable path must be UTF-8")?;
    let mut table = toml_edit::Table::new();
    for (key, val) in [
        ("name", "retok-output"),
        ("type", "pre_tool"),
        ("match", "bash"),
    ] {
        table[key] = toml_edit::value(val);
    }
    // Vibe's loader rejects every backslash, including ordinary shell escapes.
    let command = if cfg!(windows) {
        ensure!(
            !path.contains(['%', '!', '"', '\r', '\n']),
            "Vibe command path contains unsupported cmd expansion characters"
        );
        format!("\"{}\" hook vibe", path.replace('\\', "/"))
    } else {
        ensure!(
            !path.contains('\\'),
            "Vibe does not support backslashes in hook executable paths"
        );
        format!("'{}' hook vibe", path.replace('\'', "'\"'\"'"))
    };
    table["command"] = toml_edit::value(command);
    table["timeout"] = toml_edit::value(10.0);
    table["strict"] = toml_edit::value(false);
    Ok(table)
}
fn same_toml_fields(a: &toml_edit::Table, b: &toml_edit::Table) -> bool {
    a.len() == b.len()
        && a.iter().all(|(k, v)| {
            b.get(k).is_some_and(|other| {
                // Value formatting/comments are user-owned; compare only typed values.
                match (v.as_value(), other.as_value()) {
                    (Some(a), Some(b)) => {
                        a.to_string().trim() == b.to_string().trim()
                            || a.as_str().zip(b.as_str()).is_some_and(|(a, b)| a == b)
                            || a.as_bool().zip(b.as_bool()).is_some_and(|(a, b)| a == b)
                            || a.as_float().zip(b.as_float()).is_some_and(|(a, b)| a == b)
                    }
                    _ => false,
                }
            })
        })
}
fn vibe_change(plan: &mut Plan, h: &Host, o: &Options) -> Result<()> {
    let path = h.root.join("hooks.toml");
    let before = plan.read(&path)?;
    let text = before
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .context("Vibe hooks are not UTF-8; unchanged")?
        .unwrap_or("");
    let bom = text.starts_with('\u{feff}');
    let mut doc = text
        .trim_start_matches('\u{feff}')
        .parse::<toml_edit::DocumentMut>()
        .context("invalid Vibe TOML; unchanged")?;
    let desired = vibe_entry(&h.executable)?;
    let mut remove = vec![];
    let mut configured = false;
    let mut present = false;
    let mut rtk = 0;
    if let Some(item) = doc.get("hooks") {
        let hooks = item
            .as_array_of_tables()
            .context("Vibe hooks must be an array of tables; unchanged")?;
        for (index, hook) in hooks.iter().enumerate() {
            let field = |key| hook.get(key).and_then(toml_edit::Item::as_str);
            if field("name") == Some("retok-output") {
                let entry = json!({"command":field("command")});
                let old = entry_executable(&entry, h);
                let owned = old
                    .as_deref()
                    .and_then(|p| vibe_entry(p).ok())
                    .is_some_and(|table| same_toml_fields(hook, &table));
                if owned {
                    present = same_toml_fields(hook, &desired);
                    configured |=
                        !cfg!(windows) && old.as_deref().is_some_and(executable_available);
                    if o.uninstall || o.instructions_only || !present {
                        remove.push(index);
                    }
                } else if !o.uninstall && !o.show {
                    bail!(
                        "edited or unowned Vibe Retok hook; unchanged: {}",
                        path.display()
                    );
                }
            }
            if field("command") == Some("rtk hook vibe") {
                rtk += 1;
                if o.replace {
                    ensure!(
                        field("type") == Some("pre_tool")
                            && field("match") == Some("bash")
                            && hook
                                .get("strict")
                                .is_none_or(|v| v.as_bool() == Some(false))
                            && hook.iter().all(|(k, _)| [
                                "name",
                                "type",
                                "match",
                                "command",
                                "timeout",
                                "strict",
                                "description"
                            ]
                            .contains(&k)),
                        "custom RTK Vibe hook; manual migration required; unchanged"
                    );
                    remove.push(index);
                }
            }
        }
    }
    if o.replace && o.agent.is_none() && rtk == 0 {
        return Ok(());
    }
    if o.project {
        plan.messages.push(
            "vibe: project hooks require an already trusted folder; trust is unchanged".into(),
        );
    }
    plan.messages.push(format!(
        "vibe: {}; {}; recognized RTK entries: {rtk}; host loading/trust not probed",
        if o.instructions_only {
            "explicit instruction-only downgrade"
        } else {
            "native pre-execution hook"
        },
        if configured {
            "configured"
        } else {
            "not fully configured (registration or executable unavailable)"
        }
    ));
    if o.show {
        return Ok(());
    }
    if !remove.is_empty() || (!present && !o.uninstall && !o.instructions_only) {
        if doc.get("hooks").is_none() {
            doc["hooks"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
        }
        let hooks = doc["hooks"].as_array_of_tables_mut().unwrap();
        for index in remove.into_iter().rev() {
            hooks.remove(index);
        }
        if !o.uninstall && !o.instructions_only && !present {
            hooks.push(desired);
        }
        let after = format!("{}{}", if bom { "\u{feff}" } else { "" }, doc);
        plan.change(path, before, Some(after.into_bytes()))?;
    }
    instruction_change(plan, h, o.uninstall)?;
    Ok(())
}

fn native_plugin_files(h: &Host) -> &'static [(&'static str, &'static str)] {
    match h.name {
        "hermes" => &[
            (
                "__init__.py",
                include_str!("../integrations/hermes/__init__.py"),
            ),
            (
                "plugin.yaml",
                include_str!("../integrations/hermes/plugin.yaml"),
            ),
        ],
        "openclaw" => &[
            (
                "index.mjs",
                include_str!("../integrations/openclaw/index.mjs"),
            ),
            (
                "openclaw.plugin.json",
                include_str!("../integrations/openclaw/openclaw.plugin.json"),
            ),
            (
                "package.json",
                include_str!("../integrations/openclaw/package.json"),
            ),
        ],
        _ => unreachable!(),
    }
}
fn native_plugin_dir(h: &Host, name: &str) -> PathBuf {
    h.root
        .join(if h.name == "hermes" {
            "plugins"
        } else {
            "extensions"
        })
        .join(name)
}
fn native_plugin_literal(path: &Path, h: &Host) -> Result<String> {
    ensure!(path.is_absolute(), "plugin executable must be absolute");
    let text = path.to_str().context("plugin executable must be UTF-8")?;
    if h.name == "hermes" {
        Ok(text.as_bytes().iter().map(|b| format!("{b:02x}")).collect())
    } else {
        Ok(serde_json::to_string(text)?)
    }
}
fn native_plugin_marker(h: &Host) -> &'static str {
    if h.name == "hermes" {
        "__RETOK_EXECUTABLE_UTF8_HEX__"
    } else {
        "__RETOK_EXECUTABLE_JSON__"
    }
}
fn native_plugin_executable(bytes: &[u8], template: &str, h: &Host) -> Option<PathBuf> {
    let (prefix, suffix) = template.split_once(native_plugin_marker(h))?;
    let text = std::str::from_utf8(bytes).ok()?;
    let literal = text.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let path = if h.name == "hermes" {
        if !literal.len().is_multiple_of(2) || !literal.is_ascii() {
            return None;
        }
        let bytes = (0..literal.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&literal[i..i + 2], 16).ok())
            .collect::<Option<Vec<_>>>()?;
        String::from_utf8(bytes).ok()?
    } else {
        serde_json::from_str::<String>(literal).ok()?
    };
    let path = PathBuf::from(path);
    native_plugin_literal(&path, h)
        .is_ok_and(|expected| expected == literal)
        .then_some(path)
}

fn native_plugin_bundle(plan: &mut Plan, h: &Host, o: &Options) -> Result<bool> {
    let dir = native_plugin_dir(h, "retok-rewrite");
    let mut files = vec![];
    let mut configured = h.name == "hermes" || !cfg!(windows);
    let mut edited = false;
    for &(name, template) in native_plugin_files(h) {
        let path = dir.join(name);
        let before = plan.read(&path)?;
        let code = template.contains(native_plugin_marker(h));
        let executable = before
            .as_deref()
            .and_then(|bytes| native_plugin_executable(bytes, template, h));
        let owned = before.as_deref().is_some_and(|bytes| {
            if code {
                executable.is_some()
            } else {
                bytes == template.as_bytes()
            }
        });
        configured &= owned && (!code || executable.as_deref().is_some_and(executable_available));
        edited |= before.is_some() && !owned;
        files.push((path, before, template, owned));
    }
    plan.messages.push(format!(
        "{}: native {} plugin; {}; host loading, trust and execution policy not probed",
        h.name,
        if h.name == "hermes" {
            "post-output"
        } else {
            "pre-execution"
        },
        if configured {
            "plugin files and executable verified"
        } else {
            "not fully configured (files/executable/platform unavailable or edited)"
        }
    ));
    if h.name == "hermes" {
        plan.messages.push("hermes: requires host transform_tool_result capability; host version and capability are not probed".into());
    }
    if o.show {
        return Ok(configured);
    }
    if edited {
        if o.uninstall {
            plan.messages.push(format!(
                "{}: edited or unowned plugin bundle and activation preserved",
                h.name
            ));
            return Ok(false);
        }
        bail!(
            "{}: edited or unowned plugin bundle; no files changed",
            dir.display()
        );
    }
    for (path, before, template, owned) in files {
        if o.uninstall {
            if owned {
                plan.change(path, before, None)?;
            }
        } else {
            let text = template.replace(
                native_plugin_marker(h),
                &native_plugin_literal(&h.executable, h)?,
            );
            plan.change(path, before, Some(text.into_bytes()))?;
        }
    }
    Ok(true)
}
fn known_rtk_native_plugin(h: &Host, stock: &[(&str, usize, &str)]) -> Result<bool> {
    let dir = native_plugin_dir(h, "rtk-rewrite");
    let names: &[&str] = if h.name == "hermes" {
        &["__init__.py", "plugin.yaml"]
    } else {
        &["index.ts", "openclaw.plugin.json", "package.json"]
    };
    for name in names {
        let Some(bytes) = read(&dir.join(name))? else {
            return Ok(false);
        };
        if !stock_match(&format!("{}-{name}", h.name), &bytes, stock) {
            return Ok(false);
        }
    }
    for entry in fs::read_dir(&dir)? {
        let name = entry?.file_name();
        if !names.iter().any(|expected| name == *expected)
            && name != "__pycache__"
            && name != "README.md"
            && name != "LICENSE"
        {
            return Ok(false);
        }
    }
    Ok(true)
}
fn native_plugin_change(
    plan: &mut Plan,
    h: &Host,
    roots: &Roots,
    o: &Options,
    stock: &[(&str, usize, &str)],
) -> Result<()> {
    let rtk_present = native_plugin_dir(h, "rtk-rewrite").exists();
    if rtk_present && o.replace && !o.uninstall && !o.show && !known_rtk_native_plugin(h, stock)? {
        plan.messages.push(format!(
            "{}: RTK plugin and configuration preserved; modified or unknown plugin; manual migration required",
            h.name
        ));
        return Ok(());
    }
    if rtk_present && o.show {
        plan.messages.push(format!(
            "{}: RTK plugin remains on disk; activation is reported separately",
            h.name
        ));
    }
    if o.replace && o.agent.is_none() && !rtk_present {
        return Ok(());
    }
    if o.replace
        && rtk_present
        && ((cfg!(windows) && h.name != "hermes") || !executable_available(&h.executable))
    {
        plan.messages.push(format!(
            "{}: replacement executable/platform unavailable; RTK activation preserved",
            h.name
        ));
        return Ok(());
    }
    if h.name == "hermes" {
        hermes_plugin_change(plan, h, o, rtk_present)
    } else {
        openclaw_plugin_change(plan, h, roots, o, rtk_present)
    }
}
fn hermes_document(text: &str) -> Result<(yaml_edit::YamlFile, yaml_edit::Document)> {
    let mut file: yaml_edit::YamlFile = text.parse().context("invalid Hermes YAML; unchanged")?;
    if file.documents().next().is_none() {
        file = format!("{text}\n{{}}\n").parse()?;
    }
    let mut documents = file.documents();
    let document = documents.next().context("missing Hermes document")?;
    ensure!(
        documents.next().is_none(),
        "Hermes config must contain one YAML document; unchanged"
    );
    Ok((file, document))
}
// Normalize host-compatible booleans only in a separate semantic view. Actual
// writes retain the original tokens, including quoted strings, tags and numbers.
fn hermes_yaml(text: &str) -> Result<serde_yaml_ng::Value> {
    use yaml_edit::{AsYaml, Document, YamlNode};
    let (shadow, document) = hermes_document(text)?;
    let scalars = document
        .as_node()
        .context("missing Hermes document")?
        .descendants()
        .filter_map(YamlNode::from_syntax)
        .filter_map(|node| node.as_scalar().cloned())
        .collect::<Vec<_>>();
    for scalar in scalars {
        let tag = scalar
            .as_node()
            .and_then(|node| node.parent())
            .and_then(YamlNode::from_syntax)
            .and_then(|node| node.as_tagged().and_then(|tag| tag.tag()));
        let boolean = match tag.as_deref() {
            None => scalar.as_bool(),
            Some("!!bool" | "!<tag:yaml.org,2002:bool>") => scalar
                .as_string()
                .parse::<Document>()?
                .as_scalar()
                .and_then(|value| value.as_bool()),
            _ => None,
        };
        if let Some(boolean) = boolean {
            scalar.set_value(if boolean { "true" } else { "false" });
        }
    }
    let mut value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&shadow.to_string()).context("invalid Hermes YAML; unchanged")?;
    value
        .apply_merge()
        .context("invalid Hermes YAML merge; unchanged")?;
    Ok(value)
}
fn hermes_plugin_change(plan: &mut Plan, h: &Host, o: &Options, rtk_present: bool) -> Result<()> {
    use serde_yaml_ng::Value as Yaml;
    use yaml_edit::anchor_resolution::DocumentMergedExt;
    use yaml_edit::{AsYaml, SyntaxKind};
    let path = h.root.join("config.yaml");
    let before = plan.read(&path)?;
    let text = std::str::from_utf8(before.as_deref().unwrap_or(b"{}\n"))?;
    let mut config = hermes_yaml(text)?;
    let null_config = config.is_null();
    if null_config {
        config = Yaml::Mapping(Default::default());
    }
    ensure!(
        config.is_mapping(),
        "Hermes config must be a mapping; unchanged"
    );
    ensure!(
        config["plugins"].is_null() || config["plugins"].is_mapping(),
        "Hermes plugins must be a mapping; unchanged"
    );
    for key in ["enabled", "disabled"] {
        ensure!(
            config["plugins"][key].is_null()
                || config["plugins"][key]
                    .as_sequence()
                    .is_some_and(|items| items.iter().all(Yaml::is_string)),
            "Hermes plugins.{key} must be a list of names; unchanged"
        );
    }
    for field in ["settings", "config"] {
        ensure!(
            !matches!(
                config["plugins"]["entries"]["retok-rewrite"][field]["enabled"],
                Yaml::Tagged(_)
            ),
            "Hermes plugin enabled tag cannot be resolved; unchanged"
        );
    }
    let contains = |key: &str, name: &str| {
        config["plugins"][key]
            .as_sequence()
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(name)))
    };
    let rtk_enabled = contains("enabled", "rtk-rewrite") && !contains("disabled", "rtk-rewrite");
    if rtk_present && rtk_enabled && !o.replace && !o.uninstall && !o.show {
        plan.messages.push("hermes: active RTK plugin preserved; select --replace-rtk to migrate a recognized stock plugin".into());
        return Ok(());
    }
    let denied = contains("disabled", "retok-rewrite")
        || config["plugins"]["entries"]["retok-rewrite"]["settings"]["enabled"].as_bool()
            == Some(false)
        || config["plugins"]["entries"]["retok-rewrite"]["config"]["enabled"].as_bool()
            == Some(false);
    let enabled = contains("enabled", "retok-rewrite");
    plan.messages.push(format!("hermes: host activation {}; project plugins require a separate host opt-in which setup does not change", if denied { "explicitly disabled; preserved" } else if enabled { "enabled" } else { "not enabled" }));
    if denied && !o.uninstall {
        return Ok(());
    }
    if !native_plugin_bundle(plan, h, o)? || o.show {
        return Ok(());
    }
    let original = config.clone();
    let mut enabled = config["plugins"]["enabled"]
        .as_sequence()
        .cloned()
        .unwrap_or_default();
    if o.uninstall {
        enabled.retain(|v| v.as_str() != Some("retok-rewrite"));
    } else {
        if !enabled.iter().any(|v| v.as_str() == Some("retok-rewrite")) {
            enabled.push(Yaml::String("retok-rewrite".into()));
        }
        if o.replace && rtk_present {
            enabled.retain(|v| v.as_str() != Some("rtk-rewrite"));
        }
    }
    if !o.uninstall || config["plugins"]["enabled"].is_sequence() {
        config["plugins"]["enabled"] = Yaml::Sequence(enabled);
    }
    if config != original {
        let (mut file, mut document) = hermes_document(text)?;
        if null_config && let Some(scalar) = document.as_scalar() {
            scalar.set_value("{}");
            (file, document) = hermes_document(&file.to_string())?;
        }
        let root = document
            .as_mapping()
            .context("Hermes config must be a mapping; unchanged")?;
        if root.get_mapping("plugins").is_none() {
            let inherited = document.merged().and_then(|view| {
                view.as_mapping()
                    .get_merged("plugins")
                    .map(|plugins| plugins.base().clone())
            });
            let empty: yaml_edit::Document = "{}".parse()?;
            root.set(
                "plugins",
                inherited.unwrap_or_else(|| empty.as_mapping().unwrap()),
            );
            // Materialize an inherited mapping without redeclaring its anchors.
            // References still resolve to the unchanged original declarations.
            let copied = root.get_mapping("plugins").unwrap();
            let anchors = copied
                .as_node()
                .unwrap()
                .parent()
                .unwrap()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| token.kind() == SyntaxKind::ANCHOR)
                .collect::<Vec<_>>();
            for anchor in anchors {
                anchor.detach();
            }
        }
        let plugins = root
            .get_mapping("plugins")
            .context("Hermes plugins must be a mapping; unchanged")?;
        let names = config["plugins"]["enabled"]
            .as_sequence()
            .context("missing Hermes enabled list")?
            .iter()
            .map(|name| name.as_str().context("Hermes plugin name must be a string"))
            .collect::<Result<Vec<_>>>()?;
        // Flow sequences work in both block and flow mappings. Only these
        // owned plugin names are serialized; all other scalar styles survive.
        let list: yaml_edit::Document = serde_json::to_string(&names)?.parse()?;
        plugins.set(
            "enabled",
            list.as_sequence().context("missing Hermes enabled list")?,
        );
        let after = file.to_string();
        ensure!(
            hermes_yaml(&after)? == config,
            "Hermes YAML edit changed unrelated settings; unchanged"
        );
        plan.messages
            .push("hermes: config.yaml activation list updated; exact original backed up".into());
        plan.change(path, before, Some(after.into_bytes()))?;
    }
    instruction_change(plan, h, o.uninstall)?;
    Ok(())
}

fn openclaw_plugin_change(
    plan: &mut Plan,
    h: &Host,
    roots: &Roots,
    o: &Options,
    rtk_present: bool,
) -> Result<()> {
    let path = roots
        .openclaw_config
        .clone()
        .unwrap_or_else(|| h.root.join("openclaw.json"));
    let before = plan.read(&path)?;
    let mut config = match &before {
        // Strict JSON keeps its duplicate-key/number-lexeme guarantees. The
        // native host also accepts JSON5; normalize that syntax on changes.
        Some(bytes)
            if serde_json::from_slice::<Box<serde_json::value::RawValue>>(
                bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes),
            )
            .is_ok() =>
        {
            json_file(&path, Some(bytes))?
        }
        Some(bytes) => {
            // Use the ordinary mapping visitor, not Value's special arbitrary-
            // precision number key. This also rejects duplicate JSON5 keys.
            let parsed: serde_yaml_ng::Value =
                json5::from_str(std::str::from_utf8(bytes)?.trim_start_matches('\u{feff}'))
                    .context("invalid OpenClaw JSON5; unchanged")?;
            let mut pending = vec![&parsed];
            while let Some(value) = pending.pop() {
                match value {
                    serde_yaml_ng::Value::Number(n) => ensure!(
                        n.as_f64().is_some_and(f64::is_finite),
                        "non-finite OpenClaw JSON5 number cannot be preserved in JSON; unchanged"
                    ),
                    serde_yaml_ng::Value::Sequence(values) => pending.extend(values),
                    serde_yaml_ng::Value::Mapping(values) => pending.extend(values.values()),
                    _ => (),
                }
            }
            serde_json::to_value(parsed)?
        }
        None => json!({}),
    };
    ensure!(
        config.is_object(),
        "OpenClaw config must be an object; unchanged"
    );
    if config.get("plugins").is_none() {
        config["plugins"] = json!({});
    }
    ensure!(
        config["plugins"].is_object(),
        "OpenClaw plugins must be an object; unchanged"
    );
    for key in ["allow", "deny"] {
        ensure!(
            config["plugins"].get(key).is_none_or(|v| v
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string))),
            "OpenClaw plugins.{key} must be a list of names; unchanged"
        );
    }
    for (value, label) in [
        (&config["plugins"]["entries"], "plugins.entries"),
        (
            &config["plugins"]["entries"]["retok-rewrite"],
            "Retok entry",
        ),
        (
            &config["plugins"]["entries"]["retok-rewrite"]["config"],
            "Retok config",
        ),
    ] {
        ensure!(
            value.is_null() || value.is_object(),
            "OpenClaw {label} must be an object; unchanged"
        );
    }
    for value in [
        &config["plugins"]["enabled"],
        &config["plugins"]["entries"]["retok-rewrite"]["enabled"],
        &config["plugins"]["entries"]["retok-rewrite"]["config"]["enabled"],
    ] {
        ensure!(
            value.is_null() || value.is_boolean(),
            "OpenClaw enabled flags must be booleans; unchanged"
        );
    }
    let rtk_enabled = config["plugins"]["entries"]["rtk-rewrite"]["enabled"] != false
        && !config["plugins"]["deny"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v == "rtk-rewrite"))
        && config["plugins"]["allow"]
            .as_array()
            .is_none_or(|v| v.is_empty() || v.iter().any(|v| v == "rtk-rewrite"));
    if rtk_present && rtk_enabled && !o.replace && !o.uninstall && !o.show {
        plan.messages.push("openclaw: active RTK plugin preserved; select --replace-rtk to migrate a recognized stock plugin".into());
        return Ok(());
    }
    let denied = config["plugins"]["enabled"] == false
        || config["plugins"]["deny"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v == "retok-rewrite"))
        || config["plugins"]["entries"]["retok-rewrite"]["enabled"] == false
        || config["plugins"]["entries"]["retok-rewrite"]["config"]["enabled"] == false;
    let excluded = config["plugins"]["allow"]
        .as_array()
        .is_some_and(|v| !v.is_empty() && !v.iter().any(|v| v == "retok-rewrite"));
    if (denied || excluded && o.agent.as_deref() != Some("openclaw")) && !o.uninstall {
        plan.messages.push(format!(
            "openclaw: host activation blocked; {}; existing plugins/configuration preserved",
            if denied {
                "explicit disable/deny is unchanged"
            } else {
                "select --agent openclaw to add only retok-rewrite to the existing allowlist"
            }
        ));
        return Ok(());
    }
    plan.messages.push(format!(
        "openclaw: host activation {}",
        if config["plugins"]["entries"]["retok-rewrite"]["enabled"] == true && !denied && !excluded
        {
            "enabled"
        } else {
            "not enabled"
        }
    ));
    if !native_plugin_bundle(plan, h, o)? || o.show {
        return Ok(());
    }
    let original = config.clone();
    let plugins = config["plugins"].as_object_mut().unwrap();
    if o.uninstall {
        if let Some(allow) = plugins.get_mut("allow").and_then(Value::as_array_mut) {
            // Empty means unrestricted in OpenClaw. Keep a singleton rather
            // than authorizing unrelated plugins during uninstall.
            if allow.iter().any(|v| v != "retok-rewrite") {
                allow.retain(|v| v != "retok-rewrite");
            } else if !allow.is_empty() {
                plan.messages.push("openclaw: singleton plugin allowlist retained to avoid enabling unrelated plugins".into());
            }
        }
        if let Some(entries) = plugins.get_mut("entries").and_then(Value::as_object_mut) {
            if entries.get("retok-rewrite")
                == Some(&json!({"enabled":true,"config":{"enabled":true}}))
            {
                entries.remove("retok-rewrite");
            } else if entries.contains_key("retok-rewrite") {
                plan.messages
                    .push("openclaw: customized plugin entry preserved".into());
            }
        }
    } else {
        if excluded {
            plugins
                .get_mut("allow")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .push(json!("retok-rewrite"));
            plan.messages
                .push("openclaw: add only retok-rewrite to the existing plugin allowlist".into());
        }
        let entries = plugins
            .entry("entries")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("OpenClaw plugins.entries must be an object; unchanged")?;
        let entry = entries
            .entry("retok-rewrite")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("OpenClaw plugin entry must be an object; unchanged")?;
        entry.insert("enabled".into(), json!(true));
        entry
            .entry("config")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("OpenClaw plugin config must be an object; unchanged")?
            .insert("enabled".into(), json!(true));
        if o.replace && rtk_present {
            entries
                .entry("rtk-rewrite")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .context("OpenClaw RTK entry must be an object; unchanged")?
                .insert("enabled".into(), json!(false));
        }
    }
    if config != original {
        plan.messages.push(
            "openclaw: config formatting/comments normalized to JSON; exact original backed up"
                .into(),
        );
        let mut after = serde_json::to_vec_pretty(&config)?;
        after.push(b'\n');
        plan.change(path, before, Some(after))?;
    }
    Ok(())
}

fn settings_paths(h: &Host) -> Result<Vec<PathBuf>> {
    let mut paths = h.settings.clone();
    if h.name == "copilot" || h.name == "vscode" {
        let dir = h.root.join("hooks");
        check_config_target(&dir)?;
        match fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry?.path();
                    if path.extension().is_some_and(|x| x == "json") {
                        paths.push(path);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(out) = &h.output {
        paths.push(out.clone());
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn plugin_template(h: &Host) -> Result<String> {
    let source = if h.name == "omp" { "pi" } else { h.name };
    let runtime = include_str!("../integrations/runtime.js")
        .replace("__RETOK_SOURCE__", &serde_json::to_string(source)?);
    let adapter = match source {
        "pi" => include_str!("../integrations/pi.js"),
        "opencode" => include_str!("../integrations/opencode.js"),
        "kilo" => include_str!("../integrations/kilo.js"),
        _ => unreachable!(),
    };
    Ok(format!(
        "// retok managed plugin v1; edits prevent automatic replacement/removal\n{runtime}\n{adapter}"
    ))
}
fn plugin_text(h: &Host, roots: &Roots) -> Result<String> {
    ensure!(
        roots.executable.is_absolute(),
        "plugin executable path must be absolute"
    );
    let executable = roots
        .executable
        .to_str()
        .context("plugin executable path must be UTF-8")?;
    Ok(plugin_template(h)?.replace("__RETOK_EXECUTABLE__", &serde_json::to_string(executable)?))
}
fn owned_plugin(bytes: &[u8], h: &Host) -> bool {
    plugin_executable(bytes, h).is_some()
}
fn plugin_executable(bytes: &[u8], h: &Host) -> Option<PathBuf> {
    let text = std::str::from_utf8(bytes).ok()?;
    let template = plugin_template(h).ok()?;
    let (prefix, suffix) = template.split_once("__RETOK_EXECUTABLE__")?;
    let literal = text.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let executable: String = serde_json::from_str(literal).ok()?;
    (Path::new(&executable).is_absolute()
        && serde_json::to_string(&executable).is_ok_and(|canonical| canonical == literal))
    .then(|| PathBuf::from(executable))
}

const HELP: &str = "Usage: retok init [--agent HOST | --all] [--global | -g | --project]
                  [--replace-rtk] [--instructions-only] [--dry-run] [--show | --uninstall]

Default: global scope, detected existing agent homes only. --project stays local.
--agent HOST selects one host explicitly; --all selects detected hosts.
--replace-rtk selects recognizable RTK integrations and migrates supported stock
files. Modified or unrecognized RTK plugins/blocks require manual migration.
--dry-run previews writes and removals; --show reports status without writes.
--uninstall removes unchanged Retok-owned files/blocks and exact hook entries.
--help, -h shows this help without reading agent configuration.

Native post-output integration: claude (>=2.1.121), copilot CLI, pi, omp,
opencode, kilo (current generation; kilocode alias).
Native pre-execution hooks: codex and vibe on POSIX.
Vibe uses hooks.toml (project hooks require an already trusted folder).
Cursor, Gemini, Droid and VS Code native rewrite is unavailable: host policy or
schema is not qualified. Fresh setup uses guidance where supported; shared
Copilot setup installs only the CLI completion route. Existing RTK automation
is preserved, including shared Copilot, unless an explicit instruction-only
downgrade is selected.
Host version, discovery, trust and runtime loading are not probed by setup.
Global Hermes uses native post-output compaction after execution and redaction;
requires Hermes >=2026.9.14 with transform_tool_result (host version is not probed).
Global OpenClaw uses a native pre-execution plugin; explicit disables/denies remain.
Pre-execution adapters are POSIX-only; Windows passes through and retains RTK
automation unless an explicit instruction-only downgrade is selected.
Instruction fallback: roo, kimi; project windsurf, antigravity, cline, hermes, openclaw. --instructions-only --agent HOST explicitly selects guidance
instead of an automatic hook where a guidance target exists.
Claude honors CLAUDE_CONFIG_DIR; Pi/OMP honor PI_CODING_AGENT_DIR.
Droid uses FACTORY_HOME_OVERRIDE/.factory; Kimi uses KIMI_CODE_HOME/AGENTS.md;
Hermes uses HERMES_HOME; OpenClaw uses OPENCLAW_STATE_DIR and OPENCLAW_CONFIG_PATH.
Stock RTK plugin migration retains plugin files and changes activation only.
Stock RTK migration is pinned to the version recorded in the setup source;
other revisions and relocated plugin paths are not assumed equivalent.";

// SHA-256 and exact byte lengths from public RTK commit
// https://github.com/rtk-ai/rtk/tree/5e0f92cd108fc5a985022c5eb525a401f97fa11f
// Plugin files: hooks/{pi,opencode}/rtk.ts (OMP installs Pi verbatim).
// Scripts: hooks/{claude,cursor}/rtk-rewrite.sh and init.rs GEMINI_HOOK_SCRIPT.
// Awareness: hooks/rtk-awareness{,-high,-full}.md.
// Blocks: src/hooks/init.rs RTK_INSTRUCTIONS, COPILOT_INSTRUCTIONS and
// rtk_block(awareness). Only the final LF is excluded from block hashes because
// RTK's upsert trims it when appending to existing shared instructions.
// No templates or executable RTK code are embedded here.
const STOCK: &[(&str, usize, &str)] = &[
    (
        "hermes-__init__.py",
        2358,
        "1c211b6248d9277fed7d615faa7287db5174462c92efcecff3ed1165af81d5bb",
    ),
    (
        "hermes-plugin.yaml",
        195,
        "2f285a22ad9958ef0084c75a1b42ea350139bf35a7c3835626d93abac84c601a",
    ),
    (
        "openclaw-index.ts",
        4741,
        "92861d1b336b4649192aa32b704fd588b380e0735e46aac19ab4cfd3d773444d",
    ),
    (
        "openclaw-openclaw.plugin.json",
        838,
        "2203e8f992a30cdec6192e17ea48fdf82d793cd482097d764292444c3d5618c5",
    ),
    (
        "openclaw-package.json",
        600,
        "7873789835257f7c005ec7963923ab1584eb2f30e5dd67de480ec1ec23212141",
    ),
    (
        "script-claude",
        3337,
        "742418d70728fc3b24032fb8ae2c39a13169c2b93218beda23bfaf993d67bfe5",
    ),
    (
        "script-cursor",
        2119,
        "ac3d580f2886165bdf9459bf24ea26cda32bf35124b095f48249e89a0ed279c0",
    ),
    (
        "script-gemini",
        33,
        "e401666129b4fce7d2abb287944dd0b14b4559597c8cceffc007de73487bdad0",
    ),
    (
        "pi",
        4835,
        "d1555e0af5872a30ed04059423350bdb38302f200a9642c5a9bcc519c95a2257",
    ),
    (
        "opencode",
        1339,
        "6530c131946c84892f9522abd68d4e513e1e658d8ddbad1f59388c86ebbcb6bb",
    ),
    (
        "awareness",
        452,
        "dc37dc6afdf513200c2aae1931496e433d323f313877a49b0d5ba11992c33ac7",
    ),
    (
        "awareness",
        978,
        "d124b2926b0cd506680f785ab98ef64c211854549da4a85ec544fb803aaea812",
    ),
    (
        "awareness",
        1121,
        "278274ef3d08c858d4247cc91419c4d74ef922b95719e987b22e896aef10e1fc",
    ),
    (
        "block",
        5139,
        "c126012cc7e7308759d0c7cfbcc5f77c5dc69de264f0d72a5894f085ce14ed6e",
    ),
    (
        "block",
        751,
        "c4a719c5a9185e6cc9c96f7a185ce4e235671f8805787981f945e25770c110df",
    ),
    (
        "block",
        507,
        "2ccafee3bca6c4b61e326eecd43a59ae3fed9847ef065fce83cf6b87f834ef2f",
    ),
    (
        "block",
        1033,
        "75b3cd907056bf338ea4137000052526fe4e5a26b6651f8254cbd4126acfaa99",
    ),
    (
        "block",
        1176,
        "ce382cb06c1d302c6b9c8cd8d7c71b3153c94b0a16ad211bc9d7685dab953ad7",
    ),
];
fn stock_match(kind: &str, bytes: &[u8], stock: &[(&str, usize, &str)]) -> bool {
    let digest = format!("{:x}", Sha256::digest(bytes));
    stock
        .iter()
        .any(|(k, len, hash)| *k == kind && *len == bytes.len() && *hash == digest)
}
fn clean_rtk_guidance(
    text: &str,
    refs: &[String],
    stock: &[(&str, usize, &str)],
) -> Result<String> {
    const START: &str = "<!-- rtk-instructions";
    const END: &str = "<!-- /rtk-instructions -->";
    let mut rest = text;
    let mut output = String::new();
    while let Some(start) = rest.find(START) {
        ensure!(
            start == 0 || rest.as_bytes()[start - 1] == b'\n',
            "RTK marker is not a standalone block; manual migration required"
        );
        let tail = &rest[start..];
        let end = tail
            .find(END)
            .context("incomplete RTK instruction block; manual migration required")?
            + END.len();
        ensure!(
            stock_match("block", &tail.as_bytes()[..end], stock),
            "modified or unknown RTK instruction block; manual migration required"
        );
        output.push_str(&rest[..start]);
        rest = &tail[end..];
        if let Some(without_lf) = rest.strip_prefix('\n') {
            rest = without_lf;
        }
    }
    ensure!(
        !rest.contains(END),
        "unmatched RTK instruction marker; manual migration required"
    );
    output.push_str(rest);
    // The stock rules-only installers append an unmarked awareness file verbatim.
    // Match only a complete, exact suffix at a line boundary, preserving the prefix.
    for &(kind, len, _) in stock {
        if kind != "awareness" || len > output.len() {
            continue;
        }
        let start = output.len() - len;
        if (start == 0 || output.as_bytes()[start - 1] == b'\n')
            && stock_match("awareness", &output.as_bytes()[start..], stock)
        {
            output.truncate(start);
            break;
        }
    }
    Ok(output
        .split_inclusive('\n')
        .filter(|line| {
            let bare = line.strip_suffix('\n').unwrap_or(line);
            let bare = bare.strip_suffix('\r').unwrap_or(bare);
            !refs.iter().any(|reference| reference == bare)
        })
        .collect())
}

fn rtk_migration(
    h: &Host,
    roots: &Roots,
    project: bool,
    stock: &[(&str, usize, &str)],
    pending: &Plan,
) -> Result<Plan> {
    let mut migration = Plan::default();
    if let Some(plugin) = &h.plugin {
        let path = plugin.with_file_name("rtk.ts");
        if let Some(bytes) = pending.read(&path)? {
            let kind = if h.name == "omp" { "pi" } else { h.name };
            ensure!(
                stock_match(kind, &bytes, stock),
                "{}: modified or unknown RTK plugin; manual migration required; no files changed",
                path.display()
            );
            migration.change(path, Some(bytes), None)?;
        }
    }
    let mut shared = vec![];
    match h.name {
        "claude" => shared.push(if project {
            roots.project.join("CLAUDE.md")
        } else {
            h.root.join("CLAUDE.md")
        }),
        "codex" | "gemini" | "droid" | "kimi" | "hermes" | "vibe" => {
            shared.extend(h.instructions.iter().cloned())
        }
        "copilot" | "vscode" => shared.push(h.root.join("copilot-instructions.md")),
        "cline" if project && h.root.is_file() => shared.push(h.root.clone()),
        "windsurf" if project => shared.push(roots.project.join(".windsurfrules")),
        "antigravity" if project => shared.push(h.root.join("rules/antigravity-rtk-rules.md")),
        _ => (),
    }
    for path in shared {
        let Some(before) = pending.read(&path)? else {
            continue;
        };
        let text =
            std::str::from_utf8(&before).context("instruction file is not UTF-8; unchanged")?;
        let mut refs = vec![];
        if h.name == "claude" || h.name == "codex" {
            let rtk_md = path.with_file_name("RTK.md");
            refs = vec!["@RTK.md".into(), format!("@{}", rtk_md.display())];
            if let Some(bytes) = pending.read(&rtk_md)? {
                ensure!(
                    stock_match("awareness", &bytes, stock),
                    "{}: modified or unknown RTK.md; manual migration required; no files changed",
                    rtk_md.display()
                );
                migration.change(rtk_md, Some(bytes), None)?;
            }
        }
        let cleaned = clean_rtk_guidance(text, &refs, stock)
            .with_context(|| format!("{} unchanged", path.display()))?;
        let dedicated_rtk = h.name == "antigravity";
        if dedicated_rtk && cleaned == text {
            migration.messages.push(format!(
                "{}: no stock RTK guidance matched; existing text preserved for manual review",
                path.display()
            ));
        }
        let after = if dedicated_rtk && cleaned.is_empty() {
            None
        } else {
            Some(cleaned.into_bytes())
        };
        migration.change(path, Some(before), after)?;
    }
    Ok(migration)
}

pub fn plan(args: &[OsString], roots: &Roots) -> Result<Plan> {
    plan_using_stock(args, roots, STOCK)
}
#[cfg(test)]
#[allow(dead_code)] // Called by the separate setup integration-test crate.
pub fn plan_with_stock(
    args: &[OsString],
    roots: &Roots,
    stock: &[(&str, usize, &str)],
) -> Result<Plan> {
    plan_using_stock(args, roots, stock)
}
fn plan_using_stock(
    args: &[OsString],
    roots: &Roots,
    stock: &[(&str, usize, &str)],
) -> Result<Plan> {
    let o = options(args)?;
    let mut plan = Plan::default();
    if o.help {
        plan.messages.push(HELP.into());
        return Ok(plan);
    }
    let mut selected = 0;
    for &name in HOSTS {
        if name == "antigravity" && o.agent.is_none() && !(o.replace && o.project) {
            continue;
        }
        if name == "kilocode" {
            continue;
        }
        if o.agent.as_deref().is_some_and(|a| a != name) {
            continue;
        }
        if o.project
            && o.agent.is_none()
            && ((name == "kimi" || name == "openclaw")
                || (name == "hermes"
                    && !roots.project.join(".hermes.md").exists()
                    && !roots.project.join("HERMES.md").exists()))
        {
            continue;
        }
        let mut h = host(name, roots, o.project);
        if name == "droid" {
            h.output = Some(droid_output(&h)?);
        }
        if name == "cline" && o.project && h.root.is_file() {
            h.instructions = Some(h.root.clone());
            h.dedicated = false;
        }
        let legacy_only = o.replace
            && o.project
            && match name {
                "claude" => roots.project.join("CLAUDE.md").is_file(),
                "codex" => {
                    let guidance = roots.project.join("AGENTS.md");
                    let awareness = roots.project.join("RTK.md");
                    read(&awareness)?.is_some_and(|b| stock_match("awareness", &b, stock))
                        && read(&guidance)?.is_some_and(|b| {
                            std::str::from_utf8(&b).is_ok_and(|s| {
                                s.lines().any(|l| {
                                    l == "@RTK.md" || l == format!("@{}", awareness.display())
                                })
                            })
                        })
                }
                "cline" => h.root.is_file(),
                "windsurf" => roots.project.join(".windsurfrules").is_file(),
                "kilo" => roots.project.join(".kilocode/rules/rtk-rules.md").is_file(),
                _ => false,
            };
        if o.agent.is_none() && !h.root.is_dir() && !legacy_only {
            continue;
        }
        if let Some(reason) = prehook_unavailable(name) {
            plan.messages.push(format!("{name}: {reason}; RTK automation and trust settings preserved unless --instructions-only is selected"));
            h.limitation = Some(reason);
            if cfg!(windows)
                && matches!(name, "codex" | "vibe")
                && !o.instructions_only
                && !o.uninstall
                && !o.show
            {
                continue;
            }
        }
        if name == "kilo" && o.project && o.replace {
            let legacy_rules = roots.project.join(".kilocode/rules/rtk-rules.md");
            if read(&legacy_rules)?.is_some() {
                plan.messages.push(format!("{}: legacy Kilo rules preserved; current-generation plugin is not an established replacement for the legacy extension; manual migration required", legacy_rules.display()));
                continue;
            }
        }
        if matches!(name, "hermes" | "openclaw") && !o.project && !o.instructions_only {
            selected += 1;
            native_plugin_change(&mut plan, &h, roots, &o, stock)?;
            continue;
        }
        if name == "vibe" {
            selected += 1;
            vibe_change(&mut plan, &h, &o)?;
            continue;
        }
        let mut files = vec![];
        let mut recognized = 0;
        let mut installed = std::collections::HashSet::new();
        let mut native_messages = vec![];
        for path in settings_paths(&h)? {
            let bytes = plan.read(&path)?;
            let value = json_file(&path, bytes.as_deref())?;
            let mut probe = value.clone();
            match remove_entries(&mut probe, &h, &path, true, false, stock) {
                Ok(count) => recognized += count,
                Err(error) if !o.replace => plan.messages.push(format!(
                    "{}: RTK integration left unchanged: {error:#}",
                    path.display()
                )),
                Err(error) => return Err(error),
            }
            if h.output.is_some() {
                let (count, messages) = native_status(&value, &h, &path)?;
                installed.extend(count);
                native_messages.extend(messages);
            }
            files.push((path, bytes, value));
        }
        if matches!(name, "copilot" | "vscode") {
            plan.messages.push(format!(
                "{name}: CLI post-output supported; {}",
                prehook_unavailable("vscode").unwrap()
            ));
            if recognized > 0 && !o.instructions_only && !o.uninstall && !o.show {
                plan.messages.push("Shared RTK Copilot activation preserved: both replacement consumers are not supported".into());
                continue;
            }
        } else if prehook_unavailable(name).is_some() {
            h.output = None;
        }
        if o.instructions_only {
            ensure!(
                h.instructions.is_some() || matches!(name, "copilot" | "vscode"),
                "{name} has no instruction-only target"
            );
            if matches!(name, "copilot" | "vscode") {
                h.instructions = Some(h.root.join("copilot-instructions.md"));
            }
            h.output = None;
            h.plugin = None;
            plan.messages.push(format!(
                "{name}: explicit instruction-only downgrade selected"
            ));
        }
        if o.replace
            && recognized > 0
            && h.output.is_none()
            && h.plugin.is_none()
            && !o.instructions_only
        {
            plan.messages.push(format!("{name}: existing automatic RTK hooks preserved; no automatic replacement configured; select --agent {name} --instructions-only to explicitly downgrade"));
            continue;
        }
        let legacy_plugin = h.plugin.as_ref().map(|p| p.with_file_name("rtk.ts"));
        let has_legacy_plugin = legacy_plugin
            .as_ref()
            .map(|p| read(p))
            .transpose()?
            .flatten()
            .is_some();
        let migration = if o.replace {
            rtk_migration(&h, roots, o.project, stock, &plan)?
        } else {
            Plan::default()
        };
        recognized += migration.changes.len();
        plan.messages.extend(migration.messages.iter().cloned());
        if o.replace && o.agent.is_none() && recognized == 0 {
            continue;
        }
        selected += 1;
        if h.output.is_none() && h.instructions.is_none() && h.plugin.is_none() {
            plan.messages.push(format!(
                "{name}: unavailable; {}",
                h.limitation.unwrap_or("no qualified installation target")
            ));
            continue;
        }
        let plugin_text = h
            .plugin
            .as_ref()
            .map(|_| plugin_text(&h, roots))
            .transpose()?;
        let plugin_executable = h
            .plugin
            .as_ref()
            .map(|path| plan.read(path))
            .transpose()?
            .flatten()
            .and_then(|bytes| plugin_executable(&bytes, &h));
        let plugin_installed = plugin_executable
            .as_deref()
            .is_some_and(executable_available);
        if let Some(executable) = &plugin_executable {
            plan.messages.push(format!(
                "{name}: plugin executable {} ({})",
                if plugin_installed {
                    "available"
                } else {
                    "missing or not executable"
                },
                executable.display()
            ));
        }
        let instruction_installed = h
            .instructions
            .as_ref()
            .map(|p| read(p))
            .transpose()?
            .flatten()
            .is_some_and(|b| {
                std::str::from_utf8(&b)
                    .ok()
                    .and_then(|text| instruction_range(text, &h))
                    .is_some()
            });
        plan.messages.push(format!(
            "{name}: {}; {}; recognized RTK entries: {recognized}{}",
            mode(&h),
            if h.output.is_some() {
                if matches!(name, "copilot" | "vscode") {
                    if installed.contains("copilot") {
                        "CLI post-output configured; VS Code native rewrite unavailable"
                    } else {
                        "CLI post-output not configured; VS Code native rewrite unavailable"
                    }
                } else if !installed.is_empty() {
                    "configured"
                } else {
                    "not fully configured"
                }
            } else if h.plugin.is_some() {
                if plugin_installed {
                    "configured"
                } else {
                    "not fully configured"
                }
            } else if instruction_installed {
                "configured"
            } else {
                "not configured"
            },
            h.limitation.map(|s| format!("; {s}")).unwrap_or_default()
        ));
        plan.messages.extend(native_messages);
        if h.output.is_some() {
            plan.messages.push(format!(
                "{name}: host version, discovery, trust and runtime loading not probed"
            ));
        }
        if o.show {
            continue;
        }
        for change in migration.changes {
            plan.change(change.path, change.before, change.after)?;
        }
        for (path, before, mut value) in files {
            let original = value.clone();
            remove_entries(
                &mut value,
                &h,
                &path,
                o.replace,
                o.uninstall || o.instructions_only,
                stock,
            )?;
            if !o.uninstall && h.output.as_ref() == Some(&path) {
                install_native(&mut value, &h)?;
            }
            if value != original {
                let mut after = serde_json::to_vec_pretty(&value)?;
                after.push(b'\n');
                if before
                    .as_deref()
                    .is_some_and(|b| b.starts_with(b"\xef\xbb\xbf"))
                {
                    after.splice(..0, [0xef, 0xbb, 0xbf]);
                }
                plan.change(path, before, Some(after))?;
            }
        }
        instruction_change(&mut plan, &h, o.uninstall)?;
        if let (Some(path), Some(text)) = (&h.plugin, plugin_text) {
            let before = plan.read(path)?;
            let owned = before
                .as_deref()
                .is_some_and(|bytes| owned_plugin(bytes, &h));
            if o.uninstall {
                if owned {
                    plan.change(path.clone(), before, None)?;
                } else if before.is_some() {
                    plan.messages
                        .push(format!("{name}: edited or unowned plugin preserved"));
                }
            } else {
                ensure!(
                    before.is_none() || owned,
                    "edited or unowned plugin; unchanged: {}",
                    path.display()
                );
                ensure!(
                    !has_legacy_plugin || o.replace,
                    "RTK plugin is present; review/remove it before installing Retok for {name}"
                );
                plan.change(path.clone(), before, Some(text.into_bytes()))?;
            }
        }
    }
    if selected == 0 {
        plan.messages.push("No matching agent integration found. Select a host with --agent HOST; use --project for local-only setup.".into());
    }
    Ok(plan)
}

pub fn run_with_roots(args: &[OsString], roots: &Roots) -> Result<()> {
    let o = options(args)?;
    if o.help {
        println!("{HELP}");
        return Ok(());
    }
    let plan = plan(args, roots)?;
    for message in &plan.messages {
        println!("{message}");
    }
    if o.show {
        return Ok(());
    }
    if o.dry {
        for c in &plan.changes {
            println!(
                "would {} {}",
                if c.after.is_some() { "write" } else { "remove" },
                c.path.display()
            );
        }
    } else {
        for backup in plan.apply()? {
            println!("backup: {}", backup.display());
        }
        for c in &plan.changes {
            println!(
                "{} {}",
                if c.after.is_some() {
                    "wrote"
                } else {
                    "removed"
                },
                c.path.display()
            );
        }
        if plan.changes.is_empty() {
            println!("No changes.");
        }
    }
    Ok(())
}
pub fn run(args: &[OsString]) -> Result<()> {
    if options(args)?.help {
        println!("{HELP}");
        return Ok(());
    }
    run_with_roots(args, &Roots::environment()?)
}
pub fn doctor(args: &[OsString]) -> Result<()> {
    if options(args)?.help {
        return run(args);
    }
    println!(
        "Configuration inventory; host version, discovery, trust and runtime loading are not probed. Claude post-output hooks require Claude Code >=2.1.121."
    );
    #[cfg(not(test))]
    {
        match crate::state::Settings::load() {
            Ok(settings) => println!(
                "Retok config: {}; excluded commands: {}",
                if settings.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                settings.exclude_commands.len()
            ),
            Err(error) => println!("Retok config: invalid or unreadable: {error:#}"),
        }
    }
    let mut args = args.to_vec();
    args.push("--show".into());
    run(&args)
}
