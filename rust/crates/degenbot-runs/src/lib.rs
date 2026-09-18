//! Per-session run-artifact directories (engine-general, synchronous).
//!
//! A "run" is one process lifetime. [`RunDirectory::create`] resolves the
//! configured root (the typed `logging.runs_dir` key), makes
//! `<runs_dir>/<engine>/<yyyymmddTHHMMSSZ>-<pid>/`, and seeds an empty
//! `stdout.log` and `trace.jsonl`. A best-effort `latest` symlink beside the
//! session directory points at the newest one.
//!
//! # Deliberate non-goals
//!
//! No rotation, compression, or size caps. Run artifacts are the operator's
//! forensics surface; the deletion policy lives outside the process, and the
//! `latest` symlink keeps the newest session addressable without scanning.
//!
//! # Dependency posture
//!
//! Only `degenbot-config` (for the typed root) is mandatory. The
//! `tracing-subscriber` `MakeWriter` bridge lives behind the `tracing` feature
//! so a consumer that owns its own subscriber stays dependency-light. The
//! crate is `std`-only and synchronous; [`RunDirectory::create`] is safe to
//! call before any async runtime exists.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "tracing")]
mod writer;

#[cfg(feature = "tracing")]
pub use writer::{LogSink, LogWriter};

/// The process-wide default JSONL trace path, set once at boot by the engine
/// that owns a [`RunDirectory`]. It lets the (env-first) trace helpers default
/// to the session's `trace.jsonl` without threading a path through every call.
static TRACE_JSONL_DEFAULT: OnceLock<PathBuf> = OnceLock::new();

/// Install the default trace path. First caller wins; returns `false` when a
/// path was already installed (boot is the only intended caller).
pub fn set_trace_jsonl_default(path: PathBuf) -> bool {
    TRACE_JSONL_DEFAULT.set(path).is_ok()
}

/// The installed default trace path, if any.
#[must_use]
pub fn trace_jsonl_default() -> Option<&'static Path> {
    TRACE_JSONL_DEFAULT.get().map(PathBuf::as_path)
}

/// Resolve the JSONL trace path with explicit-override precedence: a
/// non-blank explicit value (`SIDECAR_TRACE_JSONL`) wins; otherwise the
/// installed [`trace_jsonl_default`]; otherwise `None` (capture disabled).
///
/// Blank/whitespace is treated as absent so an exported-but-empty variable
/// cannot silently become a path.
#[must_use]
pub fn resolve_trace_jsonl_path(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(value) = explicit.map(str::trim).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(value));
    }
    trace_jsonl_default().map(Path::to_path_buf)
}

/// One session's artifact directory.
#[derive(Debug, Clone)]
pub struct RunDirectory {
    engine_dir: PathBuf,
    session_dir: PathBuf,
    stdout_path: PathBuf,
    trace_jsonl_path: PathBuf,
}

impl RunDirectory {
    /// Create the session directory under the configured root.
    ///
    /// The root is the installed typed config's `logging.runs_dir`, or (when
    /// no config is installed) a freshly loaded standard config, resolved
    /// through the XDG state home / `~` expansion.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] when the configuration cannot be
    /// loaded or `engine` is not a single path component; filesystem errors
    /// keep their own kind (e.g. `AlreadyExists`, `PermissionDenied`).
    pub fn create(engine: &str) -> io::Result<Self> {
        let root = resolve_runs_root()?;
        Self::create_in(&root, engine)
    }

