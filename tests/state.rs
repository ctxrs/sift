#[allow(dead_code)]
#[path = "../src/state.rs"]
mod state;

use state::{Event, Settings};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "retok-state-test-{}-{}-{}",
            std::process::id(),
            state::unix_millis(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn event() -> Event {
    Event {
        unix_millis: 86_400_001,
        command: "Bash".into(),
        input_tokens: Some(100),
        output_tokens: Some(40),
        input_bytes: 1000,
        output_bytes: 300,
        duration_ms: 12,
        exit_code: Some(0),
        source: Some("hook".into()),
        original_id: None,
    }
}
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn defaults_are_read_only_and_corrupt_config_is_preserved() {
    let temp = Temp::new();
    let path = temp.path().join("nested/config.json");
    assert_eq!(Settings::load_from(&path).unwrap(), Settings::default());
    assert!(!path.parent().unwrap().exists());
    Settings::create_default(&path).unwrap();
    assert_eq!(Settings::load_from(&path).unwrap(), Settings::default());
    fs::write(&path, b"{ broken config").unwrap();
    assert!(Settings::load_from(&path).is_err());
    assert!(Settings::create_default(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{ broken config");
    fs::write(&path, br#"{"exclude_commands":["git *"]}"#).unwrap();
    assert!(Settings::load_from(&path).is_err());
    fs::write(&path, br#"{"exclude_commands":["git"]}"#).unwrap();
    let settings = Settings::load_from(&path).unwrap();
    assert!(settings.excludes("/usr/bin/git"));
    assert!(!settings.excludes("git-lfs"));
}
#[test]
fn concurrent_writers_produce_complete_records() {
    let temp = Temp::new();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let path = temp.path();
            scope.spawn(move || {
                for _ in 0..20 {
                    state::record_at(path, &Settings::default(), event(), None).unwrap();
                }
            });
        }
    });
    let usage = state::usage_at(temp.path()).unwrap();
    // Contending optional writes may be skipped; every accepted line is intact.
    assert!((1..=160).contains(&usage.events.len()));
    assert_eq!(usage.malformed_records, 0);
    assert!(usage.events.iter().all(|e| e.command == "Bash"
        && e.input_tokens == Some(100)
        && e.output_tokens == Some(40)));
    assert_eq!(usage.totals().saved_tokens, usage.events.len() as i128 * 60);
}
#[test]
fn unmeasured_records_and_malformed_lines_do_not_inflate_savings() {
    use std::io::Write;
    let temp = Temp::new();
    state::record_at(temp.path(), &Settings::default(), event(), None).unwrap();
    let mut raw = event();
    raw.input_tokens = None;
    raw.output_tokens = None;
    state::record_at(temp.path(), &Settings::default(), raw, None).unwrap();
    let mut partial = event();
    partial.output_tokens = None;
    state::record_at(temp.path(), &Settings::default(), partial, None).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(temp.path().join("metrics.jsonl"))
        .unwrap()
        .write_all(b"bad JSON\n{\"command\":\"private args\"}\n")
        .unwrap();
    let usage = state::usage_at(temp.path()).unwrap();
    assert_eq!(usage.malformed_records, 2);
    let totals = usage.totals();
    assert_eq!(
        (
            totals.events,
            totals.measured_events,
            totals.input_tokens,
            totals.output_tokens,
            totals.saved_tokens
        ),
        (3, 1, 100, 40, 60)
    );
    let mut output = Vec::new();
    state::gain_at(
        temp.path(),
        &args(&["--json", "--history", "--daily"]),
        &mut output,
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["malformed_records"], 2);
    assert_eq!(json["daily_utc"]["1970-01-02"]["saved_tokens"], 60);
    assert!(json["history"][1]["input_tokens"].is_null());
    assert!(state::gain_at(temp.path(), &args(&["--reset", "--json"]), &mut output).is_err());
    assert_eq!(state::usage_at(temp.path()).unwrap().events.len(), 3);
}
#[test]
fn raw_bytes_are_opt_in_separate_and_recall_rejects_traversal() {
    let temp = Temp::new();
    let stdout = b"\xffprivate stdout\0";
    let stderr = b"\xfeprivate stderr\n";
    state::record_at(
        temp.path(),
        &Settings::default(),
        event(),
        Some((stdout, stderr)),
    )
    .unwrap();
    assert!(!temp.path().join("originals").exists());
    let log = fs::read_to_string(temp.path().join("metrics.jsonl")).unwrap();
    assert!(!log.contains("private"));
    assert!(
        state::usage_at(temp.path()).unwrap().events[0]
            .original_id
            .is_none()
    );
    let settings = Settings {
        keep_originals: true,
        ..Default::default()
    };
    state::record_at(temp.path(), &settings, event(), Some((stdout, stderr))).unwrap();
    let entries = state::list_originals_at(temp.path()).unwrap();
    assert_eq!(entries.len(), 1);
    let id = &entries[0].id;
    assert_eq!(
        state::usage_at(temp.path()).unwrap().events[1]
            .original_id
            .as_ref(),
        Some(id)
    );
    for (is_stderr, expected) in [(false, stdout.as_slice()), (true, stderr.as_slice())] {
        let mut output = Vec::new();
        state::recall_at(temp.path(), id, is_stderr, &mut output).unwrap();
        assert_eq!(output, expected);
    }
    for id in ["../config", "/tmp/a", "..", "a/b", "a\\b", ""] {
        assert!(state::recall_at(temp.path(), id, false, &mut Vec::new()).is_err());
    }
    state::reset_at(temp.path()).unwrap();
    assert!(state::usage_at(temp.path()).unwrap().events.is_empty());
    let mut output = Vec::new();
    state::recall_at(temp.path(), id, false, &mut output).unwrap();
    assert_eq!(output, stdout);
}
#[test]
fn retention_evicts_oldest_originals_and_rotates_one_metrics_backup() {
    let temp = Temp::new();
    let settings = Settings {
        keep_originals: true,
        ..Default::default()
    };
    state::record_at(temp.path(), &settings, event(), Some((b"first", b""))).unwrap();
    let first = state::list_originals_at(temp.path()).unwrap().remove(0).id;
    // Set an old timestamp deterministically; no timing assumptions between writes.
    fs::File::open(temp.path().join("originals").join(&first))
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    for _ in 0..101 {
        state::record_at(temp.path(), &settings, event(), Some((b"x", b"y"))).unwrap();
    }
    let entries = state::list_originals_at(temp.path()).unwrap();
    assert_eq!(entries.len(), 100);
    assert!(!entries.iter().any(|entry| entry.id == first));
    let log = temp.path().join("metrics.jsonl");
    // A full log forces rotation on the next write. The old backup is replaced.
    fs::write(temp.path().join("metrics.1.jsonl"), b"older backup").unwrap();
    fs::write(&log, vec![b'\n'; 10 * 1024 * 1024]).unwrap();
    state::record_at(temp.path(), &Settings::default(), event(), None).unwrap();
    assert_eq!(
        fs::metadata(temp.path().join("metrics.1.jsonl"))
            .unwrap()
            .len(),
        10 * 1024 * 1024
    );
    let current: Event = serde_json::from_slice(&fs::read(log).unwrap()).unwrap();
    assert_eq!(current.command, "Bash");
}
#[test]
fn command_lines_are_rejected_before_any_write() {
    let temp = Temp::new();
    for command in ["git --password secret", "/usr/bin/git", "git\nsecret"] {
        let mut input = event();
        input.command = command.into();
        assert!(state::record_at(temp.path(), &Settings::default(), input, None).is_err());
    }
    assert!(!temp.path().join("metrics.jsonl").exists());
}
#[cfg(unix)]
#[test]
fn files_are_private_and_recall_rejects_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = Temp::new();
    state::record_at(
        temp.path(),
        &Settings {
            keep_originals: true,
            ..Default::default()
        },
        event(),
        Some((b"data", b"")),
    )
    .unwrap();
    let entry = state::list_originals_at(temp.path()).unwrap().remove(0);
    for path in [
        temp.path().join("metrics.jsonl"),
        temp.path().join("state.lock"),
        temp.path().join("originals").join(&entry.id).join("stdout"),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let stream = temp.path().join("originals").join(&entry.id).join("stdout");
    fs::remove_file(&stream).unwrap();
    symlink(temp.path().join("metrics.jsonl"), &stream).unwrap();
    assert!(state::recall_at(temp.path(), &entry.id, false, &mut Vec::new()).is_err());
}

#[test]
fn interrupted_record_is_preserved_and_does_not_swallow_next_event() {
    let temp = Temp::new();
    fs::write(temp.path().join("metrics.jsonl"), b"{broken").unwrap();
    state::record_at(temp.path(), &Settings::default(), event(), None).unwrap();
    assert!(
        fs::read(temp.path().join("metrics.jsonl"))
            .unwrap()
            .starts_with(b"{broken\n")
    );
    let usage = state::usage_at(temp.path()).unwrap();
    assert_eq!(usage.events.len(), 1);
    assert_eq!(usage.malformed_records, 1);
}

#[test]
fn recording_disabled_creates_nothing_and_enabled_only_controls_output() {
    let temp = Temp::new();
    let dir = temp.path().join("absent");
    let settings = Settings {
        record_usage: false,
        keep_originals: true,
        ..Default::default()
    };
    state::record_at(&dir, &settings, event(), Some((b"private", b"error"))).unwrap();
    assert!(!dir.exists());
    let settings = Settings {
        enabled: false,
        ..Default::default()
    };
    state::record_at(&dir, &settings, event(), None).unwrap();
    let before = fs::read(dir.join("metrics.jsonl")).unwrap();
    state::record_at(
        &dir,
        &Settings {
            record_usage: false,
            ..settings
        },
        event(),
        None,
    )
    .unwrap();
    assert_eq!(fs::read(dir.join("metrics.jsonl")).unwrap(), before);
}
#[test]
fn daily_dates_cover_epoch_leap_centuries_and_current_date() {
    let temp = Temp::new();
    // Fixed Unix seconds, checked independently with Python's datetime UTC.
    let fixtures = [
        (0, "1970-01-01"),
        (86400, "1970-01-02"),
        (951782400, "2000-02-29"),
        (951868800, "2000-03-01"),
        (1709164800, "2024-02-29"),
        (1789603200, "2026-09-17"),
        (4107542400, "2100-03-01"),
        (13574563200, "2400-02-29"),
    ];
    for (seconds, _) in fixtures {
        let mut input = event();
        input.unix_millis = seconds * 1000;
        state::record_at(temp.path(), &Settings::default(), input, None).unwrap();
    }
    let mut output = Vec::new();
    state::gain_at(temp.path(), &args(&["--json", "--daily"]), &mut output).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["daily_utc"].as_object().unwrap().len(), fixtures.len());
    for (_, date) in fixtures {
        assert_eq!(json["daily_utc"][date]["saved_tokens"], 60, "{date}");
    }
    for flag in ["--daily", "--graph"] {
        output.clear();
        state::gain_at(temp.path(), &args(&[flag]), &mut output).unwrap();
        let text = String::from_utf8(output.clone()).unwrap();
        for (_, date) in fixtures {
            assert!(text.contains(&format!("{date}: 60")), "{text}");
        }
    }
}
#[test]
fn text_history_is_readable_and_help_does_not_create_state() {
    let temp = Temp::new();
    let dir = temp.path().join("absent");
    let mut output = Vec::new();
    state::gain_at(&dir, &args(&["--help"]), &mut output).unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("Usage: retok gain")
    );
    assert!(!dir.exists());
    state::record_at(&dir, &Settings::default(), event(), None).unwrap();
    let mut input = event();
    input.input_tokens = None;
    input.output_tokens = None;
    state::record_at(&dir, &Settings::default(), input, None).unwrap();
    let mut output = Vec::new();
    state::gain_at(&dir, &args(&["--history"]), &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("1970-01-02 00:00:00 UTC  Bash  100 -> 40 tokens (60 saved)"));
    assert!(text.contains("Bash  tokens unmeasured"));
    assert!(!text.contains("{\""));
}

#[test]
fn contended_recording_skips_without_waiting_or_saving_originals() {
    use std::sync::mpsc;
    use std::time::Duration;
    let temp = Temp::new();
    state::record_at(temp.path(), &Settings::default(), event(), None).unwrap();
    let held = fs::File::options()
        .write(true)
        .open(temp.path().join("state.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&held).unwrap();
    // fs2 reports native contention codes: Windows ERROR_LOCK_VIOLATION need
    // not map to WouldBlock. Exercise the actual platform error, then recording.
    let probe = fs::File::options()
        .write(true)
        .open(temp.path().join("state.lock"))
        .unwrap();
    let error = fs2::FileExt::try_lock_exclusive(&probe).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        fs2::lock_contended_error().raw_os_error()
    );
    #[cfg(windows)]
    assert_eq!(error.raw_os_error(), Some(33));
    drop(probe);
    let (done, result) = mpsc::channel();
    let dir = temp.path().to_owned();
    let worker = std::thread::spawn(move || {
        done.send(state::record_at(
            &dir,
            &Settings {
                keep_originals: true,
                ..Default::default()
            },
            event(),
            Some((b"raw", b"")),
        ))
        .unwrap();
    });
    let completed = result.recv_timeout(Duration::from_secs(2));
    drop(held);
    worker.join().unwrap();
    completed
        .expect("recording waited for a busy lock")
        .unwrap();
    assert_eq!(state::usage_at(temp.path()).unwrap().events.len(), 1);
    assert!(!temp.path().join("originals").exists());
}

#[cfg(unix)]
#[test]
fn paused_recall_releases_lock_and_command_can_complete() {
    use std::io::{self, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    struct Paused {
        entered: Option<mpsc::Sender<()>>,
        resume: mpsc::Receiver<()>,
        bytes: Vec<u8>,
    }
    impl Write for Paused {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.resume.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let temp = Temp::new();
    state::record_at(
        temp.path(),
        &Settings {
            keep_originals: true,
            ..Default::default()
        },
        event(),
        Some((b"saved bytes", b"")),
    )
    .unwrap();
    let id = state::list_originals_at(temp.path()).unwrap().remove(0).id;
    let (entered, blocked) = mpsc::channel();
    let (resume, receiver) = mpsc::channel();
    let dir = temp.path().to_owned();
    let worker = std::thread::spawn(move || {
        let mut sink = Paused {
            entered: Some(entered),
            resume: receiver,
            bytes: Vec::new(),
        };
        state::recall_at(&dir, &id, false, &mut sink).unwrap();
        sink.bytes
    });
    blocked.recv_timeout(Duration::from_secs(2)).unwrap();
    let file = fs::File::options()
        .write(true)
        .open(temp.path().join("state.lock"))
        .unwrap();
    let available = fs2::FileExt::try_lock_exclusive(&file).is_ok();
    drop(file);
    let mut child = Command::new(env!("CARGO_BIN_EXE_retok"))
        .args(["run", "--", "/bin/sh", "-c", "exit 0"])
        .env("RETOK_STATE_DIR", temp.path())
        .env("RETOK_CONFIG_DIR", temp.path().join("config"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    resume.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), b"saved bytes");
    assert!(
        available,
        "recall holds the state lock while blocked on its consumer"
    );
    assert!(status.expect("command blocked behind recall").success());
    assert_eq!(state::usage_at(temp.path()).unwrap().events.len(), 2);
}

#[test]
fn interrupted_originals_are_pruned_and_retained_complete_streams_survive() {
    let temp = Temp::new();
    let settings = Settings {
        keep_originals: true,
        ..Default::default()
    };
    state::record_at(
        temp.path(),
        &settings,
        event(),
        Some((b"complete stdout", b"complete stderr")),
    )
    .unwrap();
    let first = state::list_originals_at(temp.path()).unwrap().remove(0).id;
    let originals = temp.path().join("originals");
    for name in ["dead-beef", "dead-beef.pending"] {
        fs::create_dir(originals.join(name)).unwrap();
        fs::write(originals.join(name).join("stdout"), b"interrupted").unwrap();
    }
    // Save directly without listing first: normal recording itself repairs leftovers.
    state::record_at(temp.path(), &settings, event(), Some((b"next", b""))).unwrap();
    assert!(!originals.join("dead-beef").exists());
    assert!(!originals.join("dead-beef.pending").exists());
    assert_eq!(state::list_originals_at(temp.path()).unwrap().len(), 2);
    for (stderr, expected) in [(false, b"complete stdout"), (true, b"complete stderr")] {
        let mut bytes = Vec::new();
        state::recall_at(temp.path(), &first, stderr, &mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }
    for _ in 0..100 {
        state::record_at(temp.path(), &settings, event(), Some((b"later", b""))).unwrap();
    }
    assert_eq!(state::list_originals_at(temp.path()).unwrap().len(), 100);
    assert_eq!(fs::read_dir(originals).unwrap().count(), 100);
    assert_eq!(state::usage_at(temp.path()).unwrap().events.len(), 102);
}
