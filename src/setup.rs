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
    pub codex: PathBuf,
    pub copilot: PathBuf,
    pub factory: PathBuf,
    pub kimi: PathBuf,
    pub hermes: PathBuf,
    pub vibe: PathBuf,
    pub executable: PathBuf,
}
impl Roots {
    pub fn new(home: PathBuf, project: PathBuf) -> Self {
        Self {
            config: home.join(".config"),
            codex: home.join(".codex"),
            copilot: home.join(".copilot"),
            factory: home.join(".factory"),
            kimi: home.join(".kimi-code"),
            hermes: home.join(".hermes"),
            vibe: home.join(".vibe"),
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
            ("CODEX_HOME", &mut roots.codex),
            ("COPILOT_HOME", &mut roots.copilot),
            ("FACTORY_HOME_OVERRIDE", &mut roots.factory),
            ("KIMI_CODE_HOME", &mut roots.kimi),
            ("HERMES_HOME", &mut roots.hermes),
            ("VIBE_HOME", &mut roots.vibe),
        ] {
            if let Some(value) = std::env::var_os(name).filter(|v| !v.is_empty()) {
                *path = value.into();
            }
        }
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
            other => bail!("unknown setup option: {other}"),
        }
    }
    ensure!(!(out.all && out.agent.is_some()), "choose --agent or --all");
    ensure!(
        !(out.replace && out.uninstall),
        "--replace-rtk and --uninstall cannot be combined"
    );
    Ok(out)
}

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
        ("codex", false) => r.codex.clone(),
        ("copilot", false) => r.copilot.clone(),
        ("droid", false) => r.factory.clone(),
        ("kimi", false) => r.kimi.clone(),
        ("hermes", false) => r.hermes.clone(),
        ("vibe", false) => r.vibe.clone(),
        ("kimi" | "hermes" | "vibe" | "openclaw", true) => r.project.clone(),
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
            h.settings.push(h.root.join("hooks.json"));
            h.instructions = Some(if project {
                base.join("AGENTS.md")
            } else {
                h.root.join("AGENTS.md")
            });
            h.limitation = Some(
                "guidance only: native output replacement is not supported; recognized RTK hooks are removed only with --replace-rtk; existing hooks require /hooks trust review",
            );
        }
        "copilot" => {
            h.output = Some(h.root.join("hooks/retok.json"));
        }
        "cursor" => {
            h.settings.push(h.root.join("hooks.json"));
            if project {
                h.instructions = Some(h.root.join("rules/retok.mdc"));
                h.dedicated = true;
            } else {
                h.limitation = Some(
                    "global user rules require host UI; select --project for verified rule-file installation",
                );
            }
        }
        "gemini" => {
            h.settings.push(h.root.join("settings.json"));
            h.instructions = Some(if project {
                base.join("GEMINI.md")
            } else {
                h.root.join("GEMINI.md")
            });
        }
        "vscode" => {
            if project {
                h.instructions = Some(h.root.join("copilot-instructions.md"));
            } else {
                h.limitation = Some("global instruction path is user-configured; select --project");
            }
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
            h.instructions = Some(h.root.join("AGENTS.md"));
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
            h.limitation = Some(
                "guidance only; native output replacement and automatic RTK plugin/config migration are not supported",
            );
        }
        "pi" | "omp" => {
            h.plugin = Some(h.root.join(if project {
                "extensions/retok.ts"
            } else {
                "agent/extensions/retok.ts"
            }));
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
    } else if h.output.is_some() {
        "native post-output hook"
    } else {
        "instructions only (agent must choose Retok explicitly)"
    }
}

