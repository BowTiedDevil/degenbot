//! The scripted-head fixture: ONE place where a head's
//! deliver → prepare → apply → release choreography is spelled, so the
//! lifecycle, pipeline, and relevance pins drive the same phase code instead of
//! restating it.
//!
//! # What it owns
//!
//! [`ScriptedHead`] is a head delivered to a real [`BalHeadDriver`] over a
//! scripted chain: [`ScriptedHead::deliver`] records it, [`ScriptedHead::settle`]
//! runs the whole background phase, and the stages are available on their own
//! ([`ScriptedHead::prepared`], [`ScriptedHead::apply`]) for a pin that needs to
//! inspect the world between them. The claim choreography is
//! [`ScriptedHead::claimed`]: the advance is claimed and parked at its first
//! pending poll, so a test holds a genuinely live preparation and
//! [`ParkedAdvance::release`] is the cancellation every "a dropped preparation
//! releases its claim" pin needs.
//!
//! [`SpawnQueue`] is the same fixture at the PIPELINE seam: the pipeline's
//! producer edge must never await a BAL, so every phase it spawns parks here and
//! the test decides when — and in which order — a head's preparation lands.
//!
//! Nothing here dials a node, sleeps, or races a wall clock: the chain is the
//! suite's scripted stub and the phases are drained explicitly.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use alloy::primitives::{Address, B256};
use futures_util::future::BoxFuture;

use degenbot_simulation::sim::evm2::bal_heads::HeadSpawn;
use degenbot_simulation::sim::evm2::delta_snapshot::{
    BalHeadDriver, DeltaReport, HeadOutcome, HeadReadiness, HeadTicket, HeadView,
};
use degenbot_simulation::sim::evm2::read_hub::PreparedBal;
use degenbot_simulation::sim::ReadAnchor;

/// A hash no scripted chain links to: the parent of a session's first head.
pub fn genesis() -> B256 {
    B256::new([0x99; 32])
}

/// An address distinct from the fixtures' other identities.
pub fn address(tag: u8) -> Address {
    Address::new([tag; 20])
}

/// One exact-hash anchor.
pub fn identified(number: u64, hash: B256) -> ReadAnchor {
    ReadAnchor::identified(number, hash)
}

/// The settled report of one successful apply.
pub fn settled(outcome: &HeadOutcome) -> Arc<DeltaReport> {
    match outcome {
        HeadOutcome::Settled(report) => Arc::clone(report),
        other => panic!("expected a settled head, got {other:?}"),
    }
}

/// Poll `future` exactly once with a no-op waker: `Poll::Pending` means it is
/// genuinely waiting on something the test controls.
pub fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
}

/// Assert the parked future is still genuinely waiting: the one choreography
/// assertion a claim, a wait, or a parked spawn queue entry needs.
pub fn assert_parked<F: Future + Unpin>(future: &mut F) {
    assert!(
        poll_once(future).is_pending(),
        "a parked phase is still waiting on what the test controls"
    );
}

/// One head's deliver → prepare → apply → release choreography over one
/// [`BalHeadDriver`].
pub struct ScriptedHead<'a> {
    driver: &'a BalHeadDriver,
    anchor: ReadAnchor,
    ticket: HeadTicket,
}

impl<'a> ScriptedHead<'a> {
    /// Deliver `anchor` with `parent_hash`, and hold the ticket it minted.
    pub fn deliver(driver: &'a BalHeadDriver, anchor: ReadAnchor, parent_hash: B256) -> Self {
        let ticket = driver.deliver(anchor, parent_hash);
        Self {
            driver,
            anchor,
            ticket,
        }
    }

    /// [`Self::deliver`] for a head named by number and hash.
    pub fn at(driver: &'a BalHeadDriver, number: u64, hash: B256, parent_hash: B256) -> Self {
        Self::deliver(driver, identified(number, hash), parent_hash)
    }

    /// Continue the choreography of an already-delivered ticket.
    pub fn of(driver: &'a BalHeadDriver, ticket: HeadTicket) -> Self {
        Self {
            driver,
            anchor: ticket.anchor(),
            ticket,
        }
    }

    /// The exact-hash anchor this head was delivered at.
    pub fn anchor(&self) -> ReadAnchor {
        self.anchor
    }

    /// The delivery ticket a preparation is applied against.
    pub fn ticket(&self) -> HeadTicket {
        self.ticket
    }

