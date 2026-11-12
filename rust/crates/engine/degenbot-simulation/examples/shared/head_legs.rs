//! The canonical-head lifecycle probe's CONTROL legs, shared so the probe and
//! its behavior pins drive the same code.
//!
//! # The fresh-head gate
//!
//! [`fresh_heads`] selects the fresh leg's window from the node's tip, and a zero
//! sample size is that knob's documented OFF switch: the window is empty and the
//! tip is never asked for, so a disabled leg selects no head, fetches no list,
//! and settles nothing.
//!
//! # The waiter leg
//!
//! [`waiter_leg`] walks the committed real-coverage capture's own heads through
//! [`BalHeadDriver`]. Per head it delivers, starts a CLAIMED advance over the
//! chain stub's controllably deferred transport, registers a
//! [`BalHeadDriver::wait_for_head`] against that live attempt, holds BOTH futures
//! parked, releases the list, and waits for the settle — so what it reports is a
//! real wait resolution, never a proxy.
//!
//! [`WaiterSample::net`] is that resolution with the prepare wall it overlapped
//! subtracted: the settle, the wake, and the wait's own re-read, with the fetch
//! and decode the wait joined never counted a second time. Both windows share
//! ONE clock origin, taken before the advance that carries the preparation is
//! constructed and first polled, so the prepare is a sub-interval of the wait
//! rather than a window that starts after it. The controlled parked window
//! enters both windows and so cancels out of the difference.
//!
//! The leg reads no node: the heads, hashes, lists, and relevance set come from
//! the committed capture, so the apply it times is production-shaped and the run
//! is deterministic. Timing is reported, never asserted; the leg fails only when
//! the lifecycle does not do what it claims — an advance that does not park, a
//! wait that does not resolve, a head that does not settle, or a sample whose
//! wait did not overlap the prepare it says it joined.

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::delta_snapshot::{
    BalHeadDriver, HeadCold, HeadOutcome, HeadReadiness,
};
use degenbot_simulation::sim::ReadAnchor;

use crate::chain_stub::ChainStub;
use crate::modeled_chain::{bal_bytes, real_block_writes};
use crate::real_coverage::RealCoverage;

/// The park guard a waiter sample's resolve is bounded by: a wait that never
/// resolves is a broken lifecycle, not a slow one.
const SETTLE_DEADLINE: Duration = Duration::from_secs(10);

