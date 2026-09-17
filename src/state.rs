//! Local-only settings and measured usage. Unix (including macOS) uses XDG paths.
//! Metrics rotate at 10 MiB, retaining one backup; the next rotation replaces it.
//! Original retention defaults to 100 entries / 100 MiB / 30 days, configurable locally.
//! No arguments, raw output, estimates, network calls, or history scans by default.
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const METRICS_LIMIT: u64 = 10 * 1024 * 1024;
const ORIGINAL_LIMIT: u64 = 100 * 1024 * 1024;
const ORIGINAL_AGE: u64 = 30;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub record_usage: bool,
    pub keep_originals: bool,
    pub exclude_commands: Vec<String>,
    pub originals_max_entries: usize,
    pub originals_max_bytes: u64,
    pub originals_max_days: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            record_usage: true,
            keep_originals: false,
            exclude_commands: Vec::new(),
            originals_max_entries: 100,
            originals_max_bytes: ORIGINAL_LIMIT,
            originals_max_days: ORIGINAL_AGE,
        }
    }
}
impl Settings {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path()?)
    }
    /// Missing settings return defaults without creating a directory or file.
    pub fn load_from(path: &Path) -> Result<Self> {
        match File::open(path) {
            Ok(file) => {
                let settings: Self = serde_json::from_reader(file)
                    .context("invalid Retok config; file left unchanged")?;
                ensure!(
                    settings.exclude_commands.iter().all(|s| valid_label(s)),
                    "exclude_commands must contain exact executable basenames"
                );
                settings.validate()?;
                Ok(settings)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.originals_max_entries > 0,
            "originals_max_entries must be positive"
        );
        ensure!(
            self.originals_max_bytes > 0,
            "originals_max_bytes must be positive"
        );
        ensure!(
            self.originals_max_days > 0 && self.originals_max_days <= u64::MAX / 86_400_000,
            "originals_max_days must be positive and fit milliseconds"
        );
        Ok(())
    }
    pub fn excludes(&self, executable: &str) -> bool {
        let basename = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
        self.exclude_commands.iter().any(|s| s == basename)
    }
    /// Explicit creation only; never overwrites even corrupt existing settings.
    pub fn create_default(path: &Path) -> Result<()> {
        private_dir(path.parent().context("config path has no parent")?)?;
        let mut file = private_open(path, false, true)?;
        serde_json::to_writer_pretty(&mut file, &Self::default())?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}
pub fn config_dir() -> Result<PathBuf> {
    directory(true)
}
pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.json"))
}
pub fn state_dir() -> Result<PathBuf> {
    directory(false)
}
fn directory(config: bool) -> Result<PathBuf> {
    if let Some(path) = env_path(if config {
        "RETOK_CONFIG_DIR"
    } else {
        "RETOK_STATE_DIR"
    }) {
        return Ok(path);
    }
    #[cfg(windows)]
    {
        Ok(env_path(if config { "APPDATA" } else { "LOCALAPPDATA" })
            .context("Windows application data directory unavailable")?
            .join("Retok"))
    }
    #[cfg(not(windows))]
    {
        if let Some(path) = env_path(if config {
            "XDG_CONFIG_HOME"
        } else {
            "XDG_STATE_HOME"
        }) {
            return Ok(path.join("retok"));
        }
        Ok(env_path("HOME")
            .context("HOME unavailable; set RETOK_CONFIG_DIR and RETOK_STATE_DIR")?
            .join(if config {
                ".config/retok"
            } else {
                ".local/state/retok"
            }))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
// Stored records may add metadata without changing Event literal callers.
pub struct Event {
    pub unix_millis: u64,
    /// Basename or category only, never a command line.
    pub command: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub source: Option<String>,
    pub original_id: Option<String>,
}
/// A persisted event with optional project identity. Legacy records have no project.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordedEvent {
    #[serde(flatten)]
    pub event: Event,
    #[serde(default)]
    pub project: Option<String>,
}
impl std::ops::Deref for RecordedEvent {
    type Target = Event;
    fn deref(&self) -> &Event {
        &self.event
    }
}
/// Canonical checkout root (including linked worktrees), or the directory itself
/// outside Git. No subprocesses or repository metadata writes are involved.
pub fn project_at(path: &Path) -> Result<String> {
    let path = fs::canonicalize(path).context("project directory is unavailable")?;
    ensure!(path.is_dir(), "project must be a directory");
    let root = path
        .ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap_or(&path);
    Ok(root
        .to_str()
        .context("project path must be UTF-8")?
        .to_owned())
}
fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
fn validate(event: &Event) -> Result<()> {
    ensure!(
        valid_label(&event.command) && event.source.as_deref().is_none_or(valid_label),
        "command and source must be short basenames/categories, without arguments"
    );
    ensure!(
        event.original_id.as_deref().is_none_or(valid_id),
        "invalid original ID"
    );
    Ok(())
}
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
/// record_usage governs all storage; enabled remains a caller output decision.
/// Originals are independently gated by the persisted keep_originals setting.
/// Recording is best effort: a busy state lock skips this event and its originals.
pub fn record(event: Event, originals: Option<(&[u8], &[u8])>) -> Result<()> {
    let settings = Settings::load()?;
    if !settings.record_usage {
        return Ok(());
    }
    let project = std::env::current_dir()
        .ok()
        .and_then(|p| project_at(&p).ok());
    match project {
        Some(project) => {
            record_project_at(&state_dir()?, &settings, event, originals, Some(&project))
        }
        None => record_at(&state_dir()?, &settings, event, originals),
    }
}
pub fn record_at(
    dir: &Path,
    settings: &Settings,
    event: Event,
    originals: Option<(&[u8], &[u8])>,
) -> Result<()> {
    record_project_at(dir, settings, event, originals, None)
}
/// Explicit project identity for callers whose command ran outside Retok's cwd.
/// Use project_at to normalize an existing directory before recording/querying.
pub fn record_project_at(
    dir: &Path,
    settings: &Settings,
    mut event: Event,
    originals: Option<(&[u8], &[u8])>,
    project: Option<&str>,
) -> Result<()> {
    if !settings.record_usage {
        return Ok(());
    }
    settings.validate()?;
    ensure!(
        project.is_none_or(|p| !p.is_empty() && p.len() <= 4096 && !p.contains('\0')),
        "invalid project identity"
    );
    validate(&event)?;
    let Some(_lock) = try_lock(dir)? else {
        return Ok(());
    };
    // IDs are assigned here only after both streams have been saved successfully.
    event.original_id = None;
    if settings.keep_originals
        && let Some((stdout, stderr)) = originals
        && stdout.len() as u64 + stderr.len() as u64
            <= settings.originals_max_bytes.min(ORIGINAL_LIMIT)
    {
        // An oversized original is not stored, but its measured event still is.
        let originals_dir = dir.join("originals");
        private_dir(&originals_dir)?;
        let id = save_original(&originals_dir, stdout, stderr)?;
        evict_originals(&originals_dir, &id, settings)?;
        event.original_id = Some(id);
    }
    let mut bytes = serde_json::to_vec(&RecordedEvent {
        event,
        project: project.map(str::to_owned),
    })?;
    bytes.push(b'\n');
    let path = dir.join("metrics.jsonl");
    let size = match fs::symlink_metadata(&path) {
        Ok(meta) => {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "metrics must be a regular file"
            );
            meta.len()
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.into()),
    };
    // Preserve an interrupted append as a malformed record rather than joining
    // the next valid event onto it. Never rewrite or discard the damaged bytes.
    let needs_separator = if size > 0 {
        let mut file = File::open(&path)?;
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        file.read_exact(&mut last)?;
        last[0] != b'\n'
    } else {
        false
    };
    let rotate = size + bytes.len() as u64 + u64::from(needs_separator) > METRICS_LIMIT;
    if rotate {
        let backup = dir.join("metrics.1.jsonl");
        remove_if_exists(&backup)?;
        fs::rename(&path, backup)?;
    }
    let mut file = private_open(&path, true, false)?;
    if needs_separator && !rotate {
        file.write_all(b"\n")?;
    }
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "Retok directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn private_open(path: &Path, append: bool, new: bool) -> Result<File> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Retok state path must be a regular file"
        );
    }
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(!new)
        .create_new(new)
        .append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}