    /// The driver the head was delivered to.
    pub fn driver(&self) -> &'a BalHeadDriver {
        self.driver
    }

    /// The prepare phase alone: fetch and fully decode this head's list without
    /// moving any snapshot state.
    pub async fn prepared(&self) -> PreparedBal {
        self.driver.prepare(self.ticket).await
    }

    /// The apply phase of an already-prepared list.
    pub fn apply(&self, prepared: &PreparedBal, relevance: &[Address]) -> HeadOutcome {
        self.driver.apply(self.ticket, prepared, relevance)
    }

    /// The whole background phase: prepare, then apply.
    pub async fn advance(&self, relevance: &[Address]) -> HeadOutcome {
        self.driver.advance(self.ticket, relevance).await
    }

    /// The whole background phase, asserted to settle.
    pub async fn settle(&self, relevance: &[Address]) -> Arc<DeltaReport> {
        let outcome = self.advance(relevance).await;
        settled(&outcome)
    }

    /// Claim this head's preparation: the advance is claimed and parked at its
    /// first pending poll, so the test holds a genuinely live attempt.
    pub fn claimed(&self) -> ParkedAdvance<'a> {
        ParkedAdvance::claim(self.driver, self.ticket, &[])
    }

    /// [`Self::claimed`] with the relevance the apply will read.
    pub fn claimed_with(&self, relevance: &'a [Address]) -> ParkedAdvance<'a> {
        ParkedAdvance::claim(self.driver, self.ticket, relevance)
    }

    /// The view this head answers with right now.
    pub fn view(&self) -> HeadView {
        self.driver.snapshot(self.anchor)
    }

    /// The readiness this head reports right now.
    pub fn readiness(&self) -> HeadReadiness {
        self.view().readiness
    }
}

/// A CLAIMED preparation held at its first pending poll — the pose a cancelled
/// advance leaves behind, and the attempt a waiter joins.
pub struct ParkedAdvance<'a> {
    advance: Pin<Box<dyn Future<Output = HeadOutcome> + 'a>>,
}

impl<'a> ParkedAdvance<'a> {
    /// Claim `ticket`'s head and park the advance on its fetch.
    pub fn claim(driver: &'a BalHeadDriver, ticket: HeadTicket, relevance: &'a [Address]) -> Self {
        let mut advance = Box::pin(driver.advance(ticket, relevance));
        assert!(
            poll_once(&mut advance).is_pending(),
            "a claimed preparation parks on its fetch"
        );
        Self { advance }
    }

    /// Whether the held preparation is still waiting on the scripted chain.
    pub fn is_pending(&mut self) -> bool {
        poll_once(&mut self.advance).is_pending()
    }

    /// The still-parked advance, for a test that drives it itself.
    pub fn into_future(self) -> Pin<Box<dyn Future<Output = HeadOutcome> + 'a>> {
        self.advance
    }

    /// Drop the claim: the cancellation a later retry prepares the head past.
    pub fn release(self) {
        drop(self);
    }

    /// Let the released list land and finish the advance the claim took.
    pub async fn finish(self) -> HeadOutcome {
        self.advance.await
    }
}

/// The deterministic spawn seam: every phase a [`BalHeadPipeline`] spawns parks
/// here until the test drains it, in the order the test chooses.
///
/// [`BalHeadPipeline`]: degenbot_simulation::sim::evm2::bal_heads::BalHeadPipeline
#[derive(Clone, Default)]
pub struct SpawnQueue {
    parked: Arc<Mutex<Vec<BoxFuture<'static, ()>>>>,
}

impl SpawnQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// The [`HeadSpawn`] a pipeline is constructed with.
    pub fn spawner(&self) -> HeadSpawn {
        let parked = Arc::clone(&self.parked);
        Arc::new(move |phase: BoxFuture<'static, ()>| parked.lock().unwrap().push(phase))
    }

    /// Phases spawned and not yet run.
    pub fn len(&self) -> usize {
        self.parked.lock().unwrap().len()
    }

    /// Whether no phase is parked.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Take the oldest parked phase WITHOUT running it: the test decides when a
    /// head's preparation lands.
    pub fn take(&self) -> Option<BoxFuture<'static, ()>> {
        let mut parked = self.parked.lock().unwrap();
        if parked.is_empty() {
            None
        } else {
            Some(parked.remove(0))
        }
    }

    /// Run the oldest parked phase to completion.
    pub async fn run_one(&self) -> bool {
        match self.take() {
            Some(phase) => {
                phase.await;
                true
            }
            None => false,
        }
    }
}
