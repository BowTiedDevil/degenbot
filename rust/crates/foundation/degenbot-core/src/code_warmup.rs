//! The pool-registration → JIT warmup port.
//!
//! A registered pool's runtime bytecode is immutable, and registration is the
//! one moment the process knows a pool it will simulate until it exits. Handing
//! that bytecode to the process-shared JIT worker pool at registration — rather
//! than waiting for the pool's first frame to miss — is the difference between
//! "the first sim finds a compiled program, or one in flight" and "the first
//! sim pays for a cold compile inside a block's latency budget".
//!
//! # Why a queue and not a call
//!
//! The JIT holder lives in `degenbot-simulation` (the engine), which reads
//! `degenbot-bot`'s state and therefore sits ABOVE the bot: the registration
//! path cannot name the holder without a dependency cycle. The handoff is
//! inverted through this foundation port instead:
//!
//! - registration — the producer — [`submit`]s each fresh pool's bytecode;
//! - the engine — the consumer — [`drain`]s the queue when it hands the runner
//!   to a per-block handle build, and asks the backend to compile every entry
//!   its tuning admits.
//!
//! The gap between production and consumption is unbounded by design (a boot
//! registers its whole tracked set before the first block's handle exists), so
//! the queue is bounded and deduped rather than unbounded: a submission past
//! either cap is REFUSED, and the JIT simply warms on first touch as it would
//! have without this port.
//!
//! # What lives here, and what does not
//!
//! Eligibility and compilation belong to the engine: `RuntimeTuning::should_compile`
//! (the bytecode-length cap that keeps a multi-second compile off a one-off
//! contract) and the backend's own admission path decide what actually
//! compiles, and the spec id the cache key needs is the engine's production
//! spec. This port carries bytecode plus its keccak-256 hash — the cache key's
//! hash half — and nothing else.
//!
//! A build without the engine's JIT feature never drains: the entries linger
//! (bounded by the caps below) and every frame interprets, which is exactly
//! what that build does for bytecode it has never seen.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use alloy::primitives::{keccak256, Bytes, B256};

/// One queued warmup: the runtime bytecode and the cache key's hash half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingCode {
    /// `keccak256` of [`code`](Self::code) — the `code_hash` half of the
    /// engine's `RuntimeCacheKey`. Carried rather than recomputed so the queue
    /// computes the hash once, at the submission that already paid for it.
    pub code_hash: B256,
    /// The pool contract's runtime bytecode, exactly as `eth_getCode` returned
    /// it.
    pub code: Bytes,
}

/// How many distinct programs the queue holds before it refuses new
/// submissions.
///
/// The producer's legitimate burst is one entry per pool in the tracked set,
/// which is a few hundred; a process that somehow queues two orders of
/// magnitude more is not warming a tracked set and must not grow the process's
/// footprint on the engine's behalf.
pub const MAX_PENDING_ENTRIES: usize = 4096;

/// How much queued bytecode the queue holds before it refuses new submissions.
///
/// At the tuning's 24 KiB per-program cap this is roughly 340 full-size
/// programs — far above a tracked set's worth of pool contracts, and small
/// enough that a producer bug cannot turn a boot into a memory incident.
pub const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

/// The process-wide queue. `LazyLock` because the entry map is not
/// `const`-constructible.
static QUEUE: LazyLock<Mutex<Queue>> = LazyLock::new(|| Mutex::new(Queue::default()));

/// The queue's interior: insertion-ordered-by-nothing (dispatch order is not a
/// priority signal — every entry is the same immutability fact) with a running
/// byte total so the cap check is O(1).
#[derive(Default)]
struct Queue {
    entries: HashMap<B256, Bytes>,
    bytes: usize,
}

impl Queue {
    /// Queue `bytecode`, or refuse it: an empty program is not worth a compile
    /// and cannot be a pool; an already-queued hash is the same program (a
    /// re-registration, or a shared implementation); a full queue costs the
    /// newest entry rather than the oldest, because the oldest is the one a
    /// boot's first drain is about to consume.
    fn offer(&mut self, bytecode: &[u8]) -> bool {
        if bytecode.is_empty() {
            return false;
        }
        let code_hash = keccak256(bytecode);
        if self.entries.contains_key(&code_hash)
            || self.entries.len() >= MAX_PENDING_ENTRIES
            || self.bytes.saturating_add(bytecode.len()) > MAX_PENDING_BYTES
        {
            return false;
        }
        self.bytes += bytecode.len();
        self.entries
            .insert(code_hash, Bytes::copy_from_slice(bytecode));
        true
    }

