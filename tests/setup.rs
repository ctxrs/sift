#[allow(dead_code)]
#[path = "../src/setup.rs"]
mod setup;

use serde_json::{Value, json};
use setup::{Roots, plan};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    roots: Roots,
}
impl Fixture {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "retok-setup-test-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let roots = Roots::new(root.join("home"), root.join("project"));
        fs::create_dir(&roots.home).unwrap();
        fs::create_dir(&roots.project).unwrap();
        Self { root, roots }
    }
    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.roots.home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
    fn plan(&self, args: &[&str]) -> setup::Plan {
        plan(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            &self.roots,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn value(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn migration_preserves_mixed_hooks_permissions_and_exact_backup() {
    let f = Fixture::new();
    let path = f.write(
        ".claude/settings.json",
        r#"{
  "permissions": {"deny": ["Bash(rm *)"]},
  "hooks": {"PreToolUse": [{"matcher":"Bash","hooks":[
    {"type":"command","command":"rtk hook claude"},
    {"type":"command","command":"bash ~/.claude/hooks/rtk-rewrite.sh"},
    {"type":"command","command":"echo ~/.claude/hooks/rtk-rewrite.sh"},
    {"type":"command","command":"/custom/rtk-rewrite.sh"},
    {"type":"command","command":"policy-check"}]}],
    "PostToolUse": [{"matcher":"Bash","hooks":[{"type":"command","command":"audit"}]}]},
  "unrelated": "keep me"
}"#,
    );
    f.write(".claude/hooks/rtk-rewrite.sh", SYNTHETIC_SCRIPT);
    let original = fs::read(&path).unwrap();
    let p = stock_plan(&f, &["--replace-rtk"]).unwrap();
    assert_eq!(p.changes.len(), 1);
    let backups = p.apply().unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), original);
    let v = value(&path);
    assert_eq!(v["permissions"]["deny"], json!(["Bash(rm *)"]));
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        v["hooks"]["PostToolUse"][1]["hooks"][0]["command"],
        setup::native_shell_command(
            f.roots.executable.to_str().unwrap(),
            "claude",
            cfg!(windows)
        )
    );
    assert_eq!(v["hooks"]["PostToolUse"][1]["matcher"], "Bash|PowerShell");
    assert!(f.plan(&["--replace-rtk"]).changes.is_empty());
    let bytes = fs::read(&path).unwrap();
    assert!(f.plan(&["--agent", "claude"]).changes.is_empty());
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn uninstall_preserves_later_additions_and_backups_do_not_overwrite() {
    let f = Fixture::new();
    let path = f.write(".claude/settings.json", "{ \"theme\": \"dark\" }\n");
    let first = f.plan(&["--agent", "claude"]).apply().unwrap();
    let mut v = value(&path);
    v["hooks"]["PostToolUse"][0]["hooks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"command":"later-hook"}));
    fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    let second = f
        .plan(&["--agent", "claude", "--uninstall"])
        .apply()
        .unwrap();
    assert_ne!(first, second);
    assert!(first[0].exists());
    let v = value(&path);
    assert_eq!(v["theme"], "dark");
    assert_eq!(
        v["hooks"]["PostToolUse"][0]["hooks"],
        json!([{"command":"later-hook"}])
    );
    assert!(
        f.plan(&["--agent", "claude", "--uninstall"])
            .changes
            .is_empty()
    );
}

#[test]
fn codex_migration_is_guidance_only_and_project_stays_local() {
    let f = Fixture::new();
    let global = f.write(".codex/hooks.json", r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"command":"rtk hook codex"},{"command":"audit"}]}]}}"#);
    let before = fs::read(&global).unwrap();
    fs::create_dir_all(f.roots.project.join(".codex")).unwrap();
    fs::write(f.roots.project.join(".codex/hooks.json"), &before).unwrap();
    let p = f.plan(&["--agent", "codex", "--project", "--replace-rtk"]);
    assert!(p.messages.join(" ").contains("guidance only"));
    p.apply().unwrap();
    assert_eq!(fs::read(&global).unwrap(), before);
    let v = value(&f.roots.project.join(".codex/hooks.json"));
    assert!(v["hooks"].get("PostToolUse").is_none());
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["hooks"],
        json!([{"command":"audit"}])
    );
    assert!(
        fs::read_to_string(f.roots.project.join("AGENTS.md"))
            .unwrap()
            .contains("programmatic")
    );
    assert!(!f.roots.codex.join("AGENTS.md").exists());
}

