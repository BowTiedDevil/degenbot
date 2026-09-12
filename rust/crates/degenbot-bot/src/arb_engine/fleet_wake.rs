//! Fleet host waker fan-out (TB4QGX T3) — the degenbot-bot side of the
//! `PostureEdge` hint.
//!
//! The ONE posture owner (`degenbot_workers::posture::process()`) publishes
//! an edge on a real (non-`Held`) transition. The owner lives in
//! `degenbot-workers` and cannot know about host channels (layering), so the
//! BOT-side feeder sites wake the hosts: the block-pump throttle feed and
//! every typed-cause feeder call [`wake_hosts`] on a non-`Held` change.
//! There is NO owner-side sender registry (a workshop non-goal) and NO new
//! thread — this IS the block-pump-fed waker.
//!
//! The hint is UNTRUSTED: a `PostureEdge` carries no posture value — the
//! receiving host re-reads the live owner in the pump that follows. A
//! spurious, lost, or coalesced hint is therefore benign, and the backstop
//! tick (T2) keeps the hint from ever being a precondition of progress.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};

use parking_lot::Mutex;

use crate::arb_engine::seat_host::HostMsg;

/// The registered host wake senders, keyed by token.
type WakerTable = Mutex<Vec<(u64, mpsc::Sender<HostMsg>)>>;

static WAKERS: OnceLock<WakerTable> = OnceLock::new();
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn wakers() -> &'static WakerTable {
    WAKERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a host's wake sender; returns the token [`deregister`] needs.
/// The sender is CLONED — the host keeps owning its original.
pub(crate) fn register(tx: &mpsc::Sender<HostMsg>) -> u64 {
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    wakers().lock().push((token, tx.clone()));
    token
}

/// Deregister a retired host (its channel is about to close).
pub(crate) fn deregister(token: u64) {
    wakers().lock().retain(|(t, _)| *t != token);
}

/// Wake every live host with an untrusted `PostureEdge` hint. Called by
/// every bot-side owner feeder on a non-`Held` transition. A disconnected
/// receiver is pruned (host retired).
pub(crate) fn wake_hosts() {
    let mut wakers = wakers().lock();
    wakers.retain(|(_, tx)| tx.send(HostMsg::PostureEdge).is_ok());
}

#[cfg(test)]
mod tests {
    use super::{deregister, register, wake_hosts, wakers};
    use crate::arb_engine::seat_host::HostMsg;
    use std::sync::mpsc;

    #[test]
    fn wake_fans_out_to_registered_hosts_and_prunes_retired_ones() {
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let ta = register(&tx_a);
        let tb = register(&tx_b);
        wake_hosts();
        assert!(matches!(rx_a.try_recv(), Ok(HostMsg::PostureEdge)));
        assert!(matches!(rx_b.try_recv(), Ok(HostMsg::PostureEdge)));

        deregister(ta);
        wake_hosts();
        assert!(
            rx_a.try_recv().is_err(),
            "a deregistered host must not be woken"
        );
        assert!(matches!(rx_b.try_recv(), Ok(HostMsg::PostureEdge)));

        // A dropped receiver is pruned by the next wake (no panic), and the
        // dead token disappears from the table.
        drop(rx_b);
        wake_hosts();
        assert!(
            !wakers().lock().iter().any(|(t, _)| *t == tb),
            "the dead sender was pruned"
        );
        deregister(tb);
    }
}