fn lock(dir: &Path) -> Result<File> {
    private_dir(dir)?;
    let file = private_open(&dir.join("state.lock"), false, false)?;
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}
fn try_lock(dir: &Path) -> Result<Option<File>> {
    private_dir(dir)?;
    let file = private_open(&dir.join("state.lock"), false, false)?;
    // Brief contention (including descriptors inherited across fork/exec) need
    // not lose an event, but optional metrics must never wait indefinitely.
    for attempt in 0..=20 {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(Some(file)),
            Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                if attempt == 20 {
                    return Ok(None);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}
fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn save_original(dir: &Path, stdout: &[u8], stderr: &[u8]) -> Result<String> {
    // Publish only after both streams are complete. A later locked scan removes
    // pending directories left behind by process interruption.
    for _ in 0..100 {
        let id = format!(
            "{:x}-{:x}-{:x}",
            unix_millis(),
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let destination = dir.join(&id);
        if fs::symlink_metadata(&destination).is_ok() {
            continue;
        }
        let entry = dir.join(format!("{id}.pending"));
        #[cfg(unix)]
        let mut builder = fs::DirBuilder::new();
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&entry) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
            Ok(()) => {}
        }
        let result = (|| -> Result<()> {
            private_open(&entry.join("stdout"), false, true)?.write_all(stdout)?;
            private_open(&entry.join("stderr"), false, true)?.write_all(stderr)?;
            fs::rename(&entry, &destination)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&entry);
            return Err(error);
        }
        return Ok(id);
    }
    bail!("could not allocate unique original ID")
}
#[derive(Debug, Serialize)]
pub struct Original {
    pub id: String,
    pub bytes: u64,
    pub unix_millis: u64,
}
fn list_originals_unlocked(dir: &Path) -> Result<Vec<Original>> {
    if let Ok(meta) = fs::symlink_metadata(dir.join("originals")) {
        ensure!(
            !meta.file_type().is_symlink(),
            "original directory must not be a symlink"
        );
    }
    let entries = match fs::read_dir(dir.join("originals")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut result = Vec::new();
    'entries: for entry in entries {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() && id.strip_suffix(".pending").is_some_and(valid_id) {
            fs::remove_dir_all(entry.path())?;
            continue;
        }
        if !valid_id(&id) || !entry.file_type()?.is_dir() {
            continue;
        }
        let mut bytes = 0;
        for stream in ["stdout", "stderr"] {
            let meta = match fs::symlink_metadata(entry.path().join(stream)) {
                Ok(meta) => meta,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    // Older writers published the directory before both files.
                    // All writers hold this lock, so a missing stream is abandoned.
                    fs::remove_dir_all(entry.path())?;
                    continue 'entries;
                }
                Err(error) => return Err(error.into()),
            };
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "original stream must be a regular file"
            );
            bytes += meta.len();
        }
        let timestamp = entry
            .metadata()?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        result.push(Original {
            id,
            bytes,
            unix_millis: timestamp,
        });
    }
    result.sort_by(|a, b| a.unix_millis.cmp(&b.unix_millis).then(a.id.cmp(&b.id)));
    Ok(result)
}
fn evict_originals(dir: &Path, keep: &str, settings: &Settings) -> Result<()> {
    let entries = list_originals_unlocked(dir.parent().context("missing state directory")?)?;
    let mut count = entries.len();
    let mut size: u64 = entries.iter().map(|e| e.bytes).sum();
    for entry in entries {
        if entry.id != keep
            && (count > settings.originals_max_entries
                || size > settings.originals_max_bytes
                || unix_millis().saturating_sub(entry.unix_millis)
                    > settings.originals_max_days * 86_400_000)
        {
            fs::remove_dir_all(dir.join(entry.id))?;
            count -= 1;
            size -= entry.bytes;
        }
    }
    Ok(())
}
pub fn list_originals_at(dir: &Path) -> Result<Vec<Original>> {
    let _lock = lock(dir)?;
    list_originals_unlocked(dir)
}
/// Default options recover the entire stream byte-for-byte. Navigation is
/// bounded to 200 matching lines by default, at most 10,000 when requested.
#[derive(Default, Debug)]
pub struct RecallOptions {
    pub stderr: bool,
    /// One-based physical line at which searching starts.
    pub from: Option<usize>,
    /// Maximum returned lines, after literal grep filtering.
    pub lines: Option<usize>,
    pub grep: Option<String>,
}
pub fn recall_at(dir: &Path, id: &str, stderr: bool, output: &mut impl Write) -> Result<()> {
    recall_with_options_at(
        dir,
        id,
        &RecallOptions {
            stderr,
            ..Default::default()
        },
        output,
    )
}
pub fn recall_with_options_at(
    dir: &Path,
    id: &str,
    options: &RecallOptions,
    output: &mut impl Write,
) -> Result<()> {
    ensure!(valid_id(id), "invalid original ID");
    ensure!(
        options.from != Some(0),
        "--from must be a positive one-based line"
    );
    ensure!(
        options.lines.is_none_or(|n| (1..=10_000).contains(&n)),
        "--lines must be between 1 and 10000"
    );
    let _lock = lock(dir)?;
    let mut entry = dir.join("originals").join(id);
    if fs::symlink_metadata(&entry).is_err() {
        let matches: Vec<_> = list_originals_unlocked(dir)?
            .into_iter()
            .filter(|entry| entry.id.starts_with(id))
            .collect();
        ensure!(
            !matches.is_empty(),
            "original ID not found (it may have expired)"
        );
        ensure!(
            matches.len() == 1,
            "ambiguous original ID prefix; use a longer prefix"
        );
        entry = dir.join("originals").join(&matches[0].id);
    }
    ensure!(
        !fs::symlink_metadata(dir.join("originals"))?
            .file_type()
            .is_symlink()
            && !fs::symlink_metadata(&entry)?.file_type().is_symlink(),
        "original directory must not be a symlink"
    );
    let path = entry.join(if options.stderr { "stderr" } else { "stdout" });
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= ORIGINAL_LIMIT,
        "invalid original stream"
    );
    let mut file = File::open(path)?.take(ORIGINAL_LIMIT);
    // An open handle keeps this stream readable if retention unlinks it. Never
    // hold the shared state lock while waiting for the recall consumer.
    drop(_lock);
    if options.from.is_none() && options.lines.is_none() && options.grep.is_none() {
        io::copy(&mut file, output)?;
    } else {
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut number = 0;
        let mut returned = 0;
        while reader.read_until(b'\n', &mut line)? != 0 {
            number += 1;
            let matches = options.grep.as_ref().is_none_or(|pattern| {
                pattern.is_empty()
                    || line
                        .windows(pattern.len())
                        .any(|part| part == pattern.as_bytes())
            });
            if number >= options.from.unwrap_or(1) && matches {
                output.write_all(&line)?;
                returned += 1;
                if returned >= options.lines.unwrap_or(200) {
                    break;
                }
            }
            line.clear();
        }
    }
    Ok(())
}