#[test]
fn copilot_migrates_other_file_and_installs_native_exec() {
    let f = Fixture::new();
    let legacy = f.write(".copilot/hooks/rtk-rewrite.json", r#"{"version":1,"hooks":{"PreToolUse":[{"type":"command","bash":"rtk hook copilot"},{"command":"audit"}]}}"#);
    let p = f.plan(&["--replace-rtk"]);
    assert_eq!(p.changes.len(), 2);
    p.apply().unwrap();
    assert_eq!(
        value(&legacy)["hooks"]["PreToolUse"],
        json!([{"command":"audit"}])
    );
    let v = value(&f.roots.copilot.join("hooks/retok.json"));
    assert_eq!(v["version"], 1);
    assert_eq!(
        v["hooks"]["postToolUse"][0]["exec"],
        f.roots.executable.to_str().unwrap()
    );
    assert_eq!(
        v["hooks"]["postToolUse"][0]["args"],
        json!(["hook", "copilot"])
    );
    assert!(v["hooks"].get("PreToolUse").is_none());
}

#[test]
fn corrupted_or_malformed_config_prevents_every_write() {
    for bad in [
        "{broken",
        r#"{"hooks":{},"hooks":{}}"#,
        r#"{"hooks":null}"#,
        r#"{"hooks":{"PreToolUse":{}}}"#,
        r#"{"hooks":{"PreToolUse":[{"hooks":null}]}}"#,
    ] {
        let f = Fixture::new();
        let good = f.write(".claude/settings.json", "{}");
        let bad_path = f.write(".codex/hooks.json", bad);
        assert!(plan(&[], &f.roots).is_err());
        assert_eq!(fs::read_to_string(good).unwrap(), "{}");
        assert_eq!(fs::read_to_string(bad_path).unwrap(), bad);
        assert!(!f.roots.codex.join("AGENTS.md").exists());
    }
}

#[test]
fn no_matches_and_migration_only_select_recognized_roots() {
    let f = Fixture::new();
    let p = f.plan(&[]);
    assert!(p.changes.is_empty());
    assert!(p.messages[0].contains("--agent"));
    let path = f.write(
        ".claude/settings.json",
        r#"{"hooks":{"PreToolUse":[{"command":"echo rtk hook claude"}]}}"#,
    );
    fs::create_dir_all(&f.roots.codex).unwrap();
    let p = f.plan(&["--replace-rtk"]);
    assert!(p.changes.is_empty());
    assert!(!f.roots.codex.join("AGENTS.md").exists());
    assert!(fs::read_to_string(path).unwrap().contains("echo rtk"));
}

#[test]
fn dry_run_and_status_do_not_create_directories() {
    let f = Fixture::new();
    for option in ["--dry-run", "--show"] {
        setup::run_with_roots(
            &["--agent".into(), "claude".into(), option.into()],
            &f.roots,
        )
        .unwrap();
        assert!(!f.roots.home.join(".claude").exists());
    }
}

#[test]
fn concurrent_modification_after_planning_is_preserved() {
    let f = Fixture::new();
    let a = f.write(".claude/settings.json", "{}");
    let b = f.write(".codex/AGENTS.md", "{}");
    let p = f.plan(&[]);
    fs::write(&b, "{\"user\":true}").unwrap();
    assert!(p.apply().is_err());
    assert_eq!(fs::read_to_string(a).unwrap(), "{}");
    assert_eq!(fs::read_to_string(b).unwrap(), "{\"user\":true}");
}

#[cfg(unix)]
#[test]
fn symlink_file_parent_and_late_replacement_are_rejected() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let target = f.write("outside.json", "{}");
    fs::create_dir_all(f.roots.home.join(".claude")).unwrap();
    let path = f.roots.home.join(".claude/settings.json");
    symlink(&target, &path).unwrap();
    assert!(plan(&["--agent".into(), "claude".into()], &f.roots).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "{}");
    fs::remove_file(&path).unwrap();
    fs::write(&path, "{}").unwrap();
    let p = f.plan(&["--agent", "claude"]);
    fs::remove_file(&path).unwrap();
    symlink(&target, &path).unwrap();
    assert!(p.apply().is_err());
    fs::remove_file(&path).unwrap();
    fs::remove_dir(f.roots.home.join(".claude")).unwrap();
    symlink(&f.roots.project, f.roots.home.join(".claude")).unwrap();
    assert!(plan(&["--agent".into(), "claude".into()], &f.roots).is_err());
}

#[test]
fn instructions_remove_only_unchanged_owned_block() {
    let f = Fixture::new();
    let path = f.write(".gemini/GEMINI.md", "Keep these instructions.\n");
    f.plan(&["--agent", "gemini"]).apply().unwrap();
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("Later user guidance.\n");
    fs::write(&path, text).unwrap();
    f.plan(&["--agent", "gemini", "--uninstall"])
        .apply()
        .unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "Keep these instructions.\nLater user guidance.\n"
    );
    f.plan(&["--agent", "gemini"]).apply().unwrap();
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("suitable", "approved");
    fs::write(&path, &text).unwrap();
    assert!(
        f.plan(&["--agent", "gemini", "--uninstall"])
            .changes
            .is_empty()
    );
    assert_eq!(fs::read_to_string(path).unwrap(), text);
}

#[test]
fn plugin_install_is_self_contained_idempotent_and_edit_safe() {
    for (host, path) in [
        ("pi", ".pi/agent/extensions/retok.ts"),
        ("omp", ".omp/agent/extensions/retok.ts"),
        ("opencode", ".config/opencode/plugins/retok.ts"),
        ("kilocode", ".config/kilo/plugin/retok.ts"),
    ] {
        let f = Fixture::new();
        f.plan(&["--agent", host]).apply().unwrap();
        let path = f.roots.home.join(path);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("// retok managed plugin"));
        assert!(!text.contains("__RETOK_"));
        assert!(text.contains("execFile"));
        assert!(f.plan(&["--agent", host]).changes.is_empty());
        f.plan(&["--agent", host, "--uninstall"]).apply().unwrap();
        assert!(!path.exists());
        f.plan(&["--agent", host]).apply().unwrap();
        fs::write(&path, format!("{text}\n// user edit")).unwrap();
        assert!(f.plan(&["--agent", host, "--uninstall"]).changes.is_empty());
        assert!(path.exists());
    }
}

#[test]
fn unverified_rtk_plugin_is_not_overwritten_or_claimed_migrated() {
    let f = Fixture::new();
    let path = f.write(".pi/agent/extensions/rtk.ts", "// user plugin using rtk\n");
    let error = plan(&["--replace-rtk".into()], &f.roots).unwrap_err();
    assert!(error.to_string().contains("manual migration"));
    assert!(path.exists());
    assert!(!path.with_file_name("retok.ts").exists());
}

#[test]
fn opaque_json_number_marker_objects_and_numeric_lexemes_survive() {
    use serde_json::value::RawValue;
    use std::collections::BTreeMap;
    let f = Fixture::new();
    let path = f.write(".claude/settings.json", r#"{"opaque":{"\u0024serde_json::private::Number":"123","nested":[{"$serde_json::private::Number":{"keep":true}}]},"huge":1234567890123456789012345678901234567890,"precise":1.23000000000000000000000000001e+99}"#);
    f.plan(&["--agent", "claude"]).apply().unwrap();
    let fields: BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        fields["huge"].get(),
        "1234567890123456789012345678901234567890"
    );
    assert_eq!(
        fields["precise"].get(),
        "1.23000000000000000000000000001e+99"
    );
    let opaque: BTreeMap<String, Box<RawValue>> =
        serde_json::from_str(fields["opaque"].get()).unwrap();
    assert_eq!(opaque["$serde_json::private::Number"].get(), "\"123\"");
    let nested: Vec<BTreeMap<String, Box<RawValue>>> =
        serde_json::from_str(opaque["nested"].get()).unwrap();
    let preserved: BTreeMap<String, bool> =
        serde_json::from_str(nested[0]["$serde_json::private::Number"].get()).unwrap();
    assert_eq!(preserved.get("keep"), Some(&true));
    assert!(f.plan(&["--agent", "claude"]).changes.is_empty());
    let original =
        r#"{"nested":[{"\u0024serde_json::private::Number":1,"$serde_json::private::Number":2}]}"#;
    fs::write(&path, original).unwrap();
    assert!(plan(&["--agent".into(), "claude".into()], &f.roots).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn a_later_write_failure_rolls_back_prior_files() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let first = f.write(".claude/settings.json", "{ \"theme\": \"dark\" }\n");
    let second = f.write(".codex/AGENTS.md", "Original instructions.\n");
    let p = f.plan(&[]);
    fs::set_permissions(&f.roots.codex, fs::Permissions::from_mode(0o500)).unwrap();
    let result = p.apply();
    fs::set_permissions(&f.roots.codex, fs::Permissions::from_mode(0o700)).unwrap();
    // A privileged test runner can bypass directory permissions.
    if result.is_ok() && unsafe { libc::geteuid() } == 0 {
        return;
    }
    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(first).unwrap(),
        "{ \"theme\": \"dark\" }\n"
    );
    assert_eq!(
        fs::read_to_string(second).unwrap(),
        "Original instructions.\n"
    );
}