/// Refuse symlinks at every existing component, including dangling symlinks.
fn no_symlinks(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        match fs::symlink_metadata(part) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "refusing symlink: {}",
                part.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    no_symlinks(path)?;
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
            let raw: Box<serde_json::value::RawValue> = serde_json::from_slice(bytes)?;
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
    let executable = if h.name == "copilot" {
        let Some(path) = entry.get("exec").and_then(Value::as_str) else {
            return false;
        };
        path.to_owned()
    } else {
        let Some(command) = entry.get("command").and_then(Value::as_str) else {
            return false;
        };
        let prefix = if cfg!(windows) { "& '" } else { "'" };
        let suffix = format!("' hook {}", h.name);
        let Some(escaped) = command
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(&suffix))
        else {
            return false;
        };
        if cfg!(windows) {
            escaped.replace("''", "'")
        } else {
            escaped.replace("'\\''", "'")
        }
    };
    // Accept exactly what our writer emits, varying only the absolute path.
    // Re-rendering rejects shell suffixes, alternate quoting and edited fields.
    let Ok(previous) = native_entry_at(h, Path::new(&executable)) else {
        return false;
    };
    if h.name == "copilot" {
        entry == &previous
    } else {
        entry == &previous["hooks"][0]
    }
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
    filter_hooks(v, h, path, |item| {
        Ok((rtk && rtk_entry(item, h, stock)?) || (own && owned_entry(item, h)))
    })
}
fn filter_hooks(
    v: &mut Value,
    h: &Host,
    path: &Path,
    matches: impl Fn(&Value) -> Result<bool>,
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
                    if matches(&nested[index])? {
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
            } else if matches(&entry)? {
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
        Ok(
            json!({"type":"command","matcher":"bash|powershell","exec":path,"args":["hook","copilot"],"timeoutSec":10}),
        )
    } else {
        let mut hook =
            json!({"type":"command","command":native_shell_command(path, h.name, cfg!(windows))});
        if cfg!(windows) {
            hook["shell"] = json!("powershell");
        }
        Ok(json!({"matcher":"Bash|PowerShell", "hooks":[hook]}))
    }
}
fn install_native(v: &mut Value, h: &Host) -> Result<()> {
    let desired = native_entry(h)?;
    let desired_hook = if h.name == "copilot" {
        &desired
    } else {
        &desired["hooks"][0]
    };
    // Upgrade exact generated entries from another binary location and legacy
    // PATH entries. Mixed/custom invocations are never treated as owned.
    filter_hooks(v, h, h.output.as_ref().unwrap(), |item| {
        Ok(owned_entry(item, h) && item != desired_hook)
    })?;
    let top = v.as_object_mut().unwrap();
    if h.name == "copilot" {
        ensure!(
            top.get("version").is_none_or(|v| v == &json!(1)),
            "unsupported Copilot hook version; unchanged"
        );
        top.insert("version".into(), json!(1));
    }
    let hooks = top
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("hooks must be an object")?;
    let event = if h.name == "copilot" {
        "postToolUse"
    } else {
        "PostToolUse"
    };
    let entries = hooks
        .entry(event)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("post hook event must be an array")?;
    let present = if h.name == "copilot" {
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

#[derive(Debug)]
pub struct Change {
    pub path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}
#[derive(Default, Debug)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub messages: Vec<String>,
}
impl Plan {
    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        match self.changes.iter().find(|c| c.path == path) {
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
        if let Some(old) = self.changes.iter_mut().find(|c| c.path == path) {
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
            ensure!(
                read(&c.path)? == c.before,
                "configuration changed since planning: {}",
                c.path.display()
            );
        }
        let mut applied: Vec<&Change> = vec![];
        let mut backups = vec![];
        for c in &self.changes {
            let result = (|| -> Result<()> {
                ensure!(
                    read(&c.path)? == c.before,
                    "configuration changed since planning: {}",
                    c.path.display()
                );
                if let Some(before) = &c.before {
                    let (backup, mut file) = unique_file(&c.path, "backup")?;
                    file.write_all(before)?;
                    file.sync_all()?;
                    backups.push(backup);
                }
                replace(&c.path, c.before.as_deref(), c.after.as_deref())
            })();
            if let Err(error) = result {
                let mut failures = vec![];
                for done in applied.into_iter().rev() {
                    if let Err(e) =
                        replace(&done.path, done.after.as_deref(), done.before.as_deref())
                    {
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
        no_symlinks(path)?;
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
fn settings_paths(h: &Host) -> Result<Vec<PathBuf>> {
    let mut paths = h.settings.clone();
    if h.name == "copilot" || h.name == "vscode" {
        let dir = h.root.join("hooks");
        no_symlinks(&dir)?;
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
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Ok(template) = plugin_template(h) else {
        return false;
    };
    let Some((prefix, suffix)) = template.split_once("__RETOK_EXECUTABLE__") else {
        return false;
    };
    let Some(literal) = text
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
    else {
        return false;
    };
    let Ok(executable) = serde_json::from_str::<String>(literal) else {
        return false;
    };
    Path::new(&executable).is_absolute()
        && serde_json::to_string(&executable).is_ok_and(|canonical| canonical == literal)
}

const HELP: &str = "Usage: retok init [--agent HOST | --all] [--global | -g | --project]
                  [--replace-rtk] [--dry-run] [--show | --uninstall]

Default: global scope, detected existing agent homes only. --project stays local.
--agent HOST selects one host explicitly; --all selects detected hosts.
--replace-rtk selects recognizable RTK integrations and migrates supported stock
files. Modified or unrecognized RTK plugins/blocks require manual migration.
--dry-run previews writes and removals; --show reports status without writes.
--uninstall removes unchanged Retok-owned files/blocks and exact hook entries.
--help, -h shows this help without reading agent configuration.

Native output integration: claude (>=2.1.121), copilot CLI, pi, omp, opencode,
kilo (current plugin generation; kilocode alias). Codex uses guidance only.
Instruction fallback: codex, gemini, droid, roo, kimi, hermes, vibe; project cursor, vscode, cline,
windsurf, antigravity, openclaw. Global Cursor/VS Code/Cline remain unqualified.
OpenClaw requires --agent openclaw --project in the selected workspace; no
automatic middleware activation or RTK plugin removal is performed.
Kimi uses KIMI_CODE_HOME/AGENTS.md; Hermes uses HERMES_HOME/SOUL.md.
Vibe uses VIBE_HOME/AGENTS.md; TOML hook migration remains manual.
Project Kimi/Hermes/Vibe guidance requires --agent unless Hermes-specific files exist.
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
            && ((name == "kimi" || name == "vibe" || name == "openclaw")
                || (name == "hermes"
                    && !roots.project.join(".hermes.md").exists()
                    && !roots.project.join("HERMES.md").exists()))
        {
            continue;
        }
        let mut h = host(name, roots, o.project);
        if name == "cline" && o.project && h.root.is_file() {
            h.instructions = Some(h.root.clone());
            h.dedicated = false;
        }
        let legacy_only = o.replace
            && o.project
            && match name {
                "claude" => roots.project.join("CLAUDE.md").is_file(),
                "cline" => h.root.is_file(),
                "windsurf" => roots.project.join(".windsurfrules").is_file(),
                "kilo" => roots.project.join(".kilocode/rules/rtk-rules.md").is_file(),
                _ => false,
            };
        if o.agent.is_none() && !h.root.is_dir() && !legacy_only {
            continue;
        }
        if name == "kilo" && o.project && o.replace {
            let legacy_rules = roots.project.join(".kilocode/rules/rtk-rules.md");
            if read(&legacy_rules)?.is_some() {
                plan.messages.push(format!("{}: legacy Kilo rules preserved; current-generation plugin is not an established replacement for the legacy extension; manual migration required", legacy_rules.display()));
                continue;
            }
        }
        if name == "hermes" && !o.project {
            let rtk_plugin = h.root.join("plugins/rtk-rewrite");
            no_symlinks(&rtk_plugin)?;
            if rtk_plugin.exists() {
                plan.messages.push(format!(
                    "{}: RTK plugin and config.yaml preserved; manual migration required",
                    rtk_plugin.display()
                ));
                if o.replace {
                    continue;
                }
            }
        }
        if name == "vibe" && !o.project {
            let hooks = h.root.join("hooks.toml");
            no_symlinks(&hooks)?;
            if hooks.exists() {
                plan.messages.push(format!(
                    "{}: TOML hooks preserved; RTK hook migration requires manual review",
                    hooks.display()
                ));
                if o.replace {
                    continue;
                }
            }
        }
        if name == "openclaw" && !o.project {
            let rtk_plugin = h.root.join("extensions/rtk-rewrite");
            no_symlinks(&rtk_plugin)?;
            if rtk_plugin.exists() {
                plan.messages.push(format!(
                    "{}: RTK plugin and configuration preserved; manual migration required",
                    rtk_plugin.display()
                ));
            }
        }
        let mut files = vec![];
        let mut recognized = 0;
        let mut installed = 0;
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
            let mut probe = value.clone();
            installed += remove_entries(&mut probe, &h, &path, false, true, stock)?;
            files.push((path, bytes, value));
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
        let plugin_installed = if let Some(path) = &h.plugin {
            read(path)?.is_some_and(|bytes| owned_plugin(&bytes, &h))
        } else {
            false
        };
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
            if installed > 0 || instruction_installed || plugin_installed {
                "installed"
            } else {
                "not installed"
            },
            h.limitation.map(|s| format!("; {s}")).unwrap_or_default()
        ));
        if o.show {
            continue;
        }
        for change in migration.changes {
            plan.change(change.path, change.before, change.after)?;
        }
        for (path, before, mut value) in files {
            let original = value.clone();
            remove_entries(&mut value, &h, &path, o.replace, o.uninstall, stock)?;
            if !o.uninstall && h.output.as_ref() == Some(&path) {
                install_native(&mut value, &h)?;
            }
            if value != original {
                let mut after = serde_json::to_vec_pretty(&value)?;
                after.push(b'\n');
                plan.change(path, before, Some(after))?;
            }
        }
        instruction_change(&mut plan, &h, o.uninstall)?;
        if let (Some(path), Some(text)) = (&h.plugin, plugin_text) {
            let before = read(path)?;
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
    println!(
        "Claude native output hooks require Claude Code >=2.1.121; host version not probed. Codex uses instructions only, not native output replacement."
    );
    let mut args = args.to_vec();
    args.push("--show".into());
    run(&args)
}
