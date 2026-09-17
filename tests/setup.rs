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

#[cfg(not(windows))]
#[test]
fn codex_migration_installs_prehook_and_project_stays_local() {
    let f = Fixture::new();
    let global = f.write(".codex/hooks.json", r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"command":"rtk hook codex"},{"command":"audit"}]}]}}"#);
    let before = fs::read(&global).unwrap();
    fs::create_dir_all(f.roots.project.join(".codex")).unwrap();
    fs::write(f.roots.project.join(".codex/hooks.json"), &before).unwrap();
    let p = f.plan(&["--agent", "codex", "--project", "--replace-rtk"]);
    assert!(p.messages.join(" ").contains("native pre-execution hook"));
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
fn fresh_shared_copilot_installs_only_the_qualified_cli_completion_route() {
    for agent in ["copilot", "vscode"] {
        let mut f = Fixture::new();
        f.roots.executable = std::env::current_exe().unwrap();
        let p = f.plan(&["--agent", agent]);
        assert!(p.messages.join(" ").contains("native rewrite unavailable"));
        p.apply().unwrap();
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
        assert!(f.plan(&["--agent", agent]).changes.is_empty());
        let doctor = f.plan(&["--agent", agent, "--show"]).messages.join(" ");
        assert!(doctor.contains("CLI post-output configured"));
        assert!(doctor.contains("VS Code native rewrite unavailable"));
    }
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
        let bad_path = f.write(".copilot/hooks/bad.json", bad);
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
    let b = f.write(".copilot/hooks/retok.json", "{}");
    let p = f.plan(&[]);
    fs::write(&b, "{\"user\":true}").unwrap();
    assert!(p.apply().is_err());
    assert_eq!(fs::read_to_string(a).unwrap(), "{}");
    assert_eq!(fs::read_to_string(b).unwrap(), "{\"user\":true}");
}