// Authored synthetic fixtures; independently pinned with Python hashlib.
// No RTK implementation or instruction text is copied into the test suite.
const SYNTHETIC_SCRIPT: &str = "#!/bin/sh\n# Synthetic owned migration script.\n";
const SYNTHETIC_PI: &str = "// Synthetic stock Pi fixture.\n";
const SYNTHETIC_OPENCODE: &str = "// Synthetic stock OpenCode fixture.\n";
const SYNTHETIC_AWARENESS: &str = "Use the synthetic RTK fixture command.\n";
const SYNTHETIC_BLOCK: &str = "<!-- rtk-instructions v2 -->\nUse the synthetic RTK fixture command.\n<!-- /rtk-instructions -->";
const SYNTHETIC_STOCK: &[(&str, usize, &str)] = &[
    (
        "script-claude",
        46,
        "395725c0a8268332f063dacb51420883911a7f774d5edd348a9942244cfa42af",
    ),
    (
        "script-gemini",
        46,
        "395725c0a8268332f063dacb51420883911a7f774d5edd348a9942244cfa42af",
    ),
    (
        "pi",
        31,
        "bcc2b4c06b56dc255ec8e90b53a53ffd157725043f082e3d269c4a9ba49d34e9",
    ),
    (
        "opencode",
        37,
        "cbf5531aface8ae7e5822c5c4aaf4fc1fefeaa6af0e7b029aeed4c837926908c",
    ),
    (
        "awareness",
        39,
        "e0d378a2f450a0a144cf53948e2ac8448483cb73d71dba38a0f8e9c18e4efc41",
    ),
    (
        "block",
        94,
        "b79c6ace0a4a668c5c9895326bb319aef14c11860bf17713fd43a4bad0e72a60",
    ),
];
fn stock_plan(f: &Fixture, args: &[&str]) -> anyhow::Result<setup::Plan> {
    setup::plan_with_stock(
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        &f.roots,
        SYNTHETIC_STOCK,
    )
}

#[test]
fn stock_plugins_migrate_by_exact_digest_and_repeat_without_changes() {
    for (path, source) in [
        (".pi/agent/extensions/rtk.ts", SYNTHETIC_PI),
        (".omp/agent/extensions/rtk.ts", SYNTHETIC_PI),
        (".config/opencode/plugins/rtk.ts", SYNTHETIC_OPENCODE),
    ] {
        let f = Fixture::new();
        let path = f.write(path, source);
        let p = stock_plan(&f, &["--replace-rtk"]).unwrap();
        assert_eq!(p.changes.len(), 2);
        let backups = p.apply().unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read_to_string(&backups[0]).unwrap(), source);
        assert!(!path.exists());
        let installed = fs::read_to_string(path.with_file_name("retok.ts")).unwrap();
        assert!(installed.contains("retok managed plugin"));
        assert!(installed.contains("tool_result") || installed.contains("tool.execute.after"));
        assert!(
            stock_plan(&f, &["--replace-rtk"])
                .unwrap()
                .changes
                .is_empty()
        );
    }
    let f = Fixture::new();
    let path = f.write(
        ".pi/agent/extensions/rtk.ts",
        &format!("{SYNTHETIC_PI}// edited\n"),
    );
    assert!(
        stock_plan(&f, &["--replace-rtk"])
            .unwrap_err()
            .to_string()
            .contains("manual migration")
    );
    assert!(path.exists());
}

#[test]
fn stock_claude_guidance_and_imports_are_removed_without_touching_other_text() {
    let f = Fixture::new();
    let dedicated = f.write(".claude/RTK.md", SYNTHETIC_AWARENESS);
    let text = format!(
        "# Keep these rules\r\n@RTK.md\r\n\n{SYNTHETIC_BLOCK}\nUser mentions @RTK.md in prose.\n"
    );
    let shared = f.write(".claude/CLAUDE.md", &text);
    let p = stock_plan(&f, &["--replace-rtk"]).unwrap();
    let backups = p.apply().unwrap();
    assert_eq!(backups.len(), 2);
    assert!(!dedicated.exists());
    assert_eq!(
        fs::read_to_string(shared).unwrap(),
        "# Keep these rules\r\n\nUser mentions @RTK.md in prose.\n"
    );
    assert_eq!(
        value(&f.roots.home.join(".claude/settings.json"))["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        setup::native_shell_command(
            f.roots.executable.to_str().unwrap(),
            "claude",
            cfg!(windows)
        )
    );
}