#[derive(Default, Debug, Serialize)]
pub struct Usage {
    pub events: Vec<RecordedEvent>,
    pub malformed_records: u64,
}
pub fn usage_at(dir: &Path) -> Result<Usage> {
    let _lock = lock(dir)?;
    let mut usage = Usage::default();
    for name in ["metrics.1.jsonl", "metrics.jsonl"] {
        let file = match File::open(dir.join(name)) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        // Reads are bounded even if an external editor corrupts the log.
        ensure!(
            file.metadata()?.len() <= METRICS_LIMIT,
            "metrics file exceeds 10 MiB limit"
        );
        for line in BufReader::new(file).split(b'\n') {
            match serde_json::from_slice::<RecordedEvent>(&line?) {
                Ok(event) if validate(&event).is_ok() => usage.events.push(event),
                _ => usage.malformed_records += 1,
            }
        }
    }
    Ok(usage)
}
#[derive(Default, Debug, Serialize)]
pub struct Totals {
    pub events: u64,
    pub measured_events: u64,
    pub input_tokens: u128,
    pub output_tokens: u128,
    pub saved_tokens: i128,
    pub input_bytes: u128,
    pub output_bytes: u128,
}
impl Totals {
    fn add(&mut self, event: &Event) {
        self.events += 1;
        self.input_bytes += event.input_bytes as u128;
        self.output_bytes += event.output_bytes as u128;
        if let (Some(input), Some(output)) = (event.input_tokens, event.output_tokens) {
            self.measured_events += 1;
            self.input_tokens += input as u128;
            self.output_tokens += output as u128;
            self.saved_tokens += input as i128 - output as i128;
        }
    }
}
impl Usage {
    pub fn totals(&self) -> Totals {
        let mut total = Totals::default();
        for event in &self.events {
            total.add(event);
        }
        total
    }
}
/// All filters are intersected. Times are Unix milliseconds, inclusive since,
/// exclusive until. Project/command/source comparisons are exact.
#[derive(Default, Debug)]
pub struct UsageQuery {
    pub project: Option<String>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub command: Option<String>,
    pub source: Option<String>,
}
pub fn query_at(dir: &Path, query: &UsageQuery) -> Result<Usage> {
    ensure!(
        query.since.zip(query.until).is_none_or(|(a, b)| a < b),
        "--since must precede --until"
    );
    let mut usage = usage_at(dir)?;
    usage.events.retain(|event| {
        query
            .project
            .as_ref()
            .is_none_or(|p| event.project.as_ref() == Some(p))
            && query.command.as_ref().is_none_or(|c| &event.command == c)
            && query
                .source
                .as_ref()
                .is_none_or(|s| event.source.as_ref() == Some(s))
            && query.since.is_none_or(|t| event.unix_millis >= t)
            && query.until.is_none_or(|t| event.unix_millis < t)
    });
    Ok(usage)
}
pub fn reset_at(dir: &Path) -> Result<()> {
    let _lock = lock(dir)?;
    for name in ["metrics.jsonl", "metrics.1.jsonl"] {
        remove_if_exists(&dir.join(name))?;
    }
    Ok(())
}
fn args_utf8(args: &[OsString]) -> Result<Vec<&str>> {
    args.iter()
        .map(|s| s.to_str().context("options must be UTF-8"))
        .collect()
}
// Gregorian leap years repeat every 400 years (146097 days). Skip whole
// cycles from 1970, then at most 399 years and eleven months; no locale or TZ.
fn utc_date(epoch_days: u64) -> String {
    let mut year = 1970 + epoch_days / 146097 * 400;
    let mut days = epoch_days % 146097;
    let leap = |year: u64| {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    };
    loop {
        let length = if leap(year) { 366 } else { 365 };
        if days < length {
            break;
        }
        days -= length;
        year += 1;
    }
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while days >= months[month] {
        days -= months[month];
        month += 1;
    }
    format!("{year:04}-{:02}-{:02}", month + 1, days + 1)
}
const GAIN_HELP: &str = "Usage: retok gain [--json|--csv|--format text|json|csv] [--history]
       [--daily] [--weekly] [--monthly] [--graph] [--project [PATH]]
       [--since TIME] [--until TIME] [--command NAME] [--source NAME]
       retok gain --reset
