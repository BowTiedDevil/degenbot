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
use crate::arb_engine::seat_host::HostMsg;
use degenbot_workers::posture::{PostureCause, PostureChange, PostureOwner, ThrottleSample};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};
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
pub fn wake_hosts() {
    let mut wakers = wakers().lock();
    wakers.retain(|(_, tx)| tx.send(HostMsg::PostureEdge).is_ok());
}
/// Feed ONE throttle-poll delta to the ONE process-level posture owner and
/// wake the fleet hosts on a real (non-`Held`) transition. This is the ONLY
/// bot-side throttle feeder (TB4QGX T9): the pairing is mechanical, asserted
/// by `production_feeders_go_through_the_wrapper`.
pub fn feed_throttle(now_ms: u64, sample: ThrottleSample) {
    let change = degenbot_workers::posture::process().observe_throttle(now_ms, sample);
    if !matches!(change, PostureChange::Held) {
        wake_hosts();
    }
}
/// [`feed_throttle`]'s typed-cause twin against a caller-supplied owner
/// (`None` selects the process owner). Pairs the feed with the wake so a new
/// cause site cannot forget the `PostureEdge`.
pub fn feed_cause(owner: Option<&PostureOwner>, cause: PostureCause) {
    let change = match owner {
        Some(owner) => owner.observe_cause(cause),
        None => degenbot_workers::posture::process().observe_cause(cause),
    };
    if !matches!(change, PostureChange::Held) {
        wake_hosts();
    }
}
#[cfg(test)]
mod tests {
    use super::{deregister, register, wake_hosts, wakers};
    use crate::arb_engine::seat_host::HostMsg;
    use std::sync::mpsc;
    /// every bot-side production feeder MUST go through
    /// [`super::feed_throttle`]/[`super::feed_cause`], which pair `observe_*`
    /// with `wake_hosts`. FALSIFICATION: a raw
    /// `observe_throttle(`/`observe_cause(` in the non-test prefix of any
    /// other bot source file (test modules conventionally live at the end of
    /// a file, so the scan stops at the first `#[cfg(test)]`).
    #[test]
    #[expect(clippy::expect_used)]
    fn production_feeders_go_through_the_wrapper() {
        fn scan(dir: &std::path::Path, offenders: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).expect("read source dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    scan(&path, offenders);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs")
                    || path.file_name().and_then(|n| n.to_str()) == Some("fleet_wake.rs")
                {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read source file");
                let production = text.split("#[cfg(test)]").next().unwrap_or("");
                for needle in ["observe_throttle(", "observe_cause("] {
                    if production.contains(needle) {
                        offenders.push(format!("{}: {needle}", path.display()));
                    }
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        scan(&src, &mut offenders);
        assert!(
            offenders.is_empty(),
            "raw posture feeds bypass the wake pairing (use feed_throttle/feed_cause): {offenders:?}"
        );
    }
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
