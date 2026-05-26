//! Structured JSON audit log (PRD §6.2 E1.8 · §8.2).
//!
//! Emits exactly one JSON event per security-relevant action — config init,
//! DB create/drop/list, ingestion, and query — to a configurable sink
//! (stderr / file / syslog). Events carry **no secrets**: the optional error
//! message is passed through [`crate::secrets::redact`] before it is recorded,
//! as defence-in-depth on top of the redaction already applied to errors.
//!
//! Configuration is read from the environment so auditing is available even for
//! `beskar init`, which runs before any config file exists:
//!
//! * `BESKAR_AUDIT_SINK` — `off` (default), `stderr`, `file`, or `syslog`.
//! * `BESKAR_AUDIT_FILE` — path for the `file` sink. Setting this alone selects
//!   the `file` sink without needing `BESKAR_AUDIT_SINK`.
//!
//! The emitted schema is documented in `docs/audit-log.md` and validated in CI.

use std::io::Write as _;

use serde::Serialize;

use crate::secrets;

/// Stable schema version for the emitted JSON event. Bump on breaking changes;
/// keep in sync with `docs/audit-log.md`.
const SCHEMA_VERSION: &str = "1";

/// Whether the audited action succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Failure => "failure",
        }
    }
}

/// One audit event. Serialized with `serde_json`, so the on-wire JSON is always
/// well-formed. `target`/`error` are omitted when absent rather than emitted as
/// `null`, keeping events compact for SIEM ingestion.
#[derive(Serialize)]
struct Event<'a> {
    schema_version: &'a str,
    timestamp: String,
    actor: String,
    host: String,
    pid: u32,
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
    outcome: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Where audit events are written.
#[derive(Clone, Debug)]
enum Sink {
    Off,
    Stderr,
    File(String),
    Syslog,
}

/// An audit logger resolved from the environment. Cheap to clone and `Off` by
/// default, so existing non-enterprise usage is unaffected until a sink is set.
#[derive(Clone, Debug)]
pub struct Logger {
    sink: Sink,
}

impl Logger {
    /// Resolve the sink from `BESKAR_AUDIT_SINK` / `BESKAR_AUDIT_FILE`. An
    /// unparseable or unsatisfiable configuration warns and disables auditing
    /// rather than failing the command.
    pub fn from_env() -> Self {
        let file = std::env::var("BESKAR_AUDIT_FILE")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let sink = match std::env::var("BESKAR_AUDIT_SINK")
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("stderr") => Sink::Stderr,
            Some("syslog") => Sink::Syslog,
            Some("file") => match file {
                Some(path) => Sink::File(path),
                None => {
                    eprintln!(
                        "warning: BESKAR_AUDIT_SINK=file but BESKAR_AUDIT_FILE is unset; \
                         audit logging disabled"
                    );
                    Sink::Off
                }
            },
            Some("off") | Some("") | None => match file {
                // A file path alone is enough to enable the file sink.
                Some(path) => Sink::File(path),
                None => Sink::Off,
            },
            Some(other) => {
                eprintln!(
                    "warning: unknown BESKAR_AUDIT_SINK='{other}' \
                     (expected off|stderr|file|syslog); audit logging disabled"
                );
                Sink::Off
            }
        };

        Self { sink }
    }

    /// Record the outcome of an `anyhow` result, capturing a redacted error
    /// message on failure. This is the entry point used by `main`.
    pub fn record_result<T>(
        &self,
        command: &str,
        target: Option<&str>,
        result: &anyhow::Result<T>,
    ) {
        match result {
            Ok(_) => self.record(command, target, Outcome::Success, None),
            Err(e) => self.record(command, target, Outcome::Failure, Some(&format!("{e:#}"))),
        }
    }

    /// Like [`Logger::record_result`], but attributes the event to an explicit
    /// `actor` (e.g. the authenticated identity behind a `beskar serve` request,
    /// PRD §5.6) instead of the local OS user. `None` falls back to the OS user.
    pub fn record_result_as<T>(
        &self,
        command: &str,
        actor_override: Option<&str>,
        target: Option<&str>,
        result: &anyhow::Result<T>,
    ) {
        match result {
            Ok(_) => self.emit(command, actor_override, target, Outcome::Success, None),
            Err(e) => self.emit(command, actor_override, target, Outcome::Failure, Some(&format!("{e:#}"))),
        }
    }

    /// Emit one event. Never fails the command: serialization or sink errors are
    /// reported to stderr and otherwise swallowed.
    pub fn record(
        &self,
        command: &str,
        target: Option<&str>,
        outcome: Outcome,
        error: Option<&str>,
    ) {
        self.emit(command, None, target, outcome, error);
    }

    fn emit(
        &self,
        command: &str,
        actor_override: Option<&str>,
        target: Option<&str>,
        outcome: Outcome,
        error: Option<&str>,
    ) {
        if matches!(self.sink, Sink::Off) {
            return;
        }
        let event = Event {
            schema_version: SCHEMA_VERSION,
            timestamp: now_rfc3339(),
            actor: actor_override.map(str::to_string).unwrap_or_else(actor),
            host: host(),
            pid: std::process::id(),
            command,
            target,
            outcome: outcome.as_str(),
            error: error.map(secrets::redact),
        };
        let line = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: failed to serialize audit event: {e}");
                return;
            }
        };
        if let Err(e) = self.write(&line) {
            eprintln!("warning: failed to write audit event: {e}");
        }
    }

    fn write(&self, line: &str) -> std::io::Result<()> {
        match &self.sink {
            Sink::Off => Ok(()),
            Sink::Stderr => writeln!(std::io::stderr().lock(), "{line}"),
            Sink::File(path) => {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                writeln!(f, "{line}")
            }
            Sink::Syslog => write_syslog(line),
        }
    }
}