#[cfg(unix)]
#[test]
fn symlink_managed_files_preserve_links_back_up_targets_and_reject_retargeting() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let original = b"{\"user\":42}\n";
    let target = f.write(
        "dotfiles/settings.json",
        std::str::from_utf8(original).unwrap(),
    );
    fs::create_dir_all(f.roots.home.join(".claude")).unwrap();
    let path = f.roots.home.join(".claude/settings.json");
    symlink("../dotfiles/settings.json", &path).unwrap();
    let backups = f.plan(&["--agent", "claude"]).apply().unwrap();
    assert_eq!(
        fs::read_link(&path).unwrap(),
        Path::new("../dotfiles/settings.json")
    );
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].parent(), target.parent());
    assert_eq!(fs::read(&backups[0]).unwrap(), original);
    assert_eq!(value(&target)["user"], 42);
    assert!(f.plan(&["--agent", "claude"]).changes.is_empty());
    let removal = f.plan(&["--agent", "claude", "--uninstall"]);
    let same_bytes = f.write("other.json", &fs::read_to_string(&target).unwrap());
    fs::remove_file(&path).unwrap();
    symlink(&same_bytes, &path).unwrap();
    assert!(removal.apply().is_err());
    assert_eq!(fs::read(&target).unwrap(), fs::read(&same_bytes).unwrap());
    fs::remove_file(&path).unwrap();
    fs::remove_dir(f.roots.home.join(".claude")).unwrap();
    symlink(target.parent().unwrap(), f.roots.home.join(".claude")).unwrap();
    f.plan(&["--agent", "claude", "--uninstall"])
        .apply()
        .unwrap();
    assert!(
        fs::symlink_metadata(f.roots.home.join(".claude"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(value(&target)["hooks"]["PostToolUse"], json!([]));
    fs::remove_file(&target).unwrap();
    symlink("missing.json", &target).unwrap();
    assert!(plan(&["--agent".into(), "claude".into()], &f.roots).is_err());
}

#[test]
fn instructions_remove_only_unchanged_owned_block() {
    let f = Fixture::new();
    let path = f.write(".gemini/GEMINI.md", "Keep these instructions.\n");
    f.plan(&["--instructions-only", "--agent", "gemini"])
        .apply()
        .unwrap();
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("Later user guidance.\n");
    fs::write(&path, text).unwrap();
    f.plan(&["--instructions-only", "--agent", "gemini", "--uninstall"])
        .apply()
        .unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "Keep these instructions.\nLater user guidance.\n"
    );
    f.plan(&["--instructions-only", "--agent", "gemini"])
        .apply()
        .unwrap();
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("suitable", "approved");
    fs::write(&path, &text).unwrap();
    assert!(
        f.plan(&["--instructions-only", "--agent", "gemini", "--uninstall"])
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

#[cfg(unix)]
#[test]
fn symlink_plugin_removal_preserves_targets_and_repeats_uninstall_and_install() {
    use std::os::unix::fs::symlink;
    for layout in ["regular", "file-link", "directory-link"] {
        let f = Fixture::new();
        f.plan(&["--agent", "pi"]).apply().unwrap();
        let plugin = f.roots.pi.join("extensions/retok.ts");
        let original = fs::read(&plugin).unwrap();
        let target = f.roots.home.join("dotfiles/retok.ts");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        if layout == "file-link" {
            fs::rename(&plugin, &target).unwrap();
            symlink("../../../dotfiles/retok.ts", &plugin).unwrap();
        } else if layout == "directory-link" {
            let dir = plugin.parent().unwrap();
            fs::remove_dir(target.parent().unwrap()).unwrap();
            fs::rename(dir, target.parent().unwrap()).unwrap();
            symlink(target.parent().unwrap(), dir).unwrap();
        }
        let backups = f.plan(&["--agent", "pi", "--uninstall"]).apply().unwrap();
        assert!(backups.iter().any(|p| fs::read(p).unwrap() == original));
        assert!(fs::symlink_metadata(&plugin).is_err());
        if layout == "file-link" {
            assert_eq!(fs::read(&target).unwrap(), original);
        } else {
            assert!(!target.exists());
        }
        assert!(f.plan(&["--agent", "pi", "--uninstall"]).changes.is_empty());
        f.plan(&["--agent", "pi"]).apply().unwrap();
        assert_eq!(fs::read(&plugin).unwrap(), original);
        assert!(f.plan(&["--agent", "pi"]).changes.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn symlink_stock_plugin_migration_preserves_target_and_repeats() {
    use std::os::unix::fs::symlink;
    for linked in [false, true] {
        let f = Fixture::new();
        let plugin = f.write(".pi/agent/extensions/rtk.ts", SYNTHETIC_PI);
        let target = f.roots.home.join("stock-plugin.ts");
        if linked {
            fs::rename(&plugin, &target).unwrap();
            symlink(&target, &plugin).unwrap();
        }
        stock_plan(&f, &["--replace-rtk"]).unwrap().apply().unwrap();
        assert!(fs::symlink_metadata(&plugin).is_err());
        if linked {
            assert_eq!(fs::read_to_string(&target).unwrap(), SYNTHETIC_PI);
        }
        assert!(
            stock_plan(&f, &["--replace-rtk"])
                .unwrap()
                .changes
                .is_empty()
        );
        assert!(f.plan(&["--agent", "pi"]).changes.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn symlink_removal_checks_link_identity_and_rolls_back_on_later_failure() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.plan(&["--agent", "pi"]).apply().unwrap();
    f.plan(&["--agent", "omp"]).apply().unwrap();
    let pi = f.roots.pi.join("extensions/retok.ts");
    let omp = f.roots.omp.join("extensions/retok.ts");
    let target = f.roots.home.join("plugin.ts");
    fs::rename(&pi, &target).unwrap();
    let relative = Path::new("../../../plugin.ts");
    symlink(relative, &pi).unwrap();
    let p = f.plan(&["--agent", "pi", "--uninstall"]);
    fs::remove_file(&pi).unwrap();
    symlink(&target, &pi).unwrap();
    assert!(
        p.apply().is_err(),
        "same target but different link must be detected"
    );
    fs::remove_file(&pi).unwrap();
    symlink(relative, &pi).unwrap();
    // The second backup name exceeds NAME_MAX, failing even for privileged runners.
    let long_target = f.roots.home.join("x".repeat(240));
    fs::rename(&omp, &long_target).unwrap();
    symlink(&long_target, &omp).unwrap();
    let original = fs::read(&target).unwrap();
    let mut p = f.plan(&["--agent", "pi", "--uninstall"]);
    p.changes
        .extend(f.plan(&["--agent", "omp", "--uninstall"]).changes);
    assert!(p.apply().is_err());
    assert_eq!(fs::read_link(&pi).unwrap(), relative);
    assert_eq!(fs::read(&target).unwrap(), original);
    assert_eq!(fs::read_link(&omp).unwrap(), long_target);
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

#[cfg(not(windows))]
#[test]
fn codex_absolute_stock_import_is_replaced_in_one_backed_up_write() {
    let f = Fixture::new();
    let dedicated = f.write(".codex/RTK.md", SYNTHETIC_AWARENESS);
    let text = format!("User instructions.\n@{}\n", dedicated.display());
    let shared = f.write(".codex/AGENTS.md", &text);
    let p = stock_plan(&f, &["--replace-rtk"]).unwrap();
    assert_eq!(p.changes.len(), 3);
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
    assert!(f.roots.codex.join("hooks.json").exists());
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
        stock_plan(
            &f,
            &["--replace-rtk", "--agent", "codex", "--instructions-only"]
        )
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
    f.plan(&["--instructions-only", "--agent", "cursor", "--project"])
        .apply()
        .unwrap();
    let rule = f.roots.project.join(".cursor/rules/retok.mdc");
    let bytes = fs::read_to_string(&rule).unwrap();
    assert!(bytes.starts_with("---\ndescription:"));
    assert!(bytes.contains("\nalwaysApply: true\n---\n"));
    assert!(
        f.plan(&["--instructions-only", "--agent", "cursor", "--project"])
            .changes
            .is_empty()
    );
    f.plan(&[
        "--instructions-only",
        "--agent",
        "cursor",
        "--project",
        "--uninstall",
    ])
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

#[cfg(not(windows))]
#[test]
fn gemini_stock_script_and_awareness_downgrade_only_when_explicitly_selected() {
    let f = Fixture::new();
    let script = f.write(".gemini/hooks/rtk-hook-gemini.sh", SYNTHETIC_SCRIPT);
    let settings = json!({"hooks":{"BeforeTool":[{"matcher":"run_shell_command","hooks":[{"type":"command","command":script.to_str().unwrap()},{"command":format!("echo {}",script.display())}]}]}});
    let path = f.write(
        ".gemini/settings.json",
        &serde_json::to_string(&settings).unwrap(),
    );
    let instructions = f.write(".gemini/GEMINI.md", SYNTHETIC_AWARENESS);
    stock_plan(
        &f,
        &["--agent", "gemini", "--replace-rtk", "--instructions-only"],
    )
    .unwrap()
    .apply()
    .unwrap();
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
        stock_plan(
            &f,
            &["--agent", "gemini", "--replace-rtk", "--instructions-only"]
        )
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
    f.plan(&["--agent", "copilot", "--replace-rtk", "--instructions-only"])
        .apply()
        .unwrap();
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
        assert!(
            status
                .messages
                .iter()
                .any(|m| m.contains("; not fully configured;"))
        );
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
        if cfg!(windows) && host == "vibe" {
            continue;
        }
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
        assert!(p.messages.join(" ").contains(if host == "vibe" {
            "native pre-execution hook"
        } else if host == "hermes" {
            "native post-output plugin"
        } else {
            "instructions only"
        }));
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

#[cfg(not(windows))]
#[test]
fn vibe_project_native_setup_stays_local_and_custom_migration_is_manual() {
    let f = Fixture::new();
    let hooks = f.write(
        ".vibe/hooks.toml",
        "[[hooks]]\nname = 'rtk-rewrite'\ncommand = 'rtk hook vibe'\n",
    );
    let error = plan(&["--replace-rtk".into()], &f.roots).unwrap_err();
    assert!(error.to_string().contains("manual migration"));
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
            assert!(
                status
                    .messages
                    .iter()
                    .any(|m| m.contains(if host == "copilot" {
                        "CLI post-output not configured"
                    } else {
                        "; not fully configured;"
                    }))
            );
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
        let status = p.messages.join(" ");
        assert!(status.contains("RTK plugin"), "{status}");
        if args.contains(&"--replace-rtk") {
            assert!(status.contains("manual migration required"));
        }
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
            let mut args = vec!["--instructions-only", "--agent", host];
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
                    .any(|m| m.contains("; configured;"))
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
        let args = vec!["--instructions-only", "--agent", host, "--project"];
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

fn isolated_cli(f: &Fixture) -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_retok"));
    command
        .current_dir(&f.roots.project)
        .env("HOME", &f.roots.home)
        .env("USERPROFILE", &f.roots.home)
        .env("XDG_CONFIG_HOME", &f.roots.config)
        .env("RETOK_CONFIG_DIR", f.root.join("retok-config"))
        .env("RETOK_STATE_DIR", f.root.join("retok-state"));
    for name in [
        "CLAUDE_CONFIG_DIR",
        "PI_CODING_AGENT_DIR",
        "CODEX_HOME",
        "COPILOT_HOME",
        "FACTORY_HOME_OVERRIDE",
        "KIMI_CODE_HOME",
        "HERMES_HOME",
        "VIBE_HOME",
        "OPENCLAW_STATE_DIR",
        "OPENCLAW_CONFIG_PATH",
    ] {
        command.env_remove(name);
    }
    command
}

#[test]
fn relocated_environment_roots_reach_active_hosts_without_leaking_project_scope() {
    let f = Fixture::new();
    for (agent, variable, suffix) in [
        ("claude", "CLAUDE_CONFIG_DIR", "settings.json"),
        ("pi", "PI_CODING_AGENT_DIR", "extensions/retok.ts"),
        ("omp", "PI_CODING_AGENT_DIR", "extensions/retok.ts"),
        ("droid", "FACTORY_HOME_OVERRIDE", ".factory/AGENTS.md"),
    ] {
        let relocated = f.root.join(format!("relocated-{agent}"));
        let result = isolated_cli(&f)
            .env(variable, &relocated)
            .args(["init", "--agent", agent])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(relocated.join(suffix).is_file(), "{agent}");
        assert!(!f.roots.home.join(format!(".{agent}")).exists());
        let before = fs::read(relocated.join(suffix)).unwrap();
        let result = isolated_cli(&f)
            .env(variable, &relocated)
            .args(["init", "--agent", agent, "--project"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read(relocated.join(suffix)).unwrap(), before);
        assert!(
            f.roots.project.join(format!(".{agent}")).is_dir()
                || agent == "droid" && f.roots.project.join("AGENTS.md").is_file()
        );
    }
}

#[test]
fn relocated_pi_and_omp_share_one_owned_plugin_without_conflicting_plans() {
    let mut f = Fixture::new();
    f.roots.pi = f.root.join("shared-agent");
    f.roots.omp = f.roots.pi.clone();
    fs::create_dir(&f.roots.pi).unwrap();
    let p = f.plan(&[]);
    assert_eq!(p.changes.len(), 1);
    p.apply().unwrap();
    assert!(f.plan(&[]).changes.is_empty());
    assert!(!f.roots.pi.join("agent").exists());
}

#[test]
fn bom_json_is_backed_up_exactly_preserved_and_still_rejects_duplicate_keys() {
    let f = Fixture::new();
    let original = "\u{feff}{\"permissions\":{\"allow\":[\"read\"]},\"custom\":1}\r\n";
    let path = f.write(".claude/settings.json", original);
    let p = f.plan(&["--agent", "claude"]);
    let backups = p.apply().unwrap();
    assert_eq!(fs::read(&backups[0]).unwrap(), original.as_bytes());
    let after = fs::read(&path).unwrap();
    assert!(after.starts_with(b"\xef\xbb\xbf"));
    let v: Value = serde_json::from_slice(&after[3..]).unwrap();
    assert_eq!(v["permissions"], json!({"allow":["read"]}));
    assert!(f.plan(&["--agent", "claude"]).changes.is_empty());
    fs::write(&path, "\u{feff}{\"hooks\":{},\"hooks\":{}}").unwrap();
    assert!(plan(&["--agent".into(), "claude".into()], &f.roots).is_err());
}

#[cfg(not(windows))]
#[test]
fn codex_native_json_schema_survives_repeat_and_uninstall() {
    let f = Fixture::new();
    f.plan(&["--agent", "codex"]).apply().unwrap();
    let path = f.roots.codex.join("hooks.json");
    let v = value(&path);
    assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "Bash");
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        setup::native_shell_command(f.roots.executable.to_str().unwrap(), "codex", false)
    );
    assert!(v.get("permissions").is_none());
    assert!(f.plan(&["--agent", "codex"]).changes.is_empty());
    f.plan(&["--agent", "codex", "--uninstall"])
        .apply()
        .unwrap();
    assert!(!fs::read_to_string(&path).unwrap().contains(" hook "));
}

#[cfg(not(windows))]
#[test]
fn unavailable_droid_rewrite_preserves_all_native_settings_and_adds_only_guidance() {
    for root_event in [false, true] {
        let f = Fixture::new();
        let settings = f.write(".factory/settings.json", r#"{"permissions":{"allow":["read"]},"hooks":{"PreToolUse":[{"matcher":"Execute","hooks":[{"command":"settings-audit"}]}]}}"#);
        let root = f.write(
            ".factory/hooks.json",
            if root_event {
                r#"{"PreToolUse":[{"matcher":"Execute","hooks":[{"command":"root-audit"}]}]}"#
            } else {
                r#"{"PostToolUse":[{"hooks":[{"command":"post-audit"}]}]}"#
            },
        );
        let legacy = f.write(
            ".factory/hooks/hooks.json",
            r#"{"PreToolUse":[{"hooks":[{"command":"legacy-audit"}]}]}"#,
        );
        let untouched = if root_event { &settings } else { &root };
        let original = fs::read(untouched).unwrap();
        let legacy_before = fs::read(&legacy).unwrap();
        f.plan(&["--agent", "droid"]).apply().unwrap();
        assert_eq!(fs::read(untouched).unwrap(), original);
        assert_eq!(fs::read(&legacy).unwrap(), legacy_before);
        assert_eq!(value(&settings)["permissions"], json!({"allow":["read"]}));
        let selected = if root_event {
            value(&root)
        } else {
            value(&settings)["hooks"].clone()
        };
        assert_eq!(selected["PreToolUse"].as_array().unwrap().len(), 1);
        assert!(f.roots.home.join(".factory/AGENTS.md").is_file());
        assert!(f.plan(&["--agent", "droid"]).changes.is_empty());
    }
}

#[test]
fn shared_copilot_migration_preserves_both_consumers_until_explicit_downgrade() {
    for agent in ["copilot", "vscode"] {
        let f = Fixture::new();
        let original = r#"{"version":1,"hooks":{"PreToolUse":[{"type":"command","command":"rtk hook copilot","cwd":".","timeout":5}]}}"#;
        let legacy = f.write(".copilot/hooks/rtk-rewrite.json", original);
        let output = f.write(".copilot/hooks/retok.json", "{broken}");
        assert!(
            plan(
                &["--agent".into(), agent.into(), "--replace-rtk".into()],
                &f.roots
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&legacy).unwrap(), original);
        fs::remove_file(&output).unwrap();
        for args in [
            vec!["--agent", agent],
            vec!["--agent", agent, "--replace-rtk"],
        ] {
            let p = f.plan(&args);
            assert!(p.changes.is_empty());
            assert!(
                p.messages
                    .join(" ")
                    .contains("Shared RTK Copilot activation preserved")
            );
            assert_eq!(fs::read_to_string(&legacy).unwrap(), original);
            assert!(!output.exists());
        }
        f.plan(&["--agent", agent, "--replace-rtk", "--instructions-only"])
            .apply()
            .unwrap();
        assert!(
            value(&legacy)["hooks"]["PreToolUse"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(f.roots.copilot.join("copilot-instructions.md").is_file());
    }
}

#[test]
fn unavailable_prehooks_preserve_rtk_and_fresh_setup_uses_only_guidance() {
    for (host, relative, body, guidance) in [
        (
            "cursor",
            ".cursor/hooks.json",
            r#"{"hooks":{"preToolUse":[{"command":"rtk hook cursor"}]}}"#,
            None,
        ),
        (
            "gemini",
            ".gemini/settings.json",
            r#"{"hooks":{"BeforeTool":[{"command":"rtk hook gemini"}]}}"#,
            Some(".gemini/GEMINI.md"),
        ),
        (
            "droid",
            ".factory/hooks.json",
            r#"{"PreToolUse":[{"command":"rtk hook droid"}]}"#,
            Some(".factory/AGENTS.md"),
        ),
    ] {
        let f = Fixture::new();
        let fresh = f.plan(&["--agent", host]);
        assert!(
            fresh
                .messages
                .join(" ")
                .contains("native rewrite unavailable")
        );
        fresh.apply().unwrap();
        assert!(!f.roots.home.join(relative).exists());
        if let Some(path) = guidance {
            assert!(f.roots.home.join(path).is_file());
        }
        assert!(f.plan(&["--agent", host]).changes.is_empty());
        let f = Fixture::new();
        let path = f.write(relative, body);
        let migration = f.plan(&["--agent", host, "--replace-rtk"]);
        assert!(migration.changes.is_empty());
        assert!(
            migration
                .messages
                .join(" ")
                .contains("existing automatic RTK hooks preserved")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
        let doctor = f.plan(&["--agent", host, "--show"]);
        assert!(doctor.changes.is_empty());
        assert!(
            doctor
                .messages
                .join(" ")
                .contains("native rewrite unavailable")
        );
        if guidance.is_some() {
            f.plan(&["--agent", host, "--replace-rtk", "--instructions-only"])
                .apply()
                .unwrap();
            assert!(!fs::read_to_string(&path).unwrap().contains("rtk hook"));
        }
    }
}

#[cfg(not(windows))]
#[test]
fn vibe_toml_migration_preserves_custom_tables_comments_and_bom() {
    let mut f = Fixture::new();
    let text = "\u{feff}# user header\ncustom = 'keep'\n\n[[hooks]]\nname = 'audit' # user comment\ntype = 'post_tool'\ncommand = 'custom-audit'\n\n[[hooks]]\nname = 'rtk-rewrite'\ntype = 'pre_tool'\nmatch = 'bash'\ncommand = 'rtk hook vibe'\nstrict = false\n";
    let path = f.write(".vibe/hooks.toml", text);
    let backups = f
        .plan(&["--agent", "vibe", "--replace-rtk"])
        .apply()
        .unwrap();
    assert_eq!(fs::read(&backups[0]).unwrap(), text.as_bytes());
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.starts_with("\u{feff}# user header\ncustom = 'keep'"));
    assert!(after.contains("name = 'audit' # user comment"));
    assert!(!after.contains("rtk hook vibe"));
    assert!(after.contains(" hook vibe"));
    assert!(f.plan(&["--agent", "vibe"]).changes.is_empty());
    f.roots.executable = f.root.join("new-install/retok");
    f.plan(&["--agent", "vibe"]).apply().unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains("new-install"));
    let changed = fs::read_to_string(&path)
        .unwrap()
        .replace("strict = false", "strict = true");
    fs::write(&path, &changed).unwrap();
    assert!(plan(&["--agent".into(), "vibe".into()], &f.roots).is_err());
    f.plan(&["--agent", "vibe", "--uninstall"]).apply().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
}

#[cfg(not(windows))]
#[test]
fn doctor_requires_correct_event_matcher_and_executable_not_just_guidance() {
    let mut f = Fixture::new();
    f.roots.executable = std::env::current_exe().unwrap();
    f.plan(&["--agent", "codex"]).apply().unwrap();
    let path = f.roots.codex.join("hooks.json");
    let good = value(&path);
    assert!(
        f.plan(&["--agent", "codex", "--show"])
            .messages
            .iter()
            .any(|m| m.contains("; configured;"))
    );
    for wrong_event in [false, true] {
        let mut bad = good.clone();
        if wrong_event {
            let entries = bad["hooks"]
                .as_object_mut()
                .unwrap()
                .remove("PreToolUse")
                .unwrap();
            bad["hooks"]["PostToolUse"] = entries;
        } else {
            bad["hooks"]["PreToolUse"][0]["matcher"] = json!("Read");
        }
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        let status = f.plan(&["--agent", "codex", "--show"]).messages.join("\n");
        assert!(status.contains("not fully configured"), "{status}");
        assert!(status.contains("event/matcher wrong"), "{status}");
    }
    fs::remove_file(&path).unwrap();
    assert!(
        f.plan(&["--agent", "codex", "--show"])
            .messages
            .join("\n")
            .contains("not fully configured")
    );
    f.roots.executable = f.root.join("missing/retok");
    f.plan(&["--agent", "codex"]).apply().unwrap();
    let status = f.plan(&["--agent", "codex", "--show"]).messages.join("\n");
    assert!(status.contains("executable missing"), "{status}");
    assert!(status.contains("host version, discovery, trust and runtime loading not probed"));
}

#[test]
fn doctor_reports_effective_disabled_and_invalid_retok_config_without_writes() {
    let f = Fixture::new();
    let config = f.root.join("retok-config/config.json");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    for (text, expected) in [
        ("{\"enabled\":false}", "Retok config: disabled"),
        ("broken", "Retok config: invalid or unreadable"),
    ] {
        fs::write(&config, text).unwrap();
        let result = isolated_cli(&f)
            .args(["doctor", "--agent", "codex"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            String::from_utf8_lossy(&result.stdout).contains(expected),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), text);
        assert!(!f.roots.codex.exists());
    }
}

#[test]
fn unsupported_automatic_migration_requires_explicit_instruction_downgrade() {
    let f = Fixture::new();
    let original = r#"{"hooks":{"pre_run_command":[{"command":"rtk hook windsurf"}]}}"#;
    let path = f.write(".codeium/windsurf/hooks.json", original);
    let p = f.plan(&["--agent", "windsurf", "--replace-rtk"]);
    assert!(p.changes.is_empty());
    assert!(p.messages.join(" ").contains("preserved"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    f.plan(&[
        "--agent",
        "windsurf",
        "--replace-rtk",
        "--instructions-only",
    ])
    .apply()
    .unwrap();
    assert_eq!(value(&path)["hooks"]["pre_run_command"], json!([]));
}

#[cfg(not(windows))]
#[test]
fn stock_project_codex_guidance_is_detected_without_a_codex_directory() {
    let f = Fixture::new();
    fs::write(f.roots.project.join("AGENTS.md"), "User rules.\n@RTK.md\n").unwrap();
    fs::write(f.roots.project.join("RTK.md"), SYNTHETIC_AWARENESS).unwrap();
    stock_plan(&f, &["--project", "--replace-rtk"])
        .unwrap()
        .apply()
        .unwrap();
    assert!(f.roots.project.join(".codex/hooks.json").exists());
    assert!(!f.roots.project.join("RTK.md").exists());
    assert!(
        !fs::read_to_string(f.roots.project.join("AGENTS.md"))
            .unwrap()
            .contains("@RTK.md")
    );
}

#[cfg(unix)]
#[test]
fn installed_prehook_commands_execute_real_cli_contracts_with_shared_consumer_separation() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut f = Fixture::new();
    f.roots.executable = f.root.join("bin ' quoted $literal/retok");
    fs::create_dir_all(f.roots.executable.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_retok"), &f.roots.executable).unwrap();
    let invoke = |command: &str, payload: &Value| {
        let mut child = Command::new("/bin/sh")
            .args(["-c", command])
            .current_dir(&f.roots.project)
            .env("HOME", &f.roots.home)
            .env("RETOK_CONFIG_DIR", f.root.join("retok-config"))
            .env("RETOK_STATE_DIR", f.root.join("retok-state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_string(payload).unwrap().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    for (host, file, event, tool, pointer) in [
        (
            "codex",
            ".codex/hooks.json",
            "PreToolUse",
            "Bash",
            "/hookSpecificOutput/updatedInput",
        ),
        (
            "vibe",
            ".vibe/hooks.toml",
            "pre_tool",
            "bash",
            "/hook_specific_output/tool_input",
        ),
    ] {
        f.plan(&["--agent", host]).apply().unwrap();
        let command = if host == "vibe" {
            let doc = fs::read_to_string(f.roots.home.join(file))
                .unwrap()
                .parse::<toml_edit::DocumentMut>()
                .unwrap();
            let command = doc["hooks"].as_array_of_tables().unwrap().get(0).unwrap()["command"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(!command.contains('\\'));
            command
        } else {
            let doc = value(&f.roots.home.join(file));
            let events = if host == "droid" { &doc } else { &doc["hooks"] };
            let entry = &events[event][0];
            entry
                .get("command")
                .unwrap_or(&entry["hooks"][0]["command"])
                .as_str()
                .unwrap()
                .to_owned()
        };
        let input = json!({"hook_event_name":event,"tool_name":tool,"tool_input":{"command":"git status --short 'synthetic file'","opaque":{"keep":17},"shell":"sh"}});
        let result = invoke(&command, &input);
        let updated = result
            .pointer(pointer)
            .unwrap_or_else(|| panic!("{host}: {result}"));
        assert_eq!(updated["opaque"], json!({"keep":17}));
        assert_ne!(
            updated["command"], input["tool_input"]["command"],
            "{host}: {result}"
        );
        assert!(updated["command"].as_str().unwrap().contains("retok"));
        assert!(
            updated["command"]
                .as_str()
                .unwrap()
                .contains("git status --short 'synthetic file'")
        );
    }
    f.plan(&["--agent", "copilot"]).apply().unwrap();
    let post = value(&f.roots.copilot.join("hooks/retok.json"))["hooks"]["postToolUse"][0].clone();
    let post_command =
        setup::native_shell_command(post["exec"].as_str().unwrap(), "copilot", false);
    let cli_output = json!({"toolName":"bash","toolResult":{"resultType":"success","textResultForLlm":"synthetic repeated line\n".repeat(100)}});
    assert!(
        invoke(&post_command, &cli_output)
            .get("modifiedResult")
            .is_some()
    );
    let vscode = json!({"hook_event_name":"PreToolUse","tool_name":"run_in_terminal","tool_input":{"command":"git status"}});
    assert_eq!(invoke(&post_command, &vscode), json!({}));
    f.plan(&["--agent", "vibe", "--project"]).apply().unwrap();
    let project_hook = f.roots.project.join(".vibe/hooks.toml");
    assert!(project_hook.exists());
    assert!(
        fs::read_to_string(project_hook)
            .unwrap()
            .contains("retok-output")
    );
}

#[cfg(not(windows))]
#[test]
fn edited_matcher_groups_survive_upgrade_and_uninstall() {
    let f = Fixture::new();
    f.plan(&["--agent", "codex"]).apply().unwrap();
    let path = f.roots.codex.join("hooks.json");
    let mut v = value(&path);
    v["hooks"]["PreToolUse"][0]["matcher"] = json!("CustomTool");
    v["hooks"]["PreToolUse"][0]["custom"] = json!("preserve");
    let custom = v["hooks"]["PreToolUse"][0].clone();
    fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    f.plan(&["--agent", "codex"]).apply().unwrap();
    f.plan(&["--agent", "codex", "--uninstall"])
        .apply()
        .unwrap();
    assert_eq!(value(&path)["hooks"]["PreToolUse"], json!([custom]));
}

fn hermes_migration_fixture(config: &str) -> Fixture {
    let mut f = Fixture::new();
    f.roots.executable = std::env::current_exe().unwrap();
    f.write(".hermes/config.yaml", config);
    for name in ["__init__.py", "plugin.yaml"] {
        f.write(
            &format!(".hermes/plugins/rtk-rewrite/{name}"),
            "synthetic artifact\n",
        );
    }
    f
}
fn hermes_migration(f: &Fixture) -> anyhow::Result<setup::Plan> {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(b"synthetic artifact\n"));
    setup::plan_with_stock(
        &["--agent".into(), "hermes".into(), "--replace-rtk".into()],
        &f.roots,
        &[
            ("hermes-__init__.py", 19, digest.as_str()),
            ("hermes-plugin.yaml", 19, digest.as_str()),
        ],
    )
}

#[test]
fn hermes_yaml_boolean_disables_preserve_rtk_before_any_writes() {
    for field in ["settings", "config"] {
        for disabled in [
            "false",
            "off",
            "Off",
            "OFF",
            "no",
            "No",
            "NO",
            "!!bool off",
            "!!bool 'off'",
        ] {
            let body = format!(
                "plugins:\n  enabled: [rtk-rewrite, user-plugin]\n  entries:\n    retok-rewrite:\n      {field}:\n        enabled: {disabled}\n"
            );
            let f = hermes_migration_fixture(&body);
            let p = hermes_migration(&f).unwrap_or_else(|e| panic!("{disabled}: {e:#}"));
            assert!(p.changes.is_empty(), "{disabled}: {p:?}");
            assert!(p.messages.join(" ").contains("explicitly disabled"));
            assert_eq!(
                fs::read_to_string(f.roots.hermes.join("config.yaml")).unwrap(),
                body
            );
            assert!(!f.roots.hermes.join("plugins/retok-rewrite").exists());
            assert!(
                f.plan(&["--agent", "hermes", "--show"])
                    .messages
                    .join(" ")
                    .contains("explicitly disabled")
            );
        }
    }
}

#[test]
fn hermes_yaml_migration_preserves_scalar_styles_tags_and_ordinary_values() {
    let custom = "# keep this comment\nquoted_off: 'off'\nplain_off: off\nquoted_yes: 'yes'\nplain_yes: yes\nordinary: example\noctal: 012\nquoted_octal: '012'\ndate: 2026-01-02\ntagged_string: !!str off\ntagged_boolean: !!bool 'no'\n";
    for enabled in ["true", "yes", "on", "'off'", "'yes'", "!!str off"] {
        let body = format!(
            "{custom}plugins:\n  enabled: [rtk-rewrite, user-plugin]\n  entries:\n    retok-rewrite:\n      settings:\n        enabled: {enabled}\n"
        );
        let f = hermes_migration_fixture(&body);
        let backups = hermes_migration(&f).unwrap().apply().unwrap();
        assert!(
            backups
                .iter()
                .any(|p| fs::read(p).unwrap() == body.as_bytes())
        );
        let after = fs::read_to_string(f.roots.hermes.join("config.yaml")).unwrap();
        assert!(after.starts_with(custom), "{after}");
        assert!(after.contains(&format!("enabled: {enabled}\n")), "{after}");
        assert!(after.contains("user-plugin") && after.contains("retok-rewrite"));
        assert!(hermes_migration(&f).unwrap().changes.is_empty());
    }
}

#[test]
fn hermes_yaml_merges_preserve_other_plugins_and_inherited_disables() {
    for body in [
        "plugins:\n  enabled: [rtk-rewrite, user-plugin]\n",
        "base: &base\n  enabled: [rtk-rewrite, user-plugin]\nplugins:\n  <<: *base\n",
        "base: &base\n  plugins:\n    enabled: [rtk-rewrite, user-plugin]\n    custom: 'off'\n<<: *base\n",
        "base: &base\n  enabled: [rtk-rewrite, user-plugin]\nplugins: *base\n",
        "base: &base\n  enabled: [rtk-rewrite, user-plugin]\n  custom: &custom {value: 'off'}\n  copy: *custom\nplugins: *base\n",
        "base: &base\n  plugins: &plugins\n    enabled: [rtk-rewrite, user-plugin]\n    custom: &custom {value: 'off'}\n    copy: *custom\n<<: *base\n",
    ] {
        let f = hermes_migration_fixture(body);
        hermes_migration(&f).unwrap().apply().unwrap();
        let text = fs::read_to_string(f.roots.hermes.join("config.yaml")).unwrap();
        // PyYAML rejects duplicate anchors even though serde_yaml accepts them.
        // Each original declaration must remain once after materialization.
        for anchor in ["&base", "&plugins", "&custom"] {
            assert_eq!(
                text.matches(anchor).count(),
                body.matches(anchor).count(),
                "{text}"
            );
        }
        let mut after: serde_yaml_ng::Value =
            serde_yaml_ng::from_slice(&fs::read(f.roots.hermes.join("config.yaml")).unwrap())
                .unwrap();
        after.apply_merge().unwrap();
        assert_eq!(
            after["plugins"]["enabled"],
            serde_yaml_ng::from_str::<serde_yaml_ng::Value>("[user-plugin, retok-rewrite]")
                .unwrap()
        );
        assert!(hermes_migration(&f).unwrap().changes.is_empty());
    }
    for inherited in [
        "disabled: [retok-rewrite]",
        "entries: {retok-rewrite: {settings: {enabled: off}}}",
        "entries: {retok-rewrite: {config: {enabled: !!bool no}}}",
    ] {
        let body = format!(
            "base: &base\n  {inherited}\nplugins:\n  <<: *base\n  enabled: [rtk-rewrite, user-plugin]\n"
        );
        let f = hermes_migration_fixture(&body);
        let p = hermes_migration(&f).unwrap();
        assert!(p.changes.is_empty());
        assert!(p.messages.join(" ").contains("explicitly disabled"));
        assert_eq!(
            fs::read_to_string(f.roots.hermes.join("config.yaml")).unwrap(),
            body
        );
    }
    for invalid in [
        "plugins: {<<: 17}",
        "plugins: {<<: [17]}",
        "plugins: {<<: *missing}",
        "plugins: {entries: {retok-rewrite: {settings: {enabled: !<tag:yaml.org,2002:bool> no}}}}",
    ] {
        let f = hermes_migration_fixture(invalid);
        assert!(hermes_migration(&f).is_err());
        assert_eq!(
            fs::read_to_string(f.roots.hermes.join("config.yaml")).unwrap(),
            invalid
        );
        assert!(!f.roots.hermes.join("plugins/retok-rewrite").exists());
    }
}

#[test]
fn hermes_completion_bundle_path_upgrade_and_uninstall_preserve_other_plugins() {
    let mut f = Fixture::new();
    f.roots.executable = f.root.join("bin ' 日本 $value/retok");
    let original = "# keep\nplugins:\n  enabled: [user-plugin]\ncustom: 'off'\n";
    let config = f.write(".hermes/config.yaml", original);
    let backups = f.plan(&["--agent", "hermes"]).apply().unwrap();
    assert!(
        backups
            .iter()
            .any(|p| fs::read(p).unwrap() == original.as_bytes())
    );
    let plugin = f.roots.hermes.join("plugins/retok-rewrite/__init__.py");
    let text = fs::read_to_string(&plugin).unwrap();
    assert!(text.contains("register_hook(\"transform_tool_result\""));
    assert!(!text.contains("register_hook(\"pre_tool_call\""));
    let hex: String = f
        .roots
        .executable
        .to_str()
        .unwrap()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert!(text.contains(&format!("bytes.fromhex('{hex}')")));
    assert!(f.plan(&["--agent", "hermes"]).changes.is_empty());
    f.roots.executable = f.root.join("new-install/retok");
    f.plan(&["--agent", "hermes"]).apply().unwrap();
    f.plan(&["--agent", "hermes", "--uninstall"])
        .apply()
        .unwrap();
    assert!(!plugin.exists());
    let after = fs::read_to_string(&config).unwrap();
    assert!(after.contains("custom: 'off'"));
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&after).unwrap();
    assert_eq!(
        value["plugins"]["enabled"],
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>("[user-plugin]").unwrap()
    );
}

#[test]
fn hermes_empty_and_flow_configs_install_and_repeat() {
    for original in [
        "",
        "# keep\n",
        "null\n",
        "{}\n",
        "{model: 'off'}\n",
        "{plugins: {enabled: [user-plugin]}}\n",
    ] {
        let f = Fixture::new();
        let config = f.write(".hermes/config.yaml", original);
        f.plan(&["--agent", "hermes"]).apply().unwrap();
        let value: serde_yaml_ng::Value =
            serde_yaml_ng::from_slice(&fs::read(&config).unwrap()).unwrap();
        assert!(
            value["plugins"]["enabled"]
                .as_sequence()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some("retok-rewrite"))
        );
        assert!(f.plan(&["--agent", "hermes"]).changes.is_empty());
    }
}

#[test]
fn native_plugin_explicit_disables_and_denies_are_preserved() {
    for body in [
        "plugins:\n  disabled: [retok-rewrite]\n",
        "plugins:\n  entries:\n    retok-rewrite:\n      settings:\n        enabled: false\n",
        "plugins:\n  entries:\n    retok-rewrite:\n      config:\n        enabled: false\n",
    ] {
        let f = Fixture::new();
        let path = f.write(".hermes/config.yaml", body);
        let p = f.plan(&["--agent", "hermes"]);
        assert!(p.changes.is_empty());
        assert!(p.messages.join(" ").contains("explicitly disabled"));
        assert_eq!(fs::read_to_string(path).unwrap(), body);
    }
    for plugins in [
        json!({"enabled":false}),
        json!({"deny":["retok-rewrite"]}),
        json!({"entries":{"retok-rewrite":{"enabled":false}}}),
        json!({"entries":{"retok-rewrite":{"config":{"enabled":false}}}}),
    ] {
        let f = Fixture::new();
        let body = serde_json::to_string(&json!({"plugins":plugins})).unwrap();
        let path = f.write(".openclaw/openclaw.json", &body);
        let p = f.plan(&["--agent", "openclaw"]);
        assert!(p.changes.is_empty());
        assert!(p.messages.join(" ").contains("explicit disable/deny"));
        assert_eq!(fs::read_to_string(path).unwrap(), body);
    }
}

#[test]
fn openclaw_json5_unknowns_and_explicit_single_plugin_allowlist_addition() {
    let mut f = Fixture::new();
    f.roots.openclaw = f.root.join("openclaw-state");
    f.roots.openclaw_config = Some(f.root.join("separate-config.json"));
    let config = f.roots.openclaw_config.as_ref().unwrap();
    let text = "// User JSON5 comment\n{ custom: 'keep', plugins: { allow: ['user-plugin'], entries: {'user-plugin': {enabled: true, config: {keep: 7}}}, }, }";
    fs::create_dir_all(&f.roots.openclaw).unwrap();
    fs::write(config, text).unwrap();
    let detected = f.plan(&[]);
    assert!(detected.changes.is_empty());
    let p = f.plan(&["--agent", "openclaw"]);
    assert!(p.messages.join(" ").contains("add only retok-rewrite"));
    let backups = p.apply().unwrap();
    assert!(
        backups
            .iter()
            .any(|p| fs::read(p).unwrap() == text.as_bytes())
    );
    let v = value(config);
    assert_eq!(
        v["plugins"]["allow"],
        json!(["user-plugin", "retok-rewrite"])
    );
    assert_eq!(v["plugins"]["entries"]["user-plugin"]["config"]["keep"], 7);
    assert_eq!(
        v["plugins"]["entries"]["retok-rewrite"],
        json!({"enabled":true,"config":{"enabled":true}})
    );
    assert_eq!(v["custom"], "keep");
    assert!(
        f.roots
            .openclaw
            .join("extensions/retok-rewrite/index.mjs")
            .exists()
    );
    assert!(!f.roots.home.join(".openclaw").exists());
    assert!(f.plan(&["--agent", "openclaw"]).changes.is_empty());
    f.plan(&["--agent", "openclaw", "--uninstall"])
        .apply()
        .unwrap();
    assert_eq!(value(config)["plugins"]["allow"], json!(["user-plugin"]));
    assert!(
        value(config)["plugins"]["entries"]
            .get("retok-rewrite")
            .is_none()
    );
}

#[test]
fn openclaw_uninstall_never_turns_a_restrictive_allowlist_into_allow_all() {
    let f = Fixture::new();
    let config = f.write(
        ".openclaw/openclaw.json",
        r#"{"plugins":{"allow":["retok-rewrite"]}}"#,
    );
    f.plan(&["--agent", "openclaw"]).apply().unwrap();
    f.plan(&["--agent", "openclaw", "--uninstall"])
        .apply()
        .unwrap();
    assert_eq!(value(&config)["plugins"]["allow"], json!(["retok-rewrite"]));
}

#[test]
fn edited_native_plugin_bundle_preserves_activation_and_every_file() {
    for (host, relative, config) in [
        (
            "hermes",
            ".hermes/plugins/retok-rewrite/__init__.py",
            ".hermes/config.yaml",
        ),
        (
            "openclaw",
            ".openclaw/extensions/retok-rewrite/index.mjs",
            ".openclaw/openclaw.json",
        ),
    ] {
        let f = Fixture::new();
        f.plan(&["--agent", host]).apply().unwrap();
        let path = f.roots.home.join(relative);
        let changed = format!(
            "{}\n# synthetic user edit\n",
            fs::read_to_string(&path).unwrap()
        );
        fs::write(&path, &changed).unwrap();
        let config = f.roots.home.join(config);
        let before = fs::read(&config).unwrap();
        assert!(plan(&["--agent".into(), host.into()], &f.roots).is_err());
        assert!(f.plan(&["--agent", host, "--uninstall"]).changes.is_empty());
        assert_eq!(fs::read(&config).unwrap(), before);
        assert_eq!(fs::read_to_string(&path).unwrap(), changed);
    }
}

#[cfg(unix)]
#[test]
fn stock_native_plugin_migration_switches_activation_only_after_replacement_is_ready() {
    use sha2::{Digest, Sha256};
    for host in ["hermes", "openclaw"] {
        let mut f = Fixture::new();
        let files: &[&str] = if host == "hermes" {
            &["__init__.py", "plugin.yaml"]
        } else {
            &["index.ts", "openclaw.plugin.json", "package.json"]
        };
        let dir = if host == "hermes" {
            ".hermes/plugins/rtk-rewrite"
        } else {
            ".openclaw/extensions/rtk-rewrite"
        };
        let body = b"synthetic stock plugin artifact\n";
        let mut stock = vec![];
        for name in files {
            f.write(&format!("{dir}/{name}"), std::str::from_utf8(body).unwrap());
            stock.push((
                format!("{host}-{name}"),
                body.len(),
                format!("{:x}", Sha256::digest(body)),
            ));
        }
        f.write(&format!("{dir}/README.md"), "Synthetic documentation.\n");
        let table = stock
            .iter()
            .map(|(k, n, h)| (k.as_str(), *n, h.as_str()))
            .collect::<Vec<_>>();
        let config = if host == "hermes" {
            f.write(
                ".hermes/config.yaml",
                "plugins:\n  enabled: [rtk-rewrite, user-plugin]\n",
            )
        } else {
            f.write(".openclaw/openclaw.json", r#"{"plugins":{"entries":{"rtk-rewrite":{"enabled":true},"user-plugin":{"enabled":true}}}}"#)
        };
        let original = fs::read(&config).unwrap();
        let args = [
            OsString::from("--agent"),
            host.into(),
            "--replace-rtk".into(),
        ];
        let blocked = setup::plan_with_stock(&args, &f.roots, &table).unwrap();
        assert!(blocked.changes.is_empty());
        assert!(blocked.messages.join(" ").contains("unavailable"));
        f.roots.executable = std::env::current_exe().unwrap();
        let p = setup::plan_with_stock(&args, &f.roots, &table).unwrap();
        let backups = p.apply().unwrap();
        assert!(backups.iter().any(|p| fs::read(p).unwrap() == original));
        for name in files {
            assert_eq!(
                fs::read(f.roots.home.join(format!("{dir}/{name}"))).unwrap(),
                body
            );
        }
        if host == "hermes" {
            let v: serde_yaml_ng::Value =
                serde_yaml_ng::from_slice(&fs::read(&config).unwrap()).unwrap();
            assert_eq!(
                v["plugins"]["enabled"],
                serde_yaml_ng::from_str::<serde_yaml_ng::Value>("[user-plugin, retok-rewrite]")
                    .unwrap()
            );
        } else {
            assert_eq!(
                value(&config)["plugins"]["entries"]["rtk-rewrite"]["enabled"],
                false
            );
            assert_eq!(
                value(&config)["plugins"]["entries"]["retok-rewrite"]["enabled"],
                true
            );
        }
        assert!(
            setup::plan_with_stock(&args, &f.roots, &table)
                .unwrap()
                .changes
                .is_empty()
        );
        assert!(f.plan(&["--agent", host]).changes.is_empty());
        let alias = f.root.join("upgraded-retok");
        std::os::unix::fs::symlink(&f.roots.executable, &alias).unwrap();
        f.roots.executable = alias;
        let upgrade = f.plan(&["--agent", host]);
        assert!(!upgrade.changes.is_empty());
        upgrade.apply().unwrap();
        assert!(f.plan(&["--agent", host]).changes.is_empty());
    }
}

#[cfg(windows)]
#[test]
fn unsupported_windows_prehooks_preserve_rtk_while_posthooks_still_install() {
    for (host, relative, original) in [
        (
            "codex",
            ".codex/hooks.json",
            r#"{"hooks":{"PreToolUse":[{"command":"rtk hook codex"}]}}"#,
        ),
        (
            "cursor",
            ".cursor/hooks.json",
            r#"{"hooks":{"preToolUse":[{"command":"rtk hook cursor"}]}}"#,
        ),
        (
            "gemini",
            ".gemini/settings.json",
            r#"{"hooks":{"BeforeTool":[{"command":"rtk hook gemini"}]}}"#,
        ),
        (
            "droid",
            ".factory/hooks.json",
            r#"{"PreToolUse":[{"command":"rtk hook droid"}]}"#,
        ),
        (
            "copilot",
            ".copilot/hooks/rtk-rewrite.json",
            r#"{"hooks":{"PreToolUse":[{"command":"rtk hook copilot"}]}}"#,
        ),
        (
            "vibe",
            ".vibe/hooks.toml",
            "[[hooks]]\nname='rtk-rewrite'\ntype='pre_tool'\nmatch='bash'\ncommand='rtk hook vibe'\n",
        ),
    ] {
        let f = Fixture::new();
        let path = f.write(relative, original);
        assert!(
            f.plan(&["--agent", host, "--replace-rtk"])
                .changes
                .is_empty(),
            "{host}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        if host != "cursor" {
            let p = f.plan(&["--agent", host, "--replace-rtk", "--instructions-only"]);
            assert!(!p.changes.is_empty(), "{host}");
            p.apply().unwrap();
            assert!(!fs::read_to_string(path).unwrap().contains("rtk hook"));
        }
    }
    let f = Fixture::new();
    for host in ["claude", "copilot"] {
        let p = f.plan(&["--agent", host]);
        assert!(!p.changes.is_empty());
        p.apply().unwrap();
    }
    assert!(value(&f.roots.claude.join("settings.json"))["hooks"]["PostToolUse"].is_array());
    assert!(value(&f.roots.copilot.join("hooks/retok.json"))["hooks"]["postToolUse"].is_array());
}

#[test]
fn openclaw_json5_preserves_literal_number_marker_and_rejects_ambiguous_configs() {
    let f = Fixture::new();
    let path = f.write(
        ".openclaw/openclaw.json",
        "// JSON5\n{custom: {'$serde_json::private::Number': '6.0200'}, integer: 9007199254740993}",
    );
    f.plan(&["--agent", "openclaw"]).apply().unwrap();
    let fields: std::collections::BTreeMap<String, Box<serde_json::value::RawValue>> =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let custom: std::collections::BTreeMap<String, String> =
        serde_json::from_str(fields["custom"].get()).unwrap();
    assert_eq!(custom["$serde_json::private::Number"], "6.0200");
    assert_eq!(value(&path)["integer"].as_u64(), Some(9007199254740993));
    for bad in [
        "{custom: 1, custom: 2}",
        "{custom: NaN}",
        "{custom: Infinity}",
        "{plugins: {enabled: 'false'}}",
    ] {
        fs::write(&path, bad).unwrap();
        assert!(
            plan(&["--agent".into(), "openclaw".into()], &f.roots).is_err(),
            "{bad}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), bad);
    }
}
