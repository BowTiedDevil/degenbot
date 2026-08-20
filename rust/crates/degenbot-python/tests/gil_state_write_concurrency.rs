//! T1 (ergo plan /tmp/ergo-gil-fix-plan.md, incident 2026-08-20): the cycle
//! between a sync FFI pymethod that takes the BotState WRITE while holding
//! the GIL and a long-held BotState READ whose holder then wants the GIL.
//! Pre-fix: permanent inversion - everything freezes on the GIL futex (the
//! observed 'GIL deadlock'). Fixed: the write is acquired via `py.detach`,
//! the GIL is released while parked, the cycle cannot form.

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

type AttachResult = (std::result::Result<(), pyo3::PyErr>, thread::JoinHandle<()>);

const ADDR: &str = "0x0000000000000000000000000000000000000001";

#[test]
fn unregister_pool_write_does_not_invert_with_reader_gil() {
    // The cycle call runs on its own GIL-carrying thread; the test thread
    // joins with a deadline. On a red regression the work thread parks
    // inside the cycle, so a plain join would hang the suite: on timeout we
    // fail loudly and abort (a stuck non-daemon thread would otherwise
    // block test-binary exit too).
    let (tx, rx) = mpsc::channel::<AttachResult>();
    let work = thread::spawn(move || {
        let (res, holder): AttachResult = match pyo3::Python::attach(
            |py| -> std::result::Result<thread::JoinHandle<()>, pyo3::PyErr> {
                let bot = degenbot_rs::bot::PyBot::new(1);
                degenbot_rs::bot::test_gil::state_write_park_cycle(py, &bot, ADDR, 600)
            },
        ) {
            Ok(h) => (Ok(()), h),
            Err(e) => (Err(e), thread::spawn(|| ())),
        };
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
        Ok((Err(e), _)) => panic!("state_write_park_cycle failed (not the cycle): {e}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!(
                "T1 RED: GIL/BotState inversion - the pymethod's BotState write \
                 parked behind the reader while holding the GIL (incident \
                 2026-08-20). Every GIL waiter freezes: main asyncio, log \
                 drainer, gil-probe."
            );
            std::process::abort();
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("work thread died before sending"),
    }
}