#[test]
fn codex_absolute_stock_import_is_replaced_in_one_backed_up_write() {
    let f = Fixture::new();
    let dedicated = f.write(".codex/RTK.md", SYNTHETIC_AWARENESS);
    let text = format!("User instructions.\n@{}\n", dedicated.display());
    let shared = f.write(".codex/AGENTS.md", &text);
    let p = stock_plan(&f, &["--replace-rtk"]).unwrap();
    assert_eq!(p.changes.len(), 2);
    let backups = p.apply().unwrap();
    assert_eq!(backups.len(), 2);
    assert!(
        backups
            .iter()
            .any(|p| fs::read(p).unwrap() == text.as_bytes())
    );
    let now = fs::read_to_string(shared).unwrap();
    assert!(now.starts_with("User instructions.\n<!-- retok managed"));
    assert!(!now.contains("@"));
    assert!(!f.roots.codex.join("hooks.json").exists());
    assert!(
        stock_plan(&f, &["--replace-rtk"])
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn local_stock_claude_block_is_detected_without_agent_directory() {
    let f = Fixture::new();
    let path = f.roots.project.join("CLAUDE.md");
    fs::write(&path, format!("User rules.\n\n{SYNTHETIC_BLOCK}")).unwrap();
    stock_plan(&f, &["--replace-rtk", "--project"])
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "User rules.\n\n");
    assert!(f.roots.project.join(".claude/settings.json").exists());
    assert!(!f.roots.home.join(".claude").exists());
}

#[test]
fn modified_guidance_aborts_whole_migration_and_keeps_hooks() {
    for text in [
        SYNTHETIC_BLOCK.replace("fixture", "custom"),
        "<!-- rtk-instructions v2 -->\nIncomplete".into(),
    ] {
        let f = Fixture::new();
        let hook = f.write(
            ".claude/settings.json",
            r#"{"hooks":{"PreToolUse":[{"command":"rtk hook claude"}]}}"#,
        );
        let before = fs::read(&hook).unwrap();
        let path = f.write(".claude/CLAUDE.md", &text);
        assert!(stock_plan(&f, &["--replace-rtk"]).is_err());
        assert_eq!(fs::read(&hook).unwrap(), before);
        assert_eq!(fs::read_to_string(path).unwrap(), text);
    }
    let f = Fixture::new();
    let path = f.write(".codex/RTK.md", "Custom RTK instructions.\n");
    f.write(".codex/AGENTS.md", "@RTK.md\n");
    assert!(
        stock_plan(&f, &["--replace-rtk"])
            .unwrap_err()
            .to_string()
            .contains("unknown RTK.md")
    );
    assert!(path.exists());
}

#[test]
fn rules_frontmatter_is_active_and_plain_stock_suffix_preserves_user_rules() {
    for (host, legacy, new, prefix) in [
        (
            "windsurf",
            ".windsurfrules",
            ".windsurf/rules/retok.md",
            "---\ntrigger: always_on\n---\n",
        ),
        (
            "cline",
            ".clinerules",
            ".clinerules",
            "User rules.\n\n<!-- retok managed",
        ),
    ] {
        let f = Fixture::new();
        let old = f.roots.project.join(legacy);
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, format!("User rules.\n\n{SYNTHETIC_AWARENESS}")).unwrap();
        stock_plan(&f, &["--replace-rtk", "--project"])
            .unwrap()
            .apply()
            .unwrap();
        assert!(
            stock_plan(&f, &["--replace-rtk", "--project"])
                .unwrap()
                .changes
                .is_empty()
        );
        let path = f.roots.project.join(new);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(prefix), "{host}: {text}");
        if host != "cline" {
            assert_eq!(fs::read_to_string(&old).unwrap(), "User rules.\n\n");
        }
        assert!(!text.contains("synthetic RTK"));
        f.plan(&["--agent", host, "--project", "--uninstall"])
            .apply()
            .unwrap();
        if host == "cline" {
            assert_eq!(fs::read_to_string(path).unwrap(), "User rules.\n\n");
        } else {
            assert!(!path.exists());
        }
    }
    let f = Fixture::new();
    f.plan(&["--agent", "cursor", "--project"]).apply().unwrap();
    let rule = f.roots.project.join(".cursor/rules/retok.mdc");
    let bytes = fs::read_to_string(&rule).unwrap();
    assert!(bytes.starts_with("---\ndescription:"));
    assert!(bytes.contains("\nalwaysApply: true\n---\n"));
    assert!(
        f.plan(&["--agent", "cursor", "--project"])
            .changes
            .is_empty()
    );
    f.plan(&["--agent", "cursor", "--project", "--uninstall"])
        .apply()
        .unwrap();
    assert!(!rule.exists());
}

#[test]
fn help_never_reads_config_or_installs_hosts() {
    let f = Fixture::new();
    let path = f.write(".claude/settings.json", "invalid JSON");
    let p = f.plan(&["--help"]);
    assert!(p.changes.is_empty());
    assert!(p.messages[0].contains("--replace-rtk"));
    setup::run_with_roots(&["--help".into()], &f.roots).unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "invalid JSON");
}

#[test]
fn powershell_literal_path_quotes_are_escaped_without_expansion() {
    assert_eq!(
        setup::native_shell_command(
            "C:\\Program Files\\O'Brien $HOME; `echo`\\retok.exe",
            "claude",
            true
        ),
        "& 'C:\\Program Files\\O''Brien $HOME; `echo`\\retok.exe' hook claude"
    );
}

