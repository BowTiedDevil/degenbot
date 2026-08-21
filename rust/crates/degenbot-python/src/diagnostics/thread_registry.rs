//! Thread-identity registry: maps each thread's std thread-id to its OS TID
//! (+ the last span it created), so a GIL-deadlock dump can be cross-
//! referenced against the /proc futex table (ergo TPMFLV / incident
//! 2026-08-20).
//!
//! The registry fills passively: `PythonLogLayer::on_new_span` (present in
//! every registry stack) calls [`note_current_thread`] on the span-CREATING
//! thread. The watchdog (never GIL) calls [`dump_to_file`] on a confirmed
//! GIL-deadlock alarm, writing a `std-thread-id -> os_tid + last span` map joined
//! with every thread's /proc state + waited futex to a JSON file. Std
//! Standard thread-ids are the same ids that appear as the `thread.id` span tag
//! on `OTel` spans (scrape from Jaeger to join a span to an OS TID).
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct ThreadInfo {
    os_tid: u64,
    name: String,
    last_span: Option<String>,
    last_span_loc: Option<String>,
    last_span_ns: u128,
}

fn registry() -> &'static Mutex<HashMap<std::thread::ThreadId, ThreadInfo>> {
    static REGISTRY: OnceLock<Mutex<HashMap<std::thread::ThreadId, ThreadInfo>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mono_ns() -> u128 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_nanos()
}

/// Read the calling thread's OS TID via `/proc/thread-self/stat` (no libc
/// dep; only hit on each thread's FIRST span, then cached).
fn read_os_tid() -> u64 {
    match std::fs::read_to_string("/proc/thread-self/stat") {
        Ok(s) => s
            .split_whitespace()
            .next()
            .and_then(|t| t.parse().ok())
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// Record `span_name` as the last span created by the CALLING thread.
/// Cheap: one map lookup; the /proc read happens once per thread lifetime.
pub(crate) fn note_current_thread(span_name: &str, span_loc: Option<&str>) {
    if span_name.is_empty() {
        return;
    }
    let tid = std::thread::current().id();
    let now = mono_ns();
    let mut map = match registry().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let entry = map.entry(tid).or_insert_with(|| ThreadInfo {
        os_tid: read_os_tid(),
        name: std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_string(),
        last_span: None,
        last_span_loc: None,
        last_span_ns: 0,
    });
    entry.last_span = Some(span_name.to_string());
    if let Some(l) = span_loc {
        entry.last_span_loc = Some(l.to_string());
    }
    entry.last_span_ns = now;
}

/// Dump the thread registry + the /proc futex table to a JSON file. Returns
/// the path on success. Designed for the watchdog thread (never takes the
/// GIL; safe to call during a permanent GIL deadlock).
pub fn dump_to_file() -> Option<std::path::PathBuf> {
    let pid = std::process::id();
    let path = std::env::var("DEGENBOT_THREAD_REGISTRY_PATH")
        .unwrap_or_else(|_| format!("/tmp/degenbot-thread-registry-{pid}.json"));

    let mut os_threads: Vec<serde_json::Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
        for entry in entries.flatten() {
            let tid = entry.file_name().to_string_lossy().into_owned();
            let comm = std::fs::read(format!("/proc/self/task/{tid}/comm"))
                .ok()
                .map(|b| {
                    String::from_utf8_lossy(&b)
                        .trim_matches(|c| c == '\0' || c == '\n')
                        .to_string()
                })
                .unwrap_or_default();
            let state = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat"))
                .ok()
                .and_then(|s| {
                    s.split_once(')')
                        .and_then(|(_, rest)| rest.trim_start().chars().next())
                })
                .unwrap_or('?');
            // syscall file: "NR ARG1 ARG2 ..." (202 = futex; ARG1 = addr)
            // when the thread is blocked in a syscall; empty when running.
            let (syscall_nr, futex_addr) =
                match std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall")) {
                    Ok(s) => {
                        let mut it = s.split_whitespace();
                        match it.next().and_then(|t| t.parse::<u64>().ok()) {
                            Some(nr) => (Some(nr), it.next().map(str::to_string)),
                            None => (None, None),
                        }
                    }
                    Err(_) => (None, None),
                };
            os_threads.push(serde_json::json!({
                "os_tid": tid,
                "comm": comm,
                "state": state,
                "syscall_nr": syscall_nr,
                "futex_addr": futex_addr,
            }));
        }
    }

    let span_threads: Vec<serde_json::Value> = registry()
        .lock()
        .map(|map| {
            map.iter()
                .map(|(id, info)| {
                    serde_json::json!({
                        "std_thread_id": format!("{id:?}"),
                        "os_tid": info.os_tid.to_string(),
                        "name": info.name,
                        "last_span": info.last_span,
                        "last_span_loc": info.last_span_loc,
                        "idle_ns": mono_ns().saturating_sub(info.last_span_ns),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let doc = serde_json::json!({
        "pid": pid,
        "captured_at_ns": mono_ns(),
        "note": "std_thread_id matches the OTel span 'thread.id' tag; join os_threads (what each thread waits on) with span_threads (what each thread last did).",
        "os_threads": os_threads,
        "span_threads": span_threads,
    });

    match serde_json::to_string(&doc) {
        Ok(s) => match std::fs::write(&path, s) {
            Ok(()) => Some(std::path::PathBuf::from(path)),
            Err(e) => {
                tracing::error!(error = %e, "[thread-registry] dump write failed");
                None
            }
        },
        Err(e) => {
            tracing::error!(error = %e, "[thread-registry] dump serialize failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_then_dump_captures_the_calling_thread() {
        note_current_thread("test.span", Some("tests.rs:42"));
        let map = registry().lock().unwrap();
        let entry = map
            .get(&std::thread::current().id())
            .expect("the calling thread must be registered");
        assert_eq!(entry.last_span.as_deref(), Some("test.span"));
        assert_eq!(entry.last_span_loc.as_deref(), Some("tests.rs:42"));
        // /proc read must work in this environment (CI = Linux);
        // off-Linux the fallback is 0.
        if std::env::consts::OS == "linux" {
            assert!(entry.os_tid > 0, "os_tid must be non-zero on Linux");
        }
    }
}