    /// Create the session directory under an explicit `root` (the test seam;
    /// production callers use [`Self::create`]).
    ///
    /// # Errors
    ///
    /// As [`Self::create`], plus filesystem errors from creating the engine
    /// and session directories or seeding the artifact files.
    pub fn create_in(root: &Path, engine: &str) -> io::Result<Self> {
        let engine = engine_component(engine)?;
        let engine_dir = root.join(engine);
        std::fs::create_dir_all(&engine_dir)?;

        let stamp = utc_stamp(SystemTime::now());
        let session_name = format!("{stamp}-{}", std::process::id());
        let session_dir = engine_dir.join(session_name);
        std::fs::create_dir(&session_dir)?;

        let stdout_path = session_dir.join("stdout.log");
        let trace_jsonl_path = session_dir.join("trace.jsonl");
        std::fs::File::create(&stdout_path)?;
        std::fs::File::create(&trace_jsonl_path)?;

        link_latest(&engine_dir, &session_dir);

        Ok(Self {
            engine_dir,
            session_dir,
            stdout_path,
            trace_jsonl_path,
        })
    }

    /// The session directory (`<runs_dir>/<engine>/<stamp>-<pid>`).
    #[must_use]
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// The engine directory holding the session directory and `latest`.
    #[must_use]
    pub fn engine_dir(&self) -> &Path {
        &self.engine_dir
    }

    /// The captured stdout log path.
    #[must_use]
    pub fn stdout_path(&self) -> &Path {
        &self.stdout_path
    }

    /// The JSONL trace path (the default for the trace helpers when
    /// `SIDECAR_TRACE_JSONL` is absent).
    #[must_use]
    pub fn trace_jsonl_path(&self) -> &Path {
        &self.trace_jsonl_path
    }

    /// A `MakeWriter` that appends the fmt subscriber's output to
    /// `stdout.log`. Falls back to stderr if the file cannot be opened, so a
    /// full disk never silences the process.
    #[cfg(feature = "tracing")]
    #[must_use]
    pub fn stdout_writer(&self) -> LogWriter {
        LogWriter::file(self.stdout_path.clone())
    }

    /// Like [`Self::stdout_writer`], additionally mirroring to stderr (the
    /// interactive `SIDECAR_LOG_STDERR=1` posture).
    #[cfg(feature = "tracing")]
    #[must_use]
    pub fn stdout_writer_tee(&self) -> LogWriter {
        LogWriter::tee(self.stdout_path.clone())
    }
}

/// The configured per-session run-artifacts root. The built-in default is
/// the XDG state home (`$XDG_STATE_HOME` when absolute, else
/// `$HOME/.local/state`); a `~`-carrying value is expanded against `HOME`.
///
/// Prefers a config already installed in the typed holder (a real boot), and
/// otherwise loads the standard file layer + `DEGENBOT_*` env through the
/// typed loader — the one sanctioned env reader.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidInput`] when the loader refuses the configuration.
pub fn resolve_runs_root() -> io::Result<PathBuf> {
    resolve_configured_path(|config| &config.logging.runs_dir)
}

/// The configured durable-state root (`persistence.state_dir`), resolved
/// through the same state-home/`~` expansion as [`resolve_runs_root`]. This
/// root is independent of [`resolve_runs_root`]: state here outlives a
/// session and is never nested under a per-session directory.
///
/// # Errors
///
/// As [`resolve_runs_root`].
pub fn resolve_state_root() -> io::Result<PathBuf> {
    resolve_configured_path(|config| &config.persistence.state_dir)
}

/// Resolve one state-rooted schema path field through the typed config and
/// expand it through the config env seam: a usable `$XDG_STATE_HOME` rebases
/// the `~/.local/state` default, and a leading `~` otherwise expands against
/// `HOME`.
fn resolve_configured_path(
    pick: impl Fn(&degenbot_config::BotConfig) -> &std::path::PathBuf,
) -> io::Result<PathBuf> {
    let configured = if degenbot_config::holder::installed() {
        pick(degenbot_config::holder::config()).clone()
    } else {
        let loaded = degenbot_config::BotConfigLoader::new()
            .with_standard_file_paths()
            .load()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
        pick(&loaded.config).clone()
    };
    // Only the built-in default is `$XDG_STATE_HOME`-rooted; an explicit
    // TOML/env value is the operator's literal and expands as written —
    // the same rule as the database-path seam's `Source::Default` arm.
    let is_default = *configured == *pick(&degenbot_config::BotConfig::default());
    let raw = configured.to_string_lossy().to_string();
    let expanded = if is_default {
        degenbot_config::expand_state_path(&raw)
    } else {
        degenbot_config::expand_tilde_path(&raw)
    };
    Ok(expanded)
}