    /// Take every queued entry, leaving an empty queue behind (including the
    /// dedup and byte accounting: a program drained now may be queued again
    /// later, and the backend's admission path dedups a resident or in-flight
    /// program on its own).
    fn take(&mut self) -> Vec<PendingCode> {
        self.bytes = 0;
        self.entries
            .drain()
            .map(|(code_hash, code)| PendingCode { code_hash, code })
            .collect()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn byte_len(&self) -> usize {
        self.bytes
    }
}

/// The queue, ignoring a poisoned writer: the producer holds the lock only for
/// the map insert, so a panic elsewhere must not brick every later submission
/// (a warmup is never load-bearing).
fn queue() -> std::sync::MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Queue a freshly registered pool's runtime bytecode for the engine's JIT
/// drain. Returns whether the entry was queued (a refusal is not an error: the
/// program simply warms on first touch).
///
/// The CALLER owns the policy gate — an empty or over-cap program is refused
/// here, but "is the JIT armed in this process" (`simulation.sim_jit`) is the
/// submission site's check, because that is also what decides whether the
/// `eth_getCode` the submission costs is worth making.
#[must_use]
pub fn submit(bytecode: &[u8]) -> bool {
    queue().offer(bytecode)
}

/// Take every queued warmup, leaving the queue empty. The engine calls this
/// when it arms a per-block handle; a process with no armed JIT never calls it
/// and the caps above bound what the queue can hold.
#[must_use]
pub fn drain() -> Vec<PendingCode> {
    queue().take()
}

/// How many programs are queued — a telemetry/test probe; the engine learns the
/// same count from [`drain`]'s return.
#[must_use]
pub fn pending_len() -> usize {
    queue().len()
}

/// How many queued bytes are held — the companion probe for the
/// [`MAX_PENDING_BYTES`] cap.
#[must_use]
pub fn pending_bytes() -> usize {
    queue().byte_len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mechanics on a queue instance: dedup, the empty refusal, and both
    /// caps. Instance-level so these never race the process-wide queue that the
    /// wiring test below drains.
    #[test]
    fn a_queue_dedups_and_refuses_past_its_caps() {
        let mut queue = Queue::default();

        assert!(!queue.offer(&[]), "an empty program is not a warmup");
        assert!(queue.offer(&[0x00]));
        assert!(
            !queue.offer(&[0x00]),
            "the same program is queued once, however many pools share it"
        );
        assert!(queue.offer(&[0x01]));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.byte_len(), 2);

        // Entry cap: fill to the cap, then one more is refused.
        let mut queue = Queue::default();
        for index in 0..MAX_PENDING_ENTRIES {
            assert!(
                queue.offer(&index.to_be_bytes()),
                "the queue accepts up to its entry cap"
            );
        }
        assert!(!queue.offer(&[0xff, 0xff, 0xff]));
        assert_eq!(queue.len(), MAX_PENDING_ENTRIES);

        // Byte cap: a program that would exceed the byte budget is refused
        // while there is still room for a smaller one.
        let mut queue = Queue::default();
        let filler = vec![0x5b_u8; MAX_PENDING_BYTES - 1];
        assert!(queue.offer(&filler));
        assert!(
            !queue.offer(&[0x00, 0x01]),
            "two bytes past the byte cap is refused"
        );
        assert!(queue.offer(&[0x00]), "the last byte is still accepted");
    }

    /// The instance's take: everything comes out once, and the accounting
    /// resets so the program can be queued again later.
    #[test]
    fn a_queue_hands_every_entry_out_once() {
        let mut queue = Queue::default();
        let code = [0x60_u8, 0x00];
        assert!(queue.offer(&code));

        let taken = queue.take();
        assert_eq!(
            taken,
            vec![PendingCode {
                code_hash: keccak256(code),
                code: Bytes::copy_from_slice(&code),
            }]
        );
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.byte_len(), 0);
        assert!(queue.take().is_empty(), "a drained queue is empty");
        assert!(queue.offer(&code), "a drained program may be queued again");
    }

    /// The process-wide wiring the producer and the engine share: a submission
    /// is visible to the probes and leaves with the drain.
    #[test]
    fn the_process_queue_is_the_one_producers_and_the_engine_share() {
        let _ = drain();
        let code = [0x60_u8, 0x00, 0x60_u8, 0x00, 0xf3];
        assert!(submit(&code));
        assert_eq!(pending_len(), 1);
        assert_eq!(pending_bytes(), code.len());
        assert!(!submit(&code), "a queued program is not queued twice");

        let drained = drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].code_hash, keccak256(code));
        assert_eq!(drained[0].code.as_ref(), code);
        assert_eq!(pending_len(), 0);
        assert_eq!(pending_bytes(), 0);
    }
}