/// A percentile of a duration series (nearest-rank over a sorted copy);
/// `percent` in 1..=100.
#[must_use]
pub fn percentile(series: &[Duration], percent: usize) -> Duration {
    let mut sorted = series.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

/// The fresh-head leg's window over the node's tip, oldest head first.
///
/// `fresh` is the sample size, and `0` DISABLES the leg: the answer is empty and
/// `tip` is never called, so a disabled leg selects no head and fetches none.
///
/// # Errors
///
/// Whatever asking the node for its tip head failed with.
pub async fn fresh_heads<F, Fut>(fresh: u64, tip: F) -> Result<Vec<u64>, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<u64, String>>,
{
    if fresh == 0 {
        return Ok(Vec::new());
    }
    let head = tip().await?;
    Ok((head.saturating_sub(fresh - 1)..=head).collect())
}

/// One head the waiter leg delivers: the exact anchor, and the parent hash the
/// scripted chain links it with.
pub struct ScheduledHead {
    /// The head's exact anchor — the real captured hash at the real height.
    pub anchor: ReadAnchor,
    /// The parent hash the transition's lineage is verified against.
    pub parent_hash: B256,
}

/// The offline chain the waiter leg walks: the chain stub serving the committed
/// capture's own lists at its own exact hashes, plus the heads to deliver.
pub struct WaiterChain {
    stub: ChainStub,
    real: RealCoverage,
    heads: Vec<ScheduledHead>,
}

impl WaiterChain {
    /// Script the first `limit` blocks of the capture's stretch, linked as the
    /// capture links them (each block's real hash, and its real predecessor's).
    ///
    /// A zero `limit` scripts nothing, so the leg walks no head.
    ///
    /// # Errors
    ///
    /// A capture that cannot be read, or a block whose hash or lineage it does
    /// not carry.
    pub fn load(capture: &str, limit: u64) -> Result<Self, String> {
        let real = RealCoverage::load(capture)?;
        let stub = ChainStub::serving();
        let first = real.first();
        let count = real
            .last()
            .saturating_sub(first)
            .saturating_add(1)
            .min(limit);
        let mut heads = Vec::new();
        for offset in 0..count {
            let number = first + offset;
            let hash = real.hash(number)?;
            let parent_hash = real.parent_hash(number)?;
            stub.serve_bal_at(hash, bal_bytes(&real_block_writes(first, number, &real)));
            heads.push(ScheduledHead {
                anchor: ReadAnchor::identified(number, hash),
                parent_hash,
            });
        }
        Ok(Self { stub, real, heads })
    }

    /// The provider the driver under test is built over: the scripted chain.
    #[must_use]
    pub fn provider(&self) -> AlloyProvider {
        self.stub.provider()
    }

    /// The heads the leg delivers, oldest first.
    #[must_use]
    pub fn heads(&self) -> &[ScheduledHead] {
        &self.heads
    }

    /// The caller's relevance set for these heads: every distinct address the
    /// capture recorded a write for, sorted — the same set the probe's live legs
    /// apply with, so every scripted write SEEDS instead of being skipped.
    #[must_use]
    pub fn relevance(&self) -> Vec<Address> {
        let mut addresses: Vec<Address> = (self.real.first()..=self.real.last())
            .flat_map(|number| {
                self.real
                    .writes_at(number)
                    .iter()
                    .map(|(address, _)| *address)
            })
            .collect();
        addresses.sort_unstable();
        addresses.dedup();
        addresses
    }

    /// The stub serving the scripted heads — the transport the leg defers and
    /// then releases.
    fn stub(&self) -> &ChainStub {
        &self.stub
    }
}

/// One waiter sample: one claimed advance over the deferred transport, and the
/// wait that joined it.
#[derive(Debug, Clone, Copy)]
pub struct WaiterSample {
    /// The head the sample delivered.
    pub head: u64,
    /// The wait's whole wall: from the sample's common clock origin, which
    /// precedes the advance's construction and first poll, to its terminal
    /// answer. Its own first poll is where it registers on the LIVE attempt.
    pub wait: Duration,
    /// The advance's own prepare wall — the fetch and full decode the wait
    /// overlapped, measured from that same origin (inside the advance's first
    /// poll), so it is a sub-interval of [`Self::wait`].
    pub prepare: Duration,
    /// The parked window the sample waited out on the closed gate: the control
    /// that makes the wait's registration observable before its notification.
    pub hold: Duration,
    /// [`Self::wait`] net of [`Self::prepare`]: what a caller that joined the
    /// preparation pays once that preparation finishes, plus the advance's own
    /// construction between the shared origin and the prepare's start.
    pub net: Duration,
    /// What the wait answered: `ready` for a carried settle, or the settled-cold
    /// reason the leg's first head reports (it has no parent to carry from).
    pub readiness: &'static str,
}

/// One poll of `future` with the caller's own waker, without awaiting it: the
/// observation that the future is genuinely in flight before the gate opens.
async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

/// The waiter leg: one claimed advance per scripted head, each joined by a wait
/// registered on the live attempt, releasing the deferred list only after both
/// are parked.
///
/// `hold` is the parked window both futures wait out; it is inside the wait and
/// inside the prepare alike, so it cancels out of [`WaiterSample::net`]. The
/// clock origin both samples share is taken before the advance is constructed,
/// so the wait always covers the whole prepare wall it subtracts.
///
/// # Errors
///
/// A head whose advance does not park on the deferred transport, whose wait
/// resolves before the preparation it joined finishes, whose wait never
/// resolves, whose advance does not settle, whose wait does not answer the
/// settled generation, or whose wait did not overlap the prepare it reports.
pub async fn waiter_leg(
    driver: &BalHeadDriver,
    chain: &WaiterChain,
    hold: Duration,
) -> Result<Vec<WaiterSample>, String> {
    let relevance = chain.relevance();
    let mut samples = Vec::new();
    for head in chain.heads() {
        let anchor = head.anchor;
        chain.stub().defer_bal();
        let ticket = driver.deliver(anchor, head.parent_hash);

        // The sample's ONE clock origin. The prepare clock starts inside the
        // advance's first poll below, so starting the wait before that future
        // even exists makes the prepare a sub-interval of the wait instead of a
        // window that begins after the wait has already been measured.
        let wait_started = Instant::now();

        // One poll: the advance mints its claim attempt and parks on the closed
        // transport gate, so the wait below joins a genuinely LIVE preparation.
        let mut advance = Box::pin(driver.advance(ticket, &relevance));
        if poll_once(advance.as_mut()).await.is_ready() {
            return Err(format!(
                "head {}: the deferred list did not park the advance",
                anchor.number
            ));
        }

        let mut waiter = Box::pin(driver.wait_for_head(anchor));
        if poll_once(waiter.as_mut()).await.is_ready() {
            return Err(format!(
                "head {}: the wait resolved before the preparation it joined finished",
                anchor.number
            ));
        }

        // Both futures are parked: the advance on the transport gate, the wait
        // on the live attempt's completion. Only the release answers the fetch.
        tokio::time::sleep(hold).await;
        chain.stub().release_bal();

        let (outcome, view) =
            tokio::time::timeout(SETTLE_DEADLINE, async { tokio::join!(advance, waiter) })
                .await
                .map_err(|_| format!("head {}: the wait never resolved", anchor.number))?;
        let wait = wait_started.elapsed();

        let HeadOutcome::Settled(report) = outcome else {
            return Err(format!(
                "head {}: the claimed advance did not settle: {outcome:?}",
                anchor.number
            ));
        };
        // The wait answers the head's SETTLED generation: ready when the parent
        // chain carried, or the settled-cold reason of the leg's first head (no
        // parent to carry from) — never unprepared, abandoned, or superseded.
        let readiness = view
            .readiness
            .cold_reason()
            .map_or("ready", HeadCold::label);
        if !matches!(
            view.readiness,
            HeadReadiness::Ready | HeadReadiness::Cold(HeadCold::Settled { .. })
        ) {
            return Err(format!(
                "head {}: the wait answered {readiness} ({:?}), not the settled generation",
                anchor.number, view.readiness
            ));
        }
        let prepare = report.bal_latency;
        let Some(net) = wait.checked_sub(prepare) else {
            return Err(format!(
                "head {}: the wait ({wait:?}) is shorter than the prepare wall \
                 ({prepare:?}) it overlapped, so the sample measured no resolution",
                anchor.number
            ));
        };
        samples.push(WaiterSample {
            head: anchor.number,
            wait,
            prepare,
            hold,
            net,
            readiness,
        });
    }
    Ok(samples)
}