/// Validate the engine name as exactly one relative path component (never a
/// separator, `.`, or `..`): the name is operator-supplied and becomes a
/// directory segment.
fn engine_component(engine: &str) -> io::Result<&str> {
    let trimmed = engine.trim();
    let is_single_component = !trimmed.is_empty()
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.contains('/')
        && !trimmed.contains('\\');
    if !is_single_component {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("engine name {engine:?} must be a single non-empty path component"),
        ));
    }
    Ok(trimmed)
}

/// Best-effort atomic `latest` symlink: a temporary link is renamed over the
/// previous one, so a reader never observes a dangling intermediate. Failure
/// is deliberately swallowed — a symlink problem must never abort the bot.
fn link_latest(engine_dir: &Path, session_dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let latest = engine_dir.join("latest");
        let tmp = engine_dir.join(format!(".latest.tmp.{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let target = session_dir
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("."));
        if symlink(target, &tmp).is_ok() {
            let _ = std::fs::rename(&tmp, &latest);
        }
        let _ = std::fs::remove_file(&tmp);
    }
    #[cfg(not(unix))]
    {
        let _ = (engine_dir, session_dir);
    }
}

/// The session directory stamp (`yyyymmddTHHMMSSZ`) for an instant.
fn utc_stamp(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    utc_stamp_from_unix_secs(secs)
}

/// Format unix seconds as a compact, sortable UTC stamp
/// (`20231114T221320Z`). Pure arithmetic so the crate stays `chrono`-free;
/// the civil-date conversion is Howard Hinnant's `civil_from_days`.
fn utc_stamp_from_unix_secs(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// Days since 1970-01-01 -> (year, month, day) in the proleptic Gregorian
/// calendar.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = u64::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::try_from(yoe).unwrap_or(0) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "test assertions fail loudly"
    )]

    use super::*;

    #[test]
    fn utc_stamp_is_sortable_and_correct() {
        assert_eq!(utc_stamp_from_unix_secs(0), "19700101T000000Z");
        assert_eq!(utc_stamp_from_unix_secs(1_700_000_000), "20231114T221320Z");
        assert_eq!(utc_stamp_from_unix_secs(1_699_999_999), "20231114T221319Z");
        // A leap day crosses the civil-date boundary correctly.
        assert_eq!(utc_stamp_from_unix_secs(1_709_164_800), "20240229T000000Z");
    }

    #[test]
    fn trace_jsonl_override_precedence() {
        assert!(
            set_trace_jsonl_default(PathBuf::from("/run/default/trace.jsonl")),
            "first install wins"
        );
        assert_eq!(
            resolve_trace_jsonl_path(Some("/captures/explicit.jsonl")),
            Some(PathBuf::from("/captures/explicit.jsonl")),
            "explicit override beats the run default"
        );
        assert_eq!(
            resolve_trace_jsonl_path(None),
            Some(PathBuf::from("/run/default/trace.jsonl")),
            "absent override falls back to the session trace"
        );
        assert_eq!(
            resolve_trace_jsonl_path(Some("   ")),
            Some(PathBuf::from("/run/default/trace.jsonl")),
            "blank override is absent, not an empty path"
        );
        assert!(!set_trace_jsonl_default(PathBuf::from("/other.jsonl")));
    }

    #[test]
    fn engine_component_rejects_path_escapes() {
        for bad in ["", "  ", ".", "..", "a/b", "a\\b"] {
            let err = engine_component(bad).expect_err("must reject");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        assert_eq!(
            engine_component(" backrun-sidecar ").unwrap(),
            "backrun-sidecar"
        );
    }
}