/// Best-effort identity of the actor, from the environment. Never a secret.
fn actor() -> String {
    for var in ["USER", "USERNAME", "LOGNAME"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    "unknown".to_string()
}

/// Best-effort hostname, from the environment or `/etc/hostname` on unix.
fn host() -> String {
    for var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    #[cfg(unix)]
    {
        if let Ok(v) = std::fs::read_to_string("/etc/hostname") {
            let v = v.trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    "unknown".to_string()
}

/// Format the current UTC time as RFC 3339 with millisecond precision, e.g.
/// `2026-05-25T12:34:56.789Z`. Implemented without a date dependency: see
/// [`civil_from_days`].
fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hour, min, sec) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
}

/// Convert a count of days since the Unix epoch (1970-01-01) into a civil
/// `(year, month, day)`. Howard Hinnant's well-known `civil_from_days`
/// algorithm (public domain), valid for the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m: i64 = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d)
}

/// Emit an RFC 5424 syslog datagram to the local syslog socket. Falls back to
/// stderr if no socket accepts the message, so an event is never silently lost.
#[cfg(unix)]
fn write_syslog(line: &str) -> std::io::Result<()> {
    use std::os::unix::net::UnixDatagram;
    // facility = local0 (16), severity = informational (6): PRI = 16*8 + 6.
    const PRI: u8 = 134;
    let msg = format!(
        "<{PRI}>1 {ts} {host} beskar {pid} audit - {line}",
        ts = now_rfc3339(),
        host = host(),
        pid = std::process::id(),
    );
    if let Ok(sock) = UnixDatagram::unbound() {
        for path in ["/dev/log", "/var/run/syslog"] {
            if sock.send_to(msg.as_bytes(), path).is_ok() {
                return Ok(());
            }
        }
    }
    writeln!(std::io::stderr().lock(), "{line}")
}

/// No native syslog on this platform; fall back to stderr.
#[cfg(not(unix))]
fn write_syslog(line: &str) -> std::io::Result<()> {
    writeln!(std::io::stderr().lock(), "{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-02-29 (leap day): 951782400 / 86400 = 11016 days.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        // 2023-11-14: 1700000000 / 86400 = 19675 days (22:13:20 UTC remainder).
        assert_eq!(civil_from_days(19_675), (2023, 11, 14));
        // A pre-epoch date exercises the negative-era branch.
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn timestamp_is_rfc3339_utc_shaped() {
        let ts = now_rfc3339();
        // YYYY-MM-DDTHH:MM:SS.mmmZ
        assert_eq!(ts.len(), 24, "unexpected timestamp: {ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn off_logger_writes_nothing() {
        let logger = Logger { sink: Sink::Off };
        assert!(!logger.enabled_for_test());
        // Should be a no-op and not panic.
        logger.record("db", Some("notes"), Outcome::Success, None);
    }

    #[test]
    fn event_omits_absent_optional_fields() {
        let event = Event {
            schema_version: SCHEMA_VERSION,
            timestamp: "2026-05-25T00:00:00.000Z".to_string(),
            actor: "tester".into(),
            host: "h".into(),
            pid: 42,
            command: "db",
            target: None,
            outcome: Outcome::Success.as_str(),
            error: None,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(v["schema_version"], "1");
        assert_eq!(v["command"], "db");
        assert_eq!(v["outcome"], "success");
        assert!(v.get("target").is_none());
        assert!(v.get("error").is_none());
    }

    #[test]
    fn file_sink_writes_one_redacted_failure_event() {
        let path = std::env::temp_dir().join(format!("beskar-audit-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        secrets::register_secret("TOPSECRETtoken12345");

        let logger = Logger {
            sink: Sink::File(path.display().to_string()),
        };
        logger.record(
            "generate",
            Some("notes"),
            Outcome::Failure,
            Some("request failed: token=TOPSECRETtoken12345"),
        );

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1, "expected exactly one event");
        assert!(
            !contents.contains("TOPSECRETtoken12345"),
            "secret leaked: {contents}"
        );
        assert!(contents.contains("REDACTED"));

        let v: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(v["command"], "generate");
        assert_eq!(v["target"], "notes");
        assert_eq!(v["outcome"], "failure");
        let _ = std::fs::remove_file(&path);
    }
}

impl Logger {
    /// Test-only visibility into whether a sink is configured.
    #[cfg(test)]
    fn enabled_for_test(&self) -> bool {
        !matches!(self.sink, Sink::Off)
    }
}
