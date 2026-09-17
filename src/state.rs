//! Local-only settings and measured usage. Unix (including macOS) uses XDG paths.
//! Metrics rotate at 10 MiB, retaining one backup; the next rotation replaces it.
//! On original writes, evict oldest entries above 100 entries / 100 MiB / 30 days.
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
const ORIGINAL_AGE: u64 = 30 * 86400;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub record_usage: bool,
    pub keep_originals: bool,
    pub exclude_commands: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            record_usage: true,
            keep_originals: false,
            exclude_commands: Vec::new(),
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
                Ok(settings)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
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
#[serde(deny_unknown_fields)]
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
    record_at(&state_dir()?, &settings, event, originals)
}
pub fn record_at(
    dir: &Path,
    settings: &Settings,
    mut event: Event,
    originals: Option<(&[u8], &[u8])>,
) -> Result<()> {
    if !settings.record_usage {
        return Ok(());
    }
    validate(&event)?;
    let Some(_lock) = try_lock(dir)? else {
        return Ok(());
    };
    // IDs are assigned here only after both streams have been saved successfully.
    event.original_id = None;
    if settings.keep_originals
        && let Some((stdout, stderr)) = originals
    {
        ensure!(
            stdout.len() as u64 + stderr.len() as u64 <= ORIGINAL_LIMIT,
            "original exceeds 100 MiB limit"
        );
        let originals_dir = dir.join("originals");
        private_dir(&originals_dir)?;
        let id = save_original(&originals_dir, stdout, stderr)?;
        evict_originals(&originals_dir, &id)?;
        event.original_id = Some(id);
    }
    let mut bytes = serde_json::to_vec(&event)?;
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
fn evict_originals(dir: &Path, keep: &str) -> Result<()> {
    let entries = list_originals_unlocked(dir.parent().context("missing state directory")?)?;
    let mut count = entries.len();
    let mut size: u64 = entries.iter().map(|e| e.bytes).sum();
    for entry in entries {
        if entry.id != keep
            && (count > 100
                || size > ORIGINAL_LIMIT
                || unix_millis().saturating_sub(entry.unix_millis) > ORIGINAL_AGE * 1000)
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
pub fn recall_at(dir: &Path, id: &str, stderr: bool, output: &mut impl Write) -> Result<()> {
    ensure!(valid_id(id), "invalid original ID");
    let _lock = lock(dir)?;
    let entry = dir.join("originals").join(id);
    ensure!(
        !fs::symlink_metadata(dir.join("originals"))?
            .file_type()
            .is_symlink()
            && !fs::symlink_metadata(&entry)?.file_type().is_symlink(),
        "original directory must not be a symlink"
    );
    let path = entry.join(if stderr { "stderr" } else { "stdout" });
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= ORIGINAL_LIMIT,
        "invalid original stream"
    );
    let mut file = File::open(path)?.take(ORIGINAL_LIMIT);
    // An open handle keeps this stream readable if retention unlinks it. Never
    // hold the shared state lock while waiting for the recall consumer.
    drop(_lock);
    io::copy(&mut file, output)?;
    Ok(())
}

#[derive(Default, Debug, Serialize)]
pub struct Usage {
    pub events: Vec<Event>,
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
            match serde_json::from_slice::<Event>(&line?) {
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
const GAIN_HELP: &str = "Usage: retok gain [--json] [--history] [--daily] [--graph]
       retok gain --reset
Show measured token savings; daily dates are UTC. --reset clears metrics only.";
const RECALL_HELP: &str = "Usage: retok recall --list
       retok recall ID [--stderr]
List opt-in saved originals or write a saved stream as raw bytes.";
const CONFIG_HELP: &str = "Usage: retok config [show|--create]
Show effective settings. --create writes defaults only if no config exists.";
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
pub fn gain_at(dir: &Path, args: &[OsString], output: &mut impl Write) -> Result<()> {
    if help(args, GAIN_HELP, output)? {
        return Ok(());
    }
    let args = args_utf8(args)?;
    ensure!(
        args.iter()
            .all(|arg| ["--json", "--history", "--daily", "--graph", "--reset"].contains(arg)),
        "gain: expected --json, --history, --daily, --graph, or --reset"
    );
    if args.contains(&"--reset") {
        ensure!(args.len() == 1, "--reset must be used alone");
        reset_at(dir)?;
        writeln!(output, "Usage metrics reset; saved originals retained.")?;
        return Ok(());
    }
    let usage = usage_at(dir)?;
    let totals = usage.totals();
    let mut daily = BTreeMap::<u64, Totals>::new();
    for event in &usage.events {
        daily
            .entry(event.unix_millis / 86_400_000)
            .or_default()
            .add(event);
    }
    if args.contains(&"--json") {
        let mut value =
            serde_json::json!({"totals": totals, "malformed_records": usage.malformed_records});
        if args.contains(&"--history") {
            value["history"] = serde_json::to_value(&usage.events)?;
        }
        if args.contains(&"--daily") || args.contains(&"--graph") {
            value["daily_utc"] = serde_json::to_value(
                daily
                    .iter()
                    .map(|(day, total)| (utc_date(*day), total))
                    .collect::<BTreeMap<_, _>>(),
            )?;
        }
        serde_json::to_writer_pretty(&mut *output, &value)?;
        writeln!(output)?;
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
        if args.contains(&"--history") {
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
                    "{} {:02}:{:02}:{:02} UTC  {}  {tokens}; {} -> {} bytes; {} ms; exit {exit}",
                    utc_date(event.unix_millis / 86_400_000),
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60,
                    event.command,
                    event.input_bytes,
                    event.output_bytes,
                    event.duration_ms
                )?;
            }
        }
        if args.contains(&"--daily") || args.contains(&"--graph") {
            writeln!(output, "UTC date: saved tokens")?;
            let max = daily
                .values()
                .map(|t| t.saved_tokens.max(0))
                .max()
                .unwrap_or(0)
                .max(1);
            for (day, total) in daily {
                let graph = if args.contains(&"--graph") {
                    "#".repeat((total.saved_tokens.max(0) * 40 / max) as usize)
                } else {
                    String::new()
                };
                writeln!(output, "{}: {} {graph}", utc_date(day), total.saved_tokens)?;
            }
        }
    }
    Ok(())
}
pub fn recall(args: &[OsString]) -> Result<()> {
    if help(args, RECALL_HELP, &mut io::stdout().lock())? {
        return Ok(());
    }
    let args = args_utf8(args)?;
    let dir = state_dir()?;
    let mut output = io::stdout().lock();
    if args == ["--list"] {
        for entry in list_originals_at(&dir)? {
            writeln!(
                output,
                "{}\t{} bytes\t{} unix ms",
                entry.id, entry.bytes, entry.unix_millis
            )?;
        }
        return Ok(());
    }
    ensure!(
        args.len() == 1 || (args.len() == 2 && args[1] == "--stderr"),
        "recall: expected --list or ID [--stderr]"
    );
    recall_at(&dir, args[0], args.len() == 2, &mut output)
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