Show measured token savings from retained records (all projects by default).
--project defaults to the current checkout; legacy records have no project.
TIME is YYYY-MM-DD at UTC midnight or Unix milliseconds; since includes, until excludes.
Weeks start Monday UTC. --graph uses daily buckets unless a period is selected.
CSV exports totals, selected periods, or --history records. --reset clears metrics only.";
const RECALL_HELP: &str = "Usage: retok recall --list
       retok recall ID-OR-PREFIX [--stderr] [--from LINE] [--lines COUNT] [--grep TEXT]
Without navigation, writes the entire saved stream as raw bytes.
--from is one-based; --grep is literal and case-sensitive; --lines limits matches.
Navigation returns at most 200 lines by default (maximum --lines 10000).
Only opt-in saved originals are available; commands are never rerun.";
const CONFIG_HELP: &str = "Usage: retok config [show|--create]
Show effective settings. --create writes defaults only if no config exists.
Set originals_max_entries, originals_max_bytes (total), and originals_max_days in config.json.
Caps must be positive; originals are saved only when keep_originals is true.
Oversized originals are skipped; measurements are still recorded.";
fn help(args: &[OsString], text: &str, output: &mut impl Write) -> Result<bool> {
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        writeln!(output, "{text}")?;
        return Ok(true);
    }
    Ok(false)
}
pub fn gain(args: &[OsString]) -> Result<()> {
    if help(args, GAIN_HELP, &mut io::stdout().lock())? {
        return Ok(());
    }
    gain_at(&state_dir()?, args, &mut io::stdout().lock())
}
fn parse_time(value: &str) -> Result<u64> {
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return value.parse().context("invalid Unix milliseconds");
    }
    let parts: Vec<_> = value.split('-').collect();
    ensure!(
        parts.len() == 3 && parts[0].len() == 4 && parts[1].len() == 2 && parts[2].len() == 2,
        "time must be YYYY-MM-DD or Unix milliseconds"
    );
    let year: u64 = parts[0].parse().context("invalid year")?;
    let month: usize = parts[1].parse().context("invalid month")?;
    let day: u64 = parts[2].parse().context("invalid day")?;
    ensure!(
        (1970..=9999).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day),
        "invalid UTC date"
    );
    let leap_days = |y: u64| y / 4 - y / 100 + y / 400;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let days = (year - 1970) * 365 + leap_days(year - 1) - leap_days(1969)
        + months[..month - 1].iter().sum::<u64>()
        + day
        - 1;
    ensure!(utc_date(days) == value, "invalid UTC date");
    Ok(days * 86_400_000)
}
fn period_start(millis: u64, period: &str) -> String {
    let days = millis / 86_400_000;
    match period {
        "weekly" if days < 4 => "1969-12-29".into(),
        "weekly" => utc_date(days - (days + 3) % 7),
        "monthly" => utc_date(days).rsplit_once('-').unwrap().0.to_owned(),
        _ => utc_date(days),
    }
}
fn csv_row(output: &mut impl Write, fields: &[String]) -> Result<()> {
    for (index, field) in fields.iter().enumerate() {
        if index != 0 {
            write!(output, ",")?;
        }
        if field.contains([',', '"', '\r', '\n']) {
            write!(output, "\"{}\"", field.replace('"', "\"\""))?;
        } else {
            write!(output, "{field}")?;
        }
    }
    write!(output, "\r\n")?;
    Ok(())
}
fn csv_totals(
    output: &mut impl Write,
    period: &str,
    start: &str,
    total: &Totals,
    malformed: u64,
) -> Result<()> {
    csv_row(
        output,
        &[
            period.into(),
            start.into(),
            total.events.to_string(),
            total.measured_events.to_string(),
            total.input_tokens.to_string(),
            total.output_tokens.to_string(),
            total.saved_tokens.to_string(),
            total.input_bytes.to_string(),
            total.output_bytes.to_string(),
            malformed.to_string(),
        ],
    )
}
pub fn gain_at(dir: &Path, args: &[OsString], output: &mut impl Write) -> Result<()> {
    if help(args, GAIN_HELP, output)? {
        return Ok(());
    }
    let args = args_utf8(args)?;
    if args.contains(&"--reset") {
        ensure!(args.len() == 1, "--reset must be used alone");
        reset_at(dir)?;
        writeln!(output, "Usage metrics reset; saved originals retained.")?;
        return Ok(());
    }
    let mut query = UsageQuery::default();
    let mut format = None;
    let mut history = false;
    let mut graph = false;
    let mut periods = Vec::new();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg {
            "--json" | "--csv" | "--format" => {
                let value = match arg {
                    "--json" => "json",
                    "--csv" => "csv",
                    _ => args
                        .next()
                        .context("--format requires text, json, or csv")?,
                };
                ensure!(
                    ["text", "json", "csv"].contains(&value),
                    "unknown gain format {value}"
                );
                ensure!(
                    format.is_none_or(|f| f == value),
                    "choose one output format"
                );
                format = Some(value);
            }
            "--history" => history = true,
            "--graph" => graph = true,
            "--daily" | "--weekly" | "--monthly" => {
                if !periods.contains(&&arg[2..]) {
                    periods.push(&arg[2..]);
                }
            }
            "--project" => {
                let path = if args.peek().is_some_and(|s| !s.starts_with("--")) {
                    PathBuf::from(args.next().unwrap())
                } else {
                    std::env::current_dir()?
                };
                query.project = Some(project_at(&path)?);
            }
            "--since" => {
                query.since = Some(parse_time(args.next().context("--since requires a time")?)?)
            }
            "--until" => {
                query.until = Some(parse_time(args.next().context("--until requires a time")?)?)
            }
            "--command" => {
                query.command = Some(
                    args.next()
                        .context("--command requires a basename/category")?
                        .to_owned(),
                )
            }
            "--source" => {
                query.source = Some(
                    args.next()
                        .context("--source requires a category")?
                        .to_owned(),
                )
            }
            _ => bail!("gain: unknown option {arg}; see gain --help"),
        }
    }
    ensure!(
        query.command.as_deref().is_none_or(valid_label)
            && query.source.as_deref().is_none_or(valid_label),
        "command/source filters must be exact basenames/categories"
    );
    if graph && periods.is_empty() {
        periods.push("daily");
    }
    let format = format.unwrap_or("text");
    ensure!(
        format != "csv" || !history || periods.is_empty(),
        "CSV history and period summaries must be exported separately"
    );
    let usage = query_at(dir, &query)?;
    let totals = usage.totals();
    let grouped: Vec<_> = periods
        .iter()
        .map(|period| {
            let mut buckets = BTreeMap::<String, Totals>::new();
            for event in &usage.events {
                buckets
                    .entry(period_start(event.unix_millis, period))
                    .or_default()
                    .add(event);
            }
            (*period, buckets)
        })
        .collect();
    if format == "json" {
        let mut value =
            serde_json::json!({"totals": totals, "malformed_records": usage.malformed_records});
        if history {
            value["history"] = serde_json::to_value(&usage.events)?;
        }
        for (period, buckets) in &grouped {
            value[format!("{period}_utc")] = serde_json::to_value(buckets)?;
        }
        serde_json::to_writer_pretty(&mut *output, &value)?;
        writeln!(output)?;
    } else if format == "csv" {
        if history {
            writeln!(
                output,
                "unix_millis,project,command,source,measured,input_tokens,output_tokens,saved_tokens,input_bytes,output_bytes,duration_ms,exit_code,original_id\r"
            )?;
            for event in &usage.events {
                let saved = event
                    .input_tokens
                    .zip(event.output_tokens)
                    .map(|(a, b)| (a as i128 - b as i128).to_string());
                csv_row(
                    output,
                    &[
                        event.unix_millis.to_string(),
                        event.project.clone().unwrap_or_default(),
                        event.command.clone(),
                        event.source.clone().unwrap_or_default(),
                        saved.is_some().to_string(),
                        event
                            .input_tokens
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                        event
                            .output_tokens
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                        saved.unwrap_or_default(),
                        event.input_bytes.to_string(),
                        event.output_bytes.to_string(),
                        event.duration_ms.to_string(),
                        event.exit_code.map(|n| n.to_string()).unwrap_or_default(),
                        event.original_id.clone().unwrap_or_default(),
                    ],
                )?;
            }
        } else {
            writeln!(
                output,
                "period,start,events,measured_events,input_tokens,output_tokens,saved_tokens,input_bytes,output_bytes,log_malformed_records\r"
            )?;
            if grouped.is_empty() {
                csv_totals(output, "all", "", &totals, usage.malformed_records)?;
            }
            for (period, buckets) in &grouped {
                for (start, total) in buckets {
                    csv_totals(output, period, start, total, usage.malformed_records)?;
                }
            }
        }
    } else {
        writeln!(
            output,
            "{} events; {} measured; {} input / {} output tokens; {} saved tokens",
            totals.events,
            totals.measured_events,
            totals.input_tokens,
            totals.output_tokens,
            totals.saved_tokens
        )?;
        writeln!(
            output,
            "Unmeasured events excluded from token totals. {} malformed records skipped.",
            usage.malformed_records
        )?;
        if history {
            for event in &usage.events {
                let tokens = match (event.input_tokens, event.output_tokens) {
                    (Some(input), Some(result)) => format!(
                        "{input} -> {result} tokens ({} saved)",
                        input as i128 - result as i128
                    ),
                    _ => "tokens unmeasured".to_owned(),
                };
                let seconds = event.unix_millis / 1000 % 86400;
                let exit = event
                    .exit_code
                    .map_or_else(|| "unknown".to_owned(), |code| code.to_string());
                writeln!(
                    output,
                    "{} {:02}:{:02}:{:02} UTC  {}  {tokens}; {} -> {} bytes; {} ms; exit {exit}; project {}; source {}; original {}",
                    utc_date(event.unix_millis / 86_400_000),
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60,
                    event.command,
                    event.input_bytes,
                    event.output_bytes,
                    event.duration_ms,
                    event.project.as_deref().unwrap_or("unknown"),
                    event.source.as_deref().unwrap_or("unknown"),
                    event.original_id.as_deref().unwrap_or("none")
                )?;
            }
        }
        for (period, buckets) in grouped {
            writeln!(output, "UTC {period}: saved tokens")?;
            let max = buckets
                .values()
                .map(|t| t.saved_tokens.max(0))
                .max()
                .unwrap_or(0)
                .max(1);
            for (start, total) in buckets {
                let graph = if graph {
                    "#".repeat((total.saved_tokens.max(0) * 40 / max) as usize)
                } else {
                    String::new()
                };
                writeln!(output, "{start}: {} {graph}", total.saved_tokens)?;
            }
        }
    }
    Ok(())
}
pub fn recall(args: &[OsString]) -> Result<()> {
    if help(args, RECALL_HELP, &mut io::stdout().lock())? {
        return Ok(());
    }
    recall_command_at(&state_dir()?, args, &mut io::stdout().lock())
}
pub fn recall_command_at(dir: &Path, args: &[OsString], output: &mut impl Write) -> Result<()> {
    if help(args, RECALL_HELP, output)? {
        return Ok(());
    }
    let args = args_utf8(args)?;
    if args == ["--list"] {
        for entry in list_originals_at(dir)? {
            writeln!(
                output,
                "{}\t{} bytes\t{} unix ms",
                entry.id, entry.bytes, entry.unix_millis
            )?;
        }
        return Ok(());
    }
    let id = args.first().context("recall: expected --list or ID")?;
    let mut options = RecallOptions::default();
    let mut args = args[1..].iter().copied();
    while let Some(arg) = args.next() {
        match arg {
            "--stderr" => options.stderr = true,
            "--from" => {
                options.from = Some(
                    args.next()
                        .context("--from requires a line")?
                        .parse()
                        .context("invalid --from line")?,
                )
            }
            "--lines" => {
                options.lines = Some(
                    args.next()
                        .context("--lines requires a count")?
                        .parse()
                        .context("invalid --lines count")?,
                )
            }
            "--grep" => {
                options.grep = Some(
                    args.next()
                        .context("--grep requires a literal pattern")?
                        .to_owned(),
                )
            }
            _ => bail!("recall: unknown option {arg}"),
        }
    }
    if options.from.is_none() && options.lines.is_none() && options.grep.is_none() {
        recall_at(dir, id, options.stderr, output)
    } else {
        recall_with_options_at(dir, id, &options, output)
    }
}
pub fn config(args: &[OsString]) -> Result<()> {
    if help(args, CONFIG_HELP, &mut io::stdout().lock())? {
        return Ok(());
    }
    let args = args_utf8(args)?;
    ensure!(
        args.is_empty() || args == ["show"] || args == ["--create"],
        "config: expected show or --create"
    );
    let path = config_path()?;
    if args == ["--create"] {
        Settings::create_default(&path)?;
    }
    serde_json::to_writer_pretty(io::stdout().lock(), &Settings::load_from(&path)?)?;
    writeln!(io::stdout().lock())?;
    Ok(())
}
