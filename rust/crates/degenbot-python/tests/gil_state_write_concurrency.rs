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
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::doc_markdown,
    clippy::unused_self
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

/// Source guard: no BotState WRITE in `bot/mod.rs` may live outside a
/// `py.detach` scope - GIL-held writers parked behind the fan-out's reader
/// are the incident class (2026-08-20 #1: one write-futex waiter held the
/// GIL; #2: 26 readers + the V3 cold-build's GIL-held registration write).
/// Each `.write()` line must have `py.detach` within the 8 preceding lines
/// (the detach closures in this file are short).
#[test]
fn no_gil_held_botstate_writes_in_bot_mod_rs() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/bot/mod.rs");
    let src = std::fs::read_to_string(path).expect("read bot/mod.rs");
    let lines: Vec<&str> = src.lines().collect();
    let mut checked = 0;
    let mut violations = Vec::<String>::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let is_comment = trimmed.starts_with("//") || trimmed.starts_with("#");
        if !line.contains(".write()") || is_comment {
            continue;
        }
        checked += 1;
        let lo = i.saturating_sub(8);
        let window: String = lines[lo..=i].join(
            "
",
        );
        if !window.contains("py.detach")
            && !window.contains(".detach(")
            && !window.contains("T1-scan-exempt")
        {
            violations.push(format!("L{}: {}", i + 1, line.trim()));
        }
    }
    assert!(
        checked >= 15,
        "scan found only {checked} .write() sites - the pattern moved; update this guard"
    );
    if !violations.is_empty() {
        panic!("GIL-held BotState writes outside py.detach scopes in bot/mod.rs (inversion class, incidents 2026-08-20 #1/#2). Wrap each in py.detach(...) or mark pure-Rust test seams T1-scan-exempt: {}", violations.join(" | "));
    }
}

/// Source guard for the 2026-08-21 run-9 cycle (KTXKUF/OB7UNY regression):
/// the PyLiquidityPool and PyPool handle families take the BotState lock
/// across the GIL. The captured cycle: the seed_genesis pymethod held the
/// GIL while parked in core().write() -> RawRwLock::wait_for_readers behind
/// three live readers; every GIL waiter (main loop, log drainer, probe)
/// froze permanently. READS are the same hazard class: a GIL-held read
/// blocks the pump's per-log write (log_dispatcher state.write()), and a
/// reader that then wants the GIL (result-channel anext, Python log
/// forwarding) closes the cycle. Every BotState lock acquisition in
/// pool.rs must therefore live inside a py.detach scope (GIL released while
/// parked on the lock).
#[test]
fn no_gil_held_botstate_locks_in_pool_rs() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/bot/pool.rs");
    let src = std::fs::read_to_string(path).expect("read bot/pool.rs");
    let lines: Vec<&str> = src.lines().collect();
    let mut checked = 0;
    let mut violations = Vec::<String>::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let is_comment = trimmed.starts_with("//") || trimmed.starts_with("#");
        if is_comment {
            continue;
        }
        if !line.contains(".read()") && !line.contains(".write()") {
            continue;
        }
        checked += 1;
        let lo = i.saturating_sub(8);
        let window: String = lines[lo..=i].join(
            "
",
        );
        if !window.contains("py.detach")
            && !window.contains(".detach(")
            && !window.contains("T1-scan-exempt")
        {
            violations.push(format!("L{}: {}", i + 1, line.trim()));
        }
    }
    assert!(
        checked >= 100,
        "scan found only {checked} BotState lock sites in pool.rs - the pattern moved; update this guard"
    );
    if !violations.is_empty() {
        let shown: String = violations
            .iter()
            .cloned()
            .take(25)
            .collect::<Vec<_>>()
            .join(" | ");
        panic!(
            "GIL-held BotState locks outside py.detach scopes in pool.rs (inversion class, incident 2026-08-21 run-9: seed_genesis GIL-held write behind live readers). Wrap each acquisition in py.detach(...) or mark pure-Rust test seams T1-scan-exempt. {} of {} violating: {}",
            violations.len(),
            checked,
            shown
        );
    }
}
