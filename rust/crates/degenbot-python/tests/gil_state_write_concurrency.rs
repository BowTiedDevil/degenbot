//! T1 (ergo plan /tmp/ergo-gil-fix-plan.md, incident 2026-08-20 #1/#2):
//! the cycle between a sync FFI pymethod that takes the BotState WRITE while
//! holding the GIL and a long-held BotState READ whose holder then wants the
//! GIL. Pre-fix: permanent inversion - everything freezes on the GIL futex
//! (the observed 'GIL deadlock'). Fixed: the write is acquired via
//! `py.detach`, the GIL is released while parked, the cycle cannot form.
//!
//! #2 (this incident's re-audit) extended coverage:
//! - `update_v3_park_cycle`: the same cycle through the reserve/tick-update
//!   pymethods the first audit missed (build_v3_pool froze in exactly this
//!   class - a GIL-held registration write behind 26 fan-out readers).
//! - `no_gil_held_botstate_writes_in_bot_mod_rs`: source-scan guard so no
//!   future change re-introduces a GIL-held BotState write outside a
//!   `py.detach` scope (covers all incident sites, incl. ones too heavy for
//!   a unit repro).

#![cfg(feature = "auto-initialize")]
#![expect(
    clippy::panic,
    clippy::expect_used,
    clippy::doc_markdown,
    clippy::print_stderr
)]

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const ADDR: &str = "0x0000000000000000000000000000000000000001";

/// Reader-first cycle repro with a 15s outer deadline: on the red shape the
/// work thread parks inside the cycle forever, so a plain join would hang the
/// suite. On timeout we fail loudly + abort (the stuck non-daemon thread
/// would block test-binary exit anyway).
fn run_cycle_with_deadline(
    cycle: impl Fn(
            pyo3::Python<'_>,
            &degenbot_rs::bot::PyBot,
            &str,
        ) -> pyo3::PyResult<thread::JoinHandle<()>>
        + Send
        + 'static,
) {
    let (tx, rx) = mpsc::channel::<(pyo3::PyResult<()>, thread::JoinHandle<()>)>();
    let work = thread::spawn(move || {
        let (res, holder): (pyo3::PyResult<()>, thread::JoinHandle<()>) =
            pyo3::Python::attach(|py| -> (pyo3::PyResult<()>, thread::JoinHandle<()>) {
                let bot = degenbot_rs::bot::PyBot::new(1);
                match cycle(py, &bot, ADDR) {
                    Ok(h) => (Ok(()), h),
                    Err(err) => (Err(err), thread::spawn(|| ())),
                }
            });
        let _ = tx.send((res, holder));
    });
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok((Ok(()), holder)) => {
            holder
                .join()
                .expect("holder thread panicked (join is outside the GIL scope)");
            work.join()
                .expect("work thread panicked (it sent its result above)");
        }
        Ok((Err(e), holder)) => {
            let _ = holder.join(); // dummy join; the pymethod itself failed
            panic!("cycle helper failed (not the cycle): {e}");
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!("T1 RED: GIL/BotState inversion - a GIL-held pymethod's BotState write parked behind the reader (incidents 2026-08-20 #1/#2). Every GIL waiter freezes: main asyncio, log drainer, gil-probe.");
            std::process::abort();
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("work thread died before sending"),
    }
}

#[test]
fn unregister_pool_write_does_not_invert_with_reader_gil() {
    run_cycle_with_deadline(|py, bot, addr| {
        degenbot_rs::bot::test_gil::state_write_park_cycle(py, bot, addr, 600)
    });
}

#[test]
fn update_v3_pool_write_does_not_invert_with_reader_gil() {
    run_cycle_with_deadline(|py, bot, addr| {
        degenbot_rs::bot::test_gil::update_v3_park_cycle(py, bot, addr, 600)
    });
}

/// (The former mod.rs-only write scan is folded into the directory-walking
/// scan below: it covers writes as well as reads, in every file under src/bot/.)
/// Source guard for the 2026-08-21 run-9 cycle (KTXKUF/OB7UNY regression),
/// generalized from the pool.rs-only scan (ergo UX66EM/UTFQ4Q): NO file under
/// `src/bot/` may acquire a BotState/engine lock outside a `py.detach` scope —
/// new modules are covered automatically. The captured run-9 cycle: the
/// seed_genesis pymethod held the GIL while parked in core().write() ->
/// RawRwLock::wait_for_readers behind three live readers; every GIL waiter
/// (main loop, log drainer, probe) froze permanently. READS are the same
/// hazard class: a GIL-held read blocks the pump's per-log write
/// (log_dispatcher state.write()), and a reader that then wants the GIL
/// (result-channel anext, Python log forwarding) closes the cycle.
///
/// Each `.read()`/`.write()` line must have `py.detach` within the 8 preceding
/// lines, or carry a `T1-scan-exempt` marker (pure-Rust test seams only).
#[test]
fn no_gil_held_botstate_locks_in_bot_sources() {
    let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/bot");
    let mut files = Vec::<std::path::PathBuf>::new();
    collect_rs_files(std::path::Path::new(src_dir), &mut files);
    files.sort();
    assert!(
        files.len() >= 15,
        "scan found only {} files under src/bot - the layout moved; update this guard",
        files.len()
    );

    let mut checked_total = 0usize;
    let mut violations = Vec::<String>::new();
    for path in &files {
        let rel = path
            .strip_prefix(src_dir)
            .expect("path under src/bot")
            .to_string_lossy()
            .into_owned();
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        let lines: Vec<&str> = src.lines().collect();
        let mut checked_in_file = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('#') {
                continue;
            }
            if !line.contains(".read()") && !line.contains(".write()") {
                continue;
            }
            checked_in_file += 1;
            let lo = i.saturating_sub(8);
            let window: String = lines[lo..=i].join("\n");
            if !window.contains("py.detach")
                && !window.contains(".detach(")
                && !window.contains("T1-scan-exempt")
            {
                violations.push(format!("{rel}:{}: {}", i + 1, line.trim()));
            }
        }
        // Pattern-drift floors for the heaviest files: if these drop, the
        // invariant being scanned for has moved and this guard needs updating.
        match rel.as_str() {
            // pool.rs is near-zero by design since the accessor migration
            // (UX66EM/J2HPO4): only the sanctioned accessor bodies still name
            // `.read()`/`.write()` directly.
            "pool.rs" => assert!(
                (2..=10).contains(&checked_in_file),
                "pool.rs lock-site count {checked_in_file} outside the accessor-migration band - the pattern moved"
            ),
            // mod.rs is near-zero by design since the accessor migration
            // (UX66EM/3MXFTV): only the sanctioned accessor bodies + test
            // seams still name `.read()`/`.write()` directly.
            "mod.rs" => assert!(
                (2..=10).contains(&checked_in_file),
                "mod.rs lock-site count {checked_in_file} outside the accessor-migration band - the pattern moved"
            ),
            _ => {}
        }
        checked_total += checked_in_file;
    }
    assert!(
        checked_total >= 10,
        "scan found only {checked_total} BotState lock sites across src/bot - the pattern moved; update this guard"
    );
    let shown: String = violations
        .iter()
        .take(25)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(violations.is_empty(),
            "GIL-held BotState/engine locks outside py.detach scopes in src/bot (inversion class, incidents 2026-08-20 #1/#2 and 2026-08-21 run-9). Wrap each acquisition in py.detach(...) or mark pure-Rust test seams T1-scan-exempt. {} of {} violating: {}",
            violations.len(),
            checked_total,
            shown
    );
}

/// Recursively collect `.rs` files under `dir`.
fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}