#[cfg(unix)]
#[test]
fn native_hooks_invoke_absolute_executable_with_empty_path_and_literal_metacharacters() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let mut f = Fixture::new();
    f.roots.executable = f
        .root
        .join("bin space ' $HOME; `echo` (literal)/retok ' $PATH");
    fs::create_dir_all(f.roots.executable.parent().unwrap()).unwrap();
    fs::write(&f.roots.executable, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
    fs::set_permissions(&f.roots.executable, fs::Permissions::from_mode(0o700)).unwrap();
    let claude = f.write(".claude/settings.json",r#"{"permissions":{"deny":["Bash(rm *)"]},"hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"retok hook claude"},{"command":"echo retok hook claude"}]}]}}"#);
    let original = fs::read(&claude).unwrap();
    let backups = f.plan(&["--agent", "claude"]).apply().unwrap();
    assert_eq!(fs::read(&backups[0]).unwrap(), original);
    let v = value(&claude);
    assert_eq!(v["permissions"]["deny"], json!(["Bash(rm *)"]));
    assert_eq!(
        v["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "echo retok hook claude"
    );
    let command = v["hooks"]["PostToolUse"][1]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let result = Command::new("/bin/sh")
        .args(["-c", command])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"hook\nclaude\n");
    assert!(f.plan(&["--agent", "claude"]).changes.is_empty());
    f.plan(&["--agent", "claude", "--uninstall"])
        .apply()
        .unwrap();
    assert_eq!(
        value(&claude)["hooks"]["PostToolUse"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let copilot = f.write(".copilot/hooks/retok.json",r#"{"version":1,"hooks":{"postToolUse":[{"type":"command","exec":"retok","args":["hook","copilot"]},{"exec":"retok","args":["other"]}]}}"#);
    f.plan(&["--agent", "copilot"]).apply().unwrap();
    let v = value(&copilot);
    let hooks = v["hooks"]["postToolUse"].as_array().unwrap();
    assert_eq!(hooks.len(), 2);
    assert_eq!(hooks[0]["args"], json!(["other"]));
    let result = Command::new(hooks[1]["exec"].as_str().unwrap())
        .args(
            hooks[1]["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap()),
        )
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"hook\ncopilot\n");
    assert!(f.plan(&["--agent", "copilot"]).changes.is_empty());
    f.plan(&["--agent", "copilot", "--uninstall"])
        .apply()
        .unwrap();
    assert_eq!(
        value(&copilot)["hooks"]["postToolUse"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn gemini_stock_script_registration_and_awareness_migrate_together() {
    let f = Fixture::new();
    let script = f.write(".gemini/hooks/rtk-hook-gemini.sh", SYNTHETIC_SCRIPT);
    let settings = json!({"hooks":{"BeforeTool":[{"matcher":"run_shell_command","hooks":[{"type":"command","command":script.to_str().unwrap()},{"command":format!("echo {}",script.display())}]}]}});
    let path = f.write(
        ".gemini/settings.json",
        &serde_json::to_string(&settings).unwrap(),
    );
    let instructions = f.write(".gemini/GEMINI.md", SYNTHETIC_AWARENESS);
    stock_plan(&f, &["--replace-rtk"]).unwrap().apply().unwrap();
    assert_eq!(
        value(&path)["hooks"]["BeforeTool"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(value(&path)["hooks"].get("AfterTool").is_none());
    let text = fs::read_to_string(instructions).unwrap();
    assert!(!text.contains("synthetic RTK"));
    assert!(text.contains("retok run"));
    assert!(
        stock_plan(&f, &["--replace-rtk"])
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn mixed_rtk_and_custom_invocations_require_manual_migration_without_writes() {
    for entry in [
        json!({"type":"command","bash":"rtk hook copilot","powershell":"custom-policy-check"}),
        json!({"type":"command","command":"rtk hook copilot","bash":"custom-policy-check"}),
        json!({"type":"command","exec":"rtk","args":["hook","copilot"],"powershell":"custom-policy-check"}),
        json!({"type":"command","command":"rtk hook copilot","windows":{"command":"custom-policy-check"}}),
    ] {
        let f = Fixture::new();
        let original =
            serde_json::to_string(&json!({"version":1,"hooks":{"PreToolUse":[entry]}})).unwrap();
        let path = f.write(".copilot/hooks/rtk-rewrite.json", &original);
        let earlier = f.write(
            ".claude/settings.json",
            r#"{"hooks":{"PreToolUse":[{"command":"rtk hook claude"}]}}"#,
        );
        let earlier_bytes = fs::read(&earlier).unwrap();
        let error = plan(&["--replace-rtk".into()], &f.roots).unwrap_err();
        assert!(error.to_string().contains("mixed RTK and custom"));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        assert_eq!(fs::read(earlier).unwrap(), earlier_bytes);
        assert!(!f.roots.copilot.join("hooks/retok.json").exists());
    }
    let f = Fixture::new();
    let path = f.write(".copilot/hooks/rtk-rewrite.json", r#"{"version":1,"hooks":{"PreToolUse":[{"type":"command","bash":"rtk hook copilot","powershell":"rtk hook copilot"}]}}"#);
    f.plan(&["--replace-rtk"]).apply().unwrap();
    assert_eq!(value(&path)["hooks"]["PreToolUse"], json!([]));
}

#[test]
fn legacy_kilo_rules_are_not_migrated_to_an_unproven_current_plugin() {
    for explicit in [false, true] {
        let f = Fixture::new();
        let path = f.roots.project.join(".kilocode/rules/rtk-rules.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, SYNTHETIC_AWARENESS).unwrap();
        let args = if explicit {
            vec!["--replace-rtk", "--project", "--agent", "kilo"]
        } else {
            vec!["--replace-rtk", "--project"]
        };
        let p = stock_plan(&f, &args).unwrap();
        assert!(p.changes.is_empty());
        assert!(p.messages.join(" ").contains("manual migration required"));
        p.apply().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), SYNTHETIC_AWARENESS);
        assert!(!f.roots.project.join(".kilo").exists());
        // A directory alone does not establish a compatible host version either.
        fs::create_dir(f.roots.project.join(".kilo")).unwrap();
        assert!(stock_plan(&f, &args).unwrap().changes.is_empty());
    }
}

#[test]
fn unchanged_plugins_remain_owned_after_executable_moves_but_code_edits_do_not() {
    for (host, relative) in [
        ("pi", ".pi/agent/extensions/retok.ts"),
        ("omp", ".omp/agent/extensions/retok.ts"),
        ("opencode", ".config/opencode/plugins/retok.ts"),
        ("kilo", ".config/kilo/plugin/retok.ts"),
    ] {
        let mut f = Fixture::new();
        f.roots.executable = f.root.join("install A ' __RETOK_EXECUTABLE__/retok");
        f.plan(&["--agent", host]).apply().unwrap();
        let path = f.roots.home.join(relative);
        let old = fs::read(&path).unwrap();
        f.roots.executable = f.root.join("install B ' __RETOK_SOURCE__/retok");
        let status = f.plan(&["--agent", host, "--show"]);
        assert!(status.messages.iter().any(|m| m.contains("; installed;")));
        let upgrade = f.plan(&["--agent", host]);
        assert_eq!(upgrade.changes.len(), 1);
        let backups = upgrade.apply().unwrap();
        assert_eq!(fs::read(&backups[0]).unwrap(), old);
        assert!(fs::read_to_string(&path).unwrap().contains("install B"));
        assert!(f.plan(&["--agent", host]).changes.is_empty());
        f.roots.executable = f.root.join("install C/retok");
        f.plan(&["--agent", host, "--uninstall"]).apply().unwrap();
        assert!(!path.exists());
        f.plan(&["--agent", host]).apply().unwrap();
        let edited = fs::read_to_string(&path).unwrap().replace("3000", "4000");
        fs::write(&path, &edited).unwrap();
        f.roots.executable = f.root.join("install D/retok");
        assert!(plan(&["--agent".into(), host.into()], &f.roots).is_err());
        assert!(f.plan(&["--agent", host, "--uninstall"]).changes.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    }
}

#[test]
fn legacy_script_path_requires_exact_stock_bytes_not_just_a_stock_filename() {
    for body in [
        None,
        Some("#!/bin/sh\ncustom-policy-check\n"),
        Some("#!/bin/sh\n# Synthetic owned migration script.\n# modified\n"),
    ] {
        let f = Fixture::new();
        if let Some(body) = body {
            f.write(".claude/hooks/rtk-rewrite.sh", body);
        }
        let original = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"command":"rtk hook claude"},{"command":"bash ~/.claude/hooks/rtk-rewrite.sh"}]}]}}"#;
        let path = f.write(".claude/settings.json", original);
        let error = stock_plan(&f, &["--replace-rtk"]).unwrap_err();
        assert!(error.to_string().contains("manual migration required"));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        if let Some(body) = body {
            assert_eq!(
                fs::read_to_string(f.roots.home.join(".claude/hooks/rtk-rewrite.sh")).unwrap(),
                body
            );
        }
    }
    let f = Fixture::new();
    let script = f.write(".claude/hooks/rtk-rewrite.sh", SYNTHETIC_SCRIPT);
    let json = json!({"hooks":{"PreToolUse":[{"hooks":[{"command":format!("bash \"{}\"",script.display())},{"command":"audit"}]}]}});
    let path = f.write(
        ".claude/settings.json",
        &serde_json::to_string(&json).unwrap(),
    );
    stock_plan(&f, &["--replace-rtk"]).unwrap().apply().unwrap();
    assert_eq!(
        value(&path)["hooks"]["PreToolUse"][0]["hooks"],
        json!([{"command":"audit"}])
    );
    assert_eq!(fs::read_to_string(script).unwrap(), SYNTHETIC_SCRIPT);
}

#[test]
fn project_migration_does_not_assume_a_tilde_script_is_project_local() {
    let f = Fixture::new();
    f.write(".claude/hooks/rtk-rewrite.sh", SYNTHETIC_SCRIPT);
    let settings = f.roots.project.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let original =
        r#"{"hooks":{"PreToolUse":[{"command":"bash ~/.claude/hooks/rtk-rewrite.sh"}]}}"#;
    fs::write(&settings, original).unwrap();
    let error = stock_plan(&f, &["--replace-rtk", "--project"]).unwrap_err();
    assert!(error.to_string().contains("another scope/home"));
    assert_eq!(fs::read_to_string(settings).unwrap(), original);
}

#[test]
fn kimi_and_hermes_global_guidance_honor_explicit_roots_and_preserve_user_text() {
    for (host, file) in [
        ("kimi", "AGENTS.md"),
        ("hermes", "SOUL.md"),
        ("vibe", "AGENTS.md"),
    ] {
        let mut f = Fixture::new();
        let relocated = f.root.join(format!("relocated-{host}"));
        fs::create_dir(&relocated).unwrap();
        if host == "kimi" {
            f.roots.kimi = relocated.clone();
        } else if host == "hermes" {
            f.roots.hermes = relocated.clone();
        } else {
            f.roots.vibe = relocated.clone();
        }
        let path = relocated.join(file);
        fs::write(&path, "Existing user guidance.\n").unwrap();
        let p = f.plan(&[]);
        assert!(p.messages.join(" ").contains("instructions only"));
        p.apply().unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("Existing user guidance.\n<!-- retok managed")
        );
        assert!(f.plan(&["--agent", host]).changes.is_empty());
        f.plan(&["--agent", host, "--uninstall"]).apply().unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "Existing user guidance.\n"
        );
        assert!(!f.roots.home.join(".kimi-code").exists());
        assert!(!f.roots.home.join(".hermes").exists());
    }
}

#[test]
fn project_guidance_uses_hermes_precedence_and_does_not_invent_detected_hosts() {
    for existing in [
        vec![".hermes.md", "HERMES.md", "AGENTS.md"],
        vec!["HERMES.md", "AGENTS.md"],
        vec!["AGENTS.md"],
        vec![],
    ] {
        let f = Fixture::new();
        for name in &existing {
            fs::write(f.roots.project.join(name), "Keep user rules.\n").unwrap();
        }
        let chosen = existing.first().copied().unwrap_or("AGENTS.md");
        f.plan(&["--agent", "hermes", "--project"]).apply().unwrap();
        assert!(
            fs::read_to_string(f.roots.project.join(chosen))
                .unwrap()
                .contains("retok run")
        );
        for name in existing.iter().skip(1) {
            assert_eq!(
                fs::read_to_string(f.roots.project.join(name)).unwrap(),
                "Keep user rules.\n"
            );
        }
        assert!(!f.roots.hermes.exists());
    }
    let f = Fixture::new();
    fs::write(f.roots.project.join("AGENTS.md"), "Shared user rules.\n").unwrap();
    assert!(f.plan(&["--project"]).changes.is_empty());
    f.plan(&["--project", "--agent", "kimi"]).apply().unwrap();
    assert!(
        fs::read_to_string(f.roots.project.join("AGENTS.md"))
            .unwrap()
            .contains("retok run")
    );
    assert!(!f.roots.kimi.exists());
}

#[test]
fn hermes_rtk_plugin_and_config_stay_unchanged_with_manual_migration_notice() {
    let f = Fixture::new();
    let plugin = f.write(
        ".hermes/plugins/rtk-rewrite/__init__.py",
        "# Existing user plugin\n",
    );
    let config = f.write(
        ".hermes/config.yaml",
        "plugins:\n  enabled: [rtk-rewrite, user-plugin]\n",
    );
    let p = f.plan(&["--replace-rtk"]);
    assert!(p.changes.is_empty());
    assert!(p.messages.join(" ").contains("manual migration required"));
    assert_eq!(
        fs::read_to_string(plugin).unwrap(),
        "# Existing user plugin\n"
    );
    assert_eq!(
        fs::read_to_string(config).unwrap(),
        "plugins:\n  enabled: [rtk-rewrite, user-plugin]\n"
    );
    assert!(!f.roots.hermes.join("SOUL.md").exists());
}

#[test]
fn custom_rtk_script_does_not_block_removing_only_retok_entries() {
    let f = Fixture::new();
    f.plan(&["--agent", "claude"]).apply().unwrap();
    let path = f.roots.home.join(".claude/settings.json");
    let mut v = value(&path);
    v["hooks"]["PreToolUse"] = json!([{"command":"bash ~/.claude/hooks/rtk-rewrite.sh"}]);
    fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    let script = f.write(
        ".claude/hooks/rtk-rewrite.sh",
        "#!/bin/sh\ncustom-policy-check\n",
    );
    let p = f.plan(&["--agent", "claude", "--uninstall"]);
    assert!(p.messages.join(" ").contains("left unchanged"));
    p.apply().unwrap();
    let v = value(&path);
    assert_eq!(v["hooks"]["PostToolUse"], json!([]));
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["command"],
        "bash ~/.claude/hooks/rtk-rewrite.sh"
    );
    assert_eq!(
        fs::read_to_string(script).unwrap(),
        "#!/bin/sh\ncustom-policy-check\n"
    );
}

#[test]
fn vibe_guidance_stays_in_requested_scope_and_toml_migration_is_manual() {
    let f = Fixture::new();
    let hooks = f.write(
        ".vibe/hooks.toml",
        "[[hooks]]\nname = 'rtk-rewrite'\ncommand = 'rtk hook vibe'\n",
    );
    let p = f.plan(&["--replace-rtk"]);
    assert!(p.changes.is_empty());
    assert!(p.messages.join(" ").contains("manual review"));
    assert_eq!(
        fs::read_to_string(hooks).unwrap(),
        "[[hooks]]\nname = 'rtk-rewrite'\ncommand = 'rtk hook vibe'\n"
    );
    assert!(!f.roots.vibe.join("AGENTS.md").exists());
    assert!(f.plan(&["--project"]).changes.is_empty());
    f.plan(&["--agent", "vibe", "--project"]).apply().unwrap();
    let path = f.roots.project.join("AGENTS.md");
    assert!(fs::read_to_string(&path).unwrap().contains("retok run"));
    assert!(!f.roots.vibe.join("AGENTS.md").exists());
    assert!(f.plan(&["--agent", "vibe", "--project"]).changes.is_empty());
    f.plan(&["--agent", "vibe", "--project", "--uninstall"])
        .apply()
        .unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "");
}

#[test]
fn mixed_custom_rtk_entry_does_not_block_doctor_init_or_retok_uninstall() {
    use std::process::Command;
    let f = Fixture::new();
    let original = r#"{ "version":1, "hooks":{"PreToolUse":[{"type":"command","bash":"rtk hook copilot","powershell":"custom-policy-check"}]}}"#;
    let path = f.write(".copilot/hooks/rtk-rewrite.json", original);
    let result = Command::new(env!("CARGO_BIN_EXE_retok"))
        .args(["doctor", "--agent", "copilot"])
        .env("HOME", &f.roots.home)
        .env("USERPROFILE", &f.roots.home)
        .env("COPILOT_HOME", &f.roots.copilot)
        .env("XDG_CONFIG_HOME", &f.roots.config)
        .current_dir(&f.roots.project)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let status = String::from_utf8(result.stdout).unwrap();
    assert!(status.contains("manual migration required"));
    assert!(status.contains("left unchanged"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert!(!f.roots.copilot.join("hooks/retok.json").exists());

    let install = f.plan(&["--agent", "copilot"]);
    assert!(
        install
            .messages
            .join(" ")
            .contains("manual migration required")
    );
    install.apply().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    let owned = f.roots.copilot.join("hooks/retok.json");
    assert_eq!(
        value(&owned)["hooks"]["postToolUse"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let uninstall = f.plan(&["--agent", "copilot", "--uninstall"]);
    assert!(
        uninstall
            .messages
            .join(" ")
            .contains("manual migration required")
    );
    uninstall.apply().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(value(&owned)["hooks"]["postToolUse"], json!([]));
    assert!(
        f.plan(&["--agent", "copilot", "--uninstall"])
            .changes
            .is_empty()
    );
    assert!(
        plan(
            &["--agent".into(), "copilot".into(), "--replace-rtk".into()],
            &f.roots
        )
        .is_err()
    );
}

#[test]
fn native_hooks_upgrade_and_uninstall_after_executable_relocation() {
    for host in ["claude", "copilot"] {
        for update_first in [false, true] {
            let mut f = Fixture::new();
            f.roots.executable = f.root.join("install A ' $HOME;/retok");
            f.plan(&["--agent", host]).apply().unwrap();
            let path = if host == "claude" {
                f.roots.home.join(".claude/settings.json")
            } else {
                f.roots.copilot.join("hooks/retok.json")
            };
            let original = fs::read(&path).unwrap();
            f.roots.executable = f.root.join("install B ' $HOME;/retok");
            let status = f.plan(&["--agent", host, "--show"]);
            assert!(status.messages.iter().any(|m| m.contains("; installed;")));
            if update_first {
                let p = f.plan(&["--agent", host]);
                assert_eq!(p.changes.len(), 1);
                let backups = p.apply().unwrap();
                assert_eq!(fs::read(&backups[0]).unwrap(), original);
                let v = value(&path);
                let event = if host == "claude" {
                    "PostToolUse"
                } else {
                    "postToolUse"
                };
                assert_eq!(v["hooks"][event].as_array().unwrap().len(), 1);
                assert!(!fs::read_to_string(&path).unwrap().contains("install A"));
                assert!(fs::read_to_string(&path).unwrap().contains("install B"));
                assert!(f.plan(&["--agent", host]).changes.is_empty());
                f.roots.executable = f.root.join("install C/retok");
            }
            f.plan(&["--agent", host, "--uninstall"]).apply().unwrap();
            let event = if host == "claude" {
                "PostToolUse"
            } else {
                "postToolUse"
            };
            assert_eq!(value(&path)["hooks"][event], json!([]));
        }
    }
}

#[test]
fn native_relocation_keeps_custom_invocations_and_changed_generated_entries() {
    for host in ["claude", "copilot"] {
        for mixed in [false, true] {
            let mut f = Fixture::new();
            f.roots.executable = f.root.join("install A/retok");
            f.plan(&["--agent", host]).apply().unwrap();
            let path = if host == "claude" {
                f.roots.home.join(".claude/settings.json")
            } else {
                f.roots.copilot.join("hooks/retok.json")
            };
            let event = if host == "claude" {
                "PostToolUse"
            } else {
                "postToolUse"
            };
            let mut v = value(&path);
            let entry = if host == "claude" {
                &mut v["hooks"][event][0]["hooks"][0]
            } else {
                &mut v["hooks"][event][0]
            };
            if mixed {
                entry["powershell"] = json!("custom-policy-check");
            } else {
                entry["env"] = json!({"USER_CUSTOMIZATION":"preserve"});
            }
            let custom = entry.clone();
            fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
            f.roots.executable = f.root.join("install B/retok");
            f.plan(&["--agent", host]).apply().unwrap();
            f.plan(&["--agent", host, "--uninstall"]).apply().unwrap();
            let v = value(&path);
            assert_eq!(v["hooks"][event].as_array().unwrap().len(), 1);
            let left = if host == "claude" {
                &v["hooks"][event][0]["hooks"][0]
            } else {
                &v["hooks"][event][0]
            };
            assert_eq!(left, &custom);
        }
    }
}

#[test]
fn openclaw_is_explicit_workspace_guidance_only_and_preserves_rtk_plugin() {
    let f = Fixture::new();
    let path = f.roots.project.join("AGENTS.md");
    fs::write(&path, "Workspace rules.\n").unwrap();
    assert!(f.plan(&["--project"]).changes.is_empty());
    let p = f.plan(&["--agent", "openclaw", "--project"]);
    assert_eq!(p.changes.len(), 1);
    assert_eq!(p.changes[0].path, path);
    assert!(p.messages.join(" ").contains("instructions only"));
    p.apply().unwrap();
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .starts_with("Workspace rules.\n<!-- retok managed")
    );
    assert!(
        f.plan(&["--agent", "openclaw", "--project"])
            .changes
            .is_empty()
    );
    assert!(!f.roots.home.join(".openclaw").exists());
    f.plan(&["--agent", "openclaw", "--project", "--uninstall"])
        .apply()
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "Workspace rules.\n");

    let plugin = f.write(
        ".openclaw/extensions/rtk-rewrite/index.ts",
        "// Existing RTK plugin\n",
    );
    let config = f.write(
        ".openclaw/openclaw.json",
        "// Existing JSON5 configuration; not parsed by setup\n{}",
    );
    for args in [
        vec!["--agent", "openclaw"],
        vec!["--agent", "openclaw", "--replace-rtk"],
        vec!["--agent", "openclaw", "--show"],
    ] {
        let p = f.plan(&args);
        assert!(p.changes.is_empty());
        assert!(
            p.messages
                .join(" ")
                .contains("run --project in OpenClaw workspace")
        );
        assert!(p.messages.join(" ").contains("manual migration required"));
    }
    assert_eq!(
        fs::read_to_string(plugin).unwrap(),
        "// Existing RTK plugin\n"
    );
    assert_eq!(
        fs::read_to_string(config).unwrap(),
        "// Existing JSON5 configuration; not parsed by setup\n{}"
    );
    assert!(!f.roots.home.join(".openclaw/AGENTS.md").exists());
    assert!(!f.roots.home.join(".openclaw/workspace").exists());
}

#[test]
fn instruction_guidance_names_absolute_executable_and_survives_relocation() {
    for (host, project, relative, dedicated) in [
        ("codex", false, ".codex/AGENTS.md", false),
        ("cursor", true, ".cursor/rules/retok.mdc", true),
        ("windsurf", true, ".windsurf/rules/retok.md", true),
        ("openclaw", true, "AGENTS.md", false),
    ] {
        for update_first in [false, true] {
            let mut f = Fixture::new();
            f.roots.executable = f.root.join("install A ' 日本 $HOME/retok");
            let path = if project {
                f.roots.project.join(relative)
            } else {
                f.roots.home.join(relative)
            };
            if !dedicated {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, "User rules.\n").unwrap();
            }
            let mut args = vec!["--agent", host];
            if project {
                args.push("--project");
            }
            f.plan(&args).apply().unwrap();
            let text = fs::read_to_string(&path).unwrap();
            let literal = text
                .lines()
                .find_map(|line| line.strip_prefix("Installed executable (JSON string): "))
                .unwrap();
            let executable: String = serde_json::from_str(literal).unwrap();
            assert_eq!(executable, f.roots.executable.to_str().unwrap());
            assert!(text.contains("If retok is not on PATH"));
            assert!(text.contains("`retok run -- COMMAND ARG...`"));
            if !dedicated {
                fs::write(&path, format!("{text}Later user guidance.\n")).unwrap();
            }
            let original = fs::read(&path).unwrap();
            f.roots.executable = f.root.join("install B ' 日本 $HOME/retok");
            let mut status = args.clone();
            status.push("--show");
            assert!(
                f.plan(&status)
                    .messages
                    .iter()
                    .any(|m| m.contains("; installed;"))
            );
            if update_first {
                let p = f.plan(&args);
                assert_eq!(p.changes.len(), 1);
                let backups = p.apply().unwrap();
                assert_eq!(fs::read(&backups[0]).unwrap(), original);
                let text = fs::read_to_string(&path).unwrap();
                assert!(!text.contains("install A"));
                assert!(text.contains("install B"));
                assert!(f.plan(&args).changes.is_empty());
                f.roots.executable = f.root.join("install C/retok");
            }
            let mut uninstall = args.clone();
            uninstall.push("--uninstall");
            f.plan(&uninstall).apply().unwrap();
            if dedicated {
                assert!(!path.exists());
            } else {
                assert_eq!(
                    fs::read_to_string(&path).unwrap(),
                    "User rules.\nLater user guidance.\n"
                );
            }
        }
    }
}

#[test]
fn relocated_instruction_ownership_does_not_accept_custom_block_or_frontmatter_edits() {
    for host in ["codex", "cursor"] {
        let mut f = Fixture::new();
        let args = vec!["--agent", host, "--project"];
        f.plan(&args).apply().unwrap();
        let path = if host == "codex" {
            f.roots.project.join("AGENTS.md")
        } else {
            f.roots.project.join(".cursor/rules/retok.mdc")
        };
        let original = fs::read_to_string(&path).unwrap();
        let changed = if host == "codex" {
            original.replace("literal path quoting", "custom user instructions")
        } else {
            original.replace("alwaysApply: true", "alwaysApply: false")
        };
        assert_ne!(original, changed);
        fs::write(&path, &changed).unwrap();
        f.roots.executable = f.root.join("relocated/retok");
        assert!(
            plan(
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
                &f.roots
            )
            .is_err()
        );
        let mut uninstall = args.clone();
        uninstall.push("--uninstall");
        assert!(f.plan(&uninstall).changes.is_empty());
        assert_eq!(fs::read_to_string(path).unwrap(), changed);
    }
}
