//! `ReadHub` — the anchor-scoped async read hub: storage and account reads
//! coalesced per anchor and settled by a round the caller drains.
//!
//! The blocking leaf ([`BlockingAsyncDb`](super::blocking_async_db) over
//! [`AlloyAsyncDb`](super::alloy_async_db)) reads one slot or one account per
//! call: every cold read is its own RPC round trip, and two fan-outs pinned to
//! the same block re-pay the same slot. The hub is the async batching leaf in
//! its place — reads register against ONE anchor, duplicate reads collapse onto
//! ONE fetch, distinct reads queued in a round share ONE provider batch per read
//! kind, and the caller (a scheduler with work to overlap), never a timer,
//! decides when a round is dispatched.
//!
//! # The anchor pins every value
//!
//! A hub is minted for one [`ReadAnchor`] and reads the provider's batch
//! primitives at that anchor's block number, with the anchor's block hash as the
//! identity fence: [`ReadHubCache::for_anchor`] hands every caller at an anchor the same
//! hub and mints a fresh one the moment the anchor moves — a same-height reorg
//! included, exactly as
//! [`StorageMemoCache`](crate::sim::storage_memo::StorageMemoCache) retires its
//! memo. A value memoized in one hub is therefore unreachable from any other
//! anchor's hub; there is no cross-anchor map that can go stale.
//!
//! # One fetch per read identity (single flight)
//!
//! A read identity is `(anchor, kind, key)`; the anchor is the hub's, so the
//! hub's maps carry the rest. Three paths answer a read, and only the third
//! costs a round trip:
//!
//! 1. the anchor memo already holds the value: answered on the spot;
//! 2. the identity is already queued or already in flight: the requester joins
//!    that entry's waiters, so one fetch answers every requester with the same
//!    result;
//! 3. otherwise the read joins the round queue.
//!
//! # A round is drained by the caller
//!
//! [`ReadHub::drain_round`] takes every queued identity, issues the queued
//! storage identities as one
//! [`AlloyProvider::get_storage_batch_results_at`] batch and the queued account
//! identities as one [`AlloyProvider::get_accounts_batch_at`] batch (both at the
//! anchor, issued concurrently), then hands each waiter its value. There is no
//! sleep, no debounce, and no background task: a read future resolves only
//! because a `drain_round` settled it (or because the memo already had it), so a
//! caller that awaits a read without draining parks by contract. The scheduler's
//! shape is therefore: poll the runnable fibers, drain one round when none can
//! progress, poll again.
//!
//! A drain takes the queue at its start, so a read registered while a round is
//! in flight joins that round's identity instead of waiting for another drain,
//! and a second concurrent drain finds nothing queued and returns an empty
//! [`RoundReport`].
//!
//! No lock guard crosses an RPC await. The hub's lock covers registration,
//! round-taking, and settlement only: the provider calls run after it is dropped
//! and the settlement re-takes it afterwards.
//!
//! # Failure shape
//!
//! Both batches attribute failures per member, so a refusal is never round-wide:
//! a storage slot the node refuses fails only that slot's waiters, and the
//! neighbours in the same packet are answered and memoized; an account address
//! the node refuses fails only that address's requester. A refused identity
//! never joins the anchor memo — its in-flight entry is cleared instead — so a
//! later read of it queues a fresh round: a node that could not serve one slot
//! last time may serve it next time.
//!
//! Only a round-wide failure reaches every requester: a transport failure, or a
//! batch that never dispatched, has no per-member answer to attribute, so the
//! whole round fails for the identities it carried. A round that fails, or is
//! abandoned (its `drain_round` future dropped mid-flight), likewise clears its
//! in-flight entries and hands each waiter an error, so the next read of that
//! identity queues a fresh round instead of waiting on a fetch that will never
//! answer.
//!
//! # The account memo's scope and its limits
//!
//! The account memo is per anchor, not cross-block: it holds the same
//! balance/nonce/code triple the provider read at the anchor's block, for as
//! long as that anchor lives. It therefore memoizes EOA rows (an all-empty read)
//! too — they are correct for the anchor and unreachable from the next one. This
//! is deliberately NOT the cross-block
//! [`WarmCodeCache`](super::warm_code_cache) policy, which refuses to cache EOA
//! rows because their nonce/balance mutate every block: an anchor's memo dies
//! with the anchor, so the EOA staleness that policy guards against cannot arise
//! here, and the warm cache is untouched by the hub.
//!
//! The limits follow from that scope. A hub's maps are unbounded within their
//! anchor: every distinct slot and account a fan-out touches is retained until
//! the anchor moves, which is the point (the second fan-out at the anchor is
//! free) and also the cost ceiling — memory and lock contention grow with the
//! anchor's distinct read set, not with the fan-out count. Values are the
//! provider's answers, so a hub memo never holds an engine write; the EVM's
//! per-candidate overlay owns those.
//!
//! # Migration from single reads
//!
//! A caller replacing a single read ([`AlloyAsyncDb::get_storage`](super::alloy_async_db),
//! [`AlloyAsyncDb::get_account`](super::alloy_async_db), or a bare
//! [`AlloyProvider::get_storage_at`] / [`AlloyProvider::get_account_at`]) moves in
//! three steps:
//!
//! 1. own a [`ReadHubCache`] for the session and take `for_anchor(anchor)` where
//!    the caller used to pin a block number — the anchor is the block the reads
//!    are pinned to, never a lagging clock;
//! 2. replace the read call with [`ReadHub::read_storage`] /
//!    [`ReadHub::read_account`], keeping the returned future instead of awaiting
//!    it inline;
//! 3. await the futures after a [`ReadHub::drain_round`] settled their round —
//!    the scheduler drains when no fiber can progress; a sequential caller
//!    registers its reads, drains once, then awaits.
//!
//! The value shapes cross unchanged: `read_storage` answers the provider's
//! `StorageValue` for the pinned index, and `read_account` answers the provider's
//! [`AccountRead`] triple, so the all-empty → non-existent rule stays where it
//! already lives (the DB adapter's `get_account`), not in the hub.
//!
//! The failure shape does change, and in the caller's favour: a single read
//! surfaces one failure as its own, while a hub read surfaces its member's
//! refusal as its own and leaves the round's other reads answered. A caller
//! that used the flat [`AlloyProvider::get_storage_batch_at`] (which collapses a
//! batch to its first failure) reads through the hub to keep the served slots.
//!
//! # Parent-linked carry-forward
//!
//! [`ReadHubCache::advance_bal`] is the opt-in state transition between
//! anchors: install the child anchor with a snapshot of the SETTLED parent
//! chain values plus EVERY final write of the child's exact-hash canonical
//! block access list. The link is exact — carried only when the installed
//! parent is exactly one block below the child and its hash is the child's
//! parent hash, taken from the child's actual header — and the list is fetched
//! and fully decoded BEFORE anything is cloned, so a missing, unsupported,
//! RPC-failed, or malformed list yields a cold child that never serves parent
//! values. Only settled storage words cross; accounts, queued or in-flight
//! reads, waiters, and stats stay behind, so parent and child are independent
//! afterwards.
//!
//! The write coverage is a TRUST boundary, not a fallback: a syntactically
//! valid list from the trusted node is assumed to record every write the block
//! made, and valid-but-incomplete write coverage is undetectable here — a
//! carried parent value for an unrecorded write would silently outlive the
//! block that changed it. Read omissions are safe: a slot no source mentions
//! stays on the ordinary dynamic path.
//!
//! Carry-forward V1 is write-only: no read-only identities are prefetched at
//! the child, unknown reads queue and settle through the ordinary batch path,
//! and the carry takes no plan and no caps — its one filter is the relevance
//! rule on new writes ([`ReadHubCache::advance_bal`]). Same-anchor calls are
//! idempotent and repeat no BAL work.
//!
//! # What the snapshot holds, and what it costs
//!
//! The snapshot grows from two sources only: values candidates actually read
//! (the working set, carried forward) and final writes to unknown slots whose
//! address the caller's relevance set covers. Writes to known keys always
//! update — relevance may add entries, never veto updates — and an irrelevant
//! unknown write is left unknown so a later read falls back dynamically, so
//! unrelated mainnet state does not accumulate. Growth is still UNBOUNDED over
//! a long run within those two sources, and the clone cost grows with it;
//! [`ReadHub::snapshot_breakdown`] splits working-set entries from BAL-only
//! retention so the growth stays visible. Retiring BAL-only entries (or
//! bounding the snapshot) is a deliberate later decision, not a silent one.
//!
//! # What this deliberately does not do
//!
//! No EVM `Database` implementation, no worker pool, no dispatch integration,
//! and no prefetch policy of its own: the hub is the read lane those later
//! pieces share. The opt-in BAL assist ([`seed_bal`](ReadHub::seed_bal) /
//! [`prefetch_bal`](ReadHub::prefetch_bal), types in
//! [`bal_prefetch`](super::bal_prefetch)) seeds one anchor's hub from an
//! EIP-7928 block access list — final write values into the memo, read-only
//! identities into this same queue — but it is a seeding seam with no dispatch
//! and no second RPC path behind it. The carry-forward
//! ([`advance_bal`](ReadHubCache::advance_bal)) keeps the same posture one
//! anchor over: a snapshot transition seam with no dispatch. Block hashes are
//! per-chain facts and are never hub reads.

use alloy::primitives::{Address, StorageValue, B256, U256};
use degenbot_core::errors::{ProviderError, ProviderResult};
use degenbot_rpc::provider::{AccountRead, AlloyProvider};
use futures_util::future::join;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

use super::bal_prefetch::{
    fetch_bal_seed, BalPlan, BalPrefetchReport, BalSeed, BalSeedReport, BalSkip,
};
use crate::sim::storage_memo::ReadAnchor;

/// Why a hub read has no value.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ReadHubError {
    /// The provider's read failed: the member's own failure for a refused
    /// storage slot or account address, or the round's failure for a transport
    /// failure (which has no per-member answer to attribute).
    #[error("RPC read failed: {0}")]
    Rpc(#[from] ProviderError),
    /// No answer will arrive: the round owning the read was cancelled before it
    /// settled, and its in-flight entry was cleared for a later retry.
    #[error("the read was abandoned before its round settled")]
    Abandoned,
}

/// Cumulative hub counters — the read lane's own profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadHubStats {
    /// Reads answered by the anchor memo (no queue, no fetch).
    pub memo_hits: u64,
    /// Reads that joined an identity another requester already queued or was
    /// already fetching.
    pub coalesced: u64,
    /// Rounds that settled at least one member (a round abandoned mid-flight
    /// dispatched without settling, so it is not counted).
    pub rounds: u64,
    /// Distinct storage slots dispatched, summed over rounds.
    pub storage_members: u64,
    /// Distinct accounts dispatched, summed over rounds.
    pub account_members: u64,
    /// Storage slots whose member read was refused, summed over rounds. Each
    /// failed only its own waiters (see the module's failure shape).
    pub storage_member_errors: u64,
    /// Account addresses whose member read was refused, summed over rounds.
    pub account_member_errors: u64,
}

/// What one drained round dispatched and settled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoundReport {
    /// Distinct storage slots the round's storage batch carried.
    pub storage_members: usize,
    /// Distinct accounts the round's account batch carried.
    pub account_members: usize,
    /// Storage slots whose member read was refused; each failed only its own
    /// waiters, and the round's other slots were answered.
    pub storage_member_errors: usize,
    /// Account addresses whose members failed; each failed only its requester.
    pub account_member_errors: usize,
}

impl RoundReport {
    /// Whether the round dispatched nothing (a drain that found no queued read,
    /// because another round already owns every pending identity).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.storage_members == 0 && self.account_members == 0
    }

    /// The account batch's RPC member count: `eth_getBalance`,
    /// `eth_getTransactionCount`, and `eth_getCode` per account.
    #[must_use]
    pub const fn account_rpc_members(&self) -> usize {
        self.account_members * 3
    }
}

/// One hub read: a handle that resolves when the round owning the read settles,
/// or immediately when the anchor memo already holds the answer.
///
/// The read registers when the handle is created ([`ReadHub::read_storage`] /
/// [`ReadHub::read_account`]), so a caller registers everything it needs and then
/// drains once.
#[must_use = "a read is settled by a `drain_round` call, or by the anchor memo"]
pub struct ReadFuture<T> {
    rx: oneshot::Receiver<Result<T, ReadHubError>>,
}

impl<T> ReadFuture<T> {
    const fn new(rx: oneshot::Receiver<Result<T, ReadHubError>>) -> Self {
        Self { rx }
    }
}

impl<T> Future for ReadFuture<T> {
    type Output = Result<T, ReadHubError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.get_mut().rx).poll(cx) {
            Poll::Ready(Ok(outcome)) => Poll::Ready(outcome),
            // The sender is gone without an answer: the round was abandoned
            // mid-flight, or the hub dropped before settling.
            Poll::Ready(Err(_gone)) => Poll::Ready(Err(ReadHubError::Abandoned)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A storage read identity: one word of one account at the hub's anchor.
type Slot = (Address, U256);

/// One read identity's requesters, and whether a round already owns its fetch.
struct Pending<T> {
    /// A drained round is fetching this identity (its waiters are served by that
    /// round, not by the next drain).
    dispatched: bool,
    /// Every requester's answer channel, in registration order.
    waiters: Vec<oneshot::Sender<Result<T, ReadHubError>>>,
}

impl<T> Pending<T> {
    fn queued(waiter: oneshot::Sender<Result<T, ReadHubError>>) -> Self {
        Self {
            dispatched: false,
            waiters: vec![waiter],
        }
    }
    /// A BAL read-only identity: queued for the next round's batch with no
    /// waiter of its own — requesters that join it before the drain coalesce
    /// onto it exactly as they would onto any queued identity.
    fn prefetched() -> Self {
        Self {
            dispatched: false,
            waiters: Vec::new(),
        }
    }
}

#[derive(Default)]
struct HubState {
    /// The anchor's fetched storage words.
    storage_values: HashMap<Slot, StorageValue>,
    /// Every slot a CANDIDATE read at this anchor or anywhere earlier in the
    /// carried chain — the snapshot's working set (see
    /// [`ReadHub::snapshot_breakdown`]).
    read_set: HashSet<Slot>,
    /// The anchor's fetched account rows (EOA rows included: see the module docs).
    account_values: HashMap<Address, AccountRead>,
    /// Storage identities queued or in flight at this anchor.
    storage_pending: HashMap<Slot, Pending<StorageValue>>,
    /// Account identities queued or in flight at this anchor.
    account_pending: HashMap<Address, Pending<AccountRead>>,
    stats: ReadHubStats,
}

/// The anchor-scoped async read hub (see the module docs).
///
/// The hub OWNS its provider handle (`AlloyProvider` clones the one shared
/// transport), never borrowing it, so a session-scoped owner can hold a hub —
/// and a hub-holding cache — for the engine's life instead of pinning itself to
/// one call's provider borrow.
pub struct ReadHub {
    provider: AlloyProvider,
    anchor: ReadAnchor,
    state: Mutex<HubState>,
}

impl ReadHub {
    /// A hub for `anchor`: every read it makes is pinned to that block.
    #[must_use]
    pub fn new(provider: &AlloyProvider, anchor: ReadAnchor) -> Self {
        Self {
            provider: provider.clone(),
            anchor,
            state: Mutex::new(HubState::default()),
        }
    }

    /// The anchor every value this hub serves was read at.
    #[must_use]
    pub const fn anchor(&self) -> ReadAnchor {
        self.anchor
    }

    /// The hub's cumulative counters.
    #[must_use]
    pub fn stats(&self) -> ReadHubStats {
        self.state.lock().stats
    }

    /// How many storage words the anchor memo holds — the memoization seam's
    /// observable state.
    #[must_use]
    pub fn memoized_slots(&self) -> usize {
        self.state.lock().storage_values.len()
    }

    /// The snapshot's retention split: how much of it candidates have actually
    /// read, versus entries seeded only because a BAL final write's address was
    /// in the caller's relevance set. Both grow without bound over a long run
    /// (see the module docs), so `bal_only` is the growth a run pays for slots
    /// nobody reads — visible here.
    #[must_use]
    pub fn snapshot_breakdown(&self) -> SnapshotRetention {
        let state = self.state.lock();
        let working_set = state
            .read_set
            .iter()
            .filter(|key| state.storage_values.contains_key(key))
            .count();
        SnapshotRetention {
            working_set,
            bal_only: state.storage_values.len().saturating_sub(working_set),
        }
    }

    /// Register one storage read at the hub's anchor.
    ///
    /// Resolves from the anchor memo when the word is known, joins the round that
    /// already owns the identity when one is queued or in flight, and otherwise
    /// waits for the next [`Self::drain_round`].
    pub fn read_storage(&self, address: Address, index: U256) -> ReadFuture<StorageValue> {
        let (tx, rx) = oneshot::channel();
        let mut state = self.state.lock();
        state.read_set.insert((address, index));
        if let Some(value) = state.storage_values.get(&(address, index)).copied() {
            state.stats.memo_hits += 1;
            drop(state);
            let _ = tx.send(Ok(value));
            return ReadFuture::new(rx);
        }
        let coalesced = if let Some(pending) = state.storage_pending.get_mut(&(address, index)) {
            pending.waiters.push(tx);
            true
        } else {
            state
                .storage_pending
                .insert((address, index), Pending::queued(tx));
            false
        };
        if coalesced {
            state.stats.coalesced += 1;
        }
        ReadFuture::new(rx)
    }

    /// Register one account read at the hub's anchor — balance, nonce, and code
    /// in one batch member group (see [`AlloyProvider::get_accounts_batch_at`]).
    pub fn read_account(&self, address: Address) -> ReadFuture<AccountRead> {
        let (tx, rx) = oneshot::channel();
        let mut state = self.state.lock();
        if let Some(read) = state.account_values.get(&address).cloned() {
            state.stats.memo_hits += 1;
            drop(state);
            let _ = tx.send(Ok(read));
            return ReadFuture::new(rx);
        }
        let coalesced = if let Some(pending) = state.account_pending.get_mut(&address) {
            pending.waiters.push(tx);
            true
        } else {
            state.account_pending.insert(address, Pending::queued(tx));
            false
        };
        if coalesced {
            state.stats.coalesced += 1;
        }
        ReadFuture::new(rx)
    }

    /// Dispatch one round: every identity queued by the time this call takes the
    /// queue becomes one provider batch per kind, both issued at the anchor and
    /// issued concurrently, and every waiter is handed its answer before this
    /// returns.
    ///
    /// Returns an empty [`RoundReport`] when nothing was queued (every pending
    /// identity is already owned by another in-flight round).
    pub async fn drain_round(&self) -> RoundReport {
        let (storage_keys, account_keys) = self.take_queued();
        if storage_keys.is_empty() && account_keys.is_empty() {
            return RoundReport::default();
        }
        // The guard abandons this round's in-flight entries if the future is
        // dropped before it settles, so a cancelled drain never strands a read.
        let round = RoundGuard {
            hub: self,
            storage: &storage_keys,
            accounts: &account_keys,
            settled: false,
        };
        let (storage, accounts) = join(
            self.fetch_storage(&storage_keys),
            self.fetch_accounts(&account_keys),
        )
        .await;
        round.settle(storage, accounts)
    }

    /// Seed one decoded BAL's assistance into this anchor's hub: every final
    /// write value joins the anchor memo — settling any queued waiter with the
    /// very value its round would have fetched — and every read-only identity
    /// joins the queue the next [`Self::drain_round`] batches. No storage RPC
    /// lives here; omitted and unread slots keep the ordinary dynamic path.
    ///
    /// Race contract: an identity an in-flight round already owns is left
    /// exactly as it is (that round settles it with the same anchor value), so
    /// no waiter is stranded and no pending entry is lost. Everything else is
    /// seeded under this hub's lock, and every value is this anchor's alone.
    pub fn seed_bal(&self, seed: &BalSeed) -> BalSeedReport {
        let mut report = BalSeedReport::default();
        let mut state = self.state.lock();
        for &(address, slot, value) in &seed.writes {
            let key = (address, slot);
            if state.storage_values.contains_key(&key) {
                report.writes_already_known += 1;
                continue;
            }
            if let Some(pending) = state.storage_pending.remove(&key) {
                if pending.dispatched {
                    // The in-flight round owns this identity and will settle it
                    // with the same anchor value; touching it here could only
                    // duplicate or displace that settlement.
                    report.writes_ignored_in_flight += 1;
                    state.storage_pending.insert(key, pending);
                    continue;
                }
                state.storage_values.insert(key, value);
                report.writes_seeded += 1;
                // The lock is held across the sends only (see `RoundGuard::settle`).
                deliver(pending.waiters, &Ok(value));
                continue;
            }
            state.storage_values.insert(key, value);
            report.writes_seeded += 1;
        }
        for &(address, slot) in &seed.reads {
            let key = (address, slot);
            if state.storage_values.contains_key(&key) || state.storage_pending.contains_key(&key) {
                report.reads_skipped_known += 1;
                continue;
            }
            state.storage_pending.insert(key, Pending::prefetched());
            report.reads_queued += 1;
        }
        report
    }

    /// The opt-in BAL-accelerated prefetch: fetch this anchor's EXACT-hash block
    /// access list, decode it wholesale, and seed what the plan admits (see
    /// [`Self::seed_bal`]). The list arrives in whichever official
    /// representation the node serves — raw RLP preferred, the official JSON as
    /// the automatic eth-only fallback — through the one acquisition path
    /// [`fetch_bal_seed`] shares with the canonical-head prepare.
    ///
    /// Best-effort by contract — candidate execution is never failed and later
    /// reads are never poisoned. A number-only anchor (no hash to request by),
    /// a refused or failed request, or malformed bytes is reported as a
    /// [`BalSkip`] with nothing seeded, and every read falls back to the
    /// ordinary batch/drain path with unchanged semantics. Subsequent dynamic
    /// misses — the slots assistance did not cover — are visible afterwards as
    /// fresh rounds in [`Self::stats`].
    pub async fn prefetch_bal(&self, plan: &BalPlan) -> BalPrefetchReport {
        let Some(hash) = self.anchor.hash else {
            return BalPrefetchReport {
                skip: Some(BalSkip::NoAnchorHash),
                ..BalPrefetchReport::default()
            };
        };
        let seed = match fetch_bal_seed(&self.provider, hash, plan).await {
            Ok(seed) => seed,
            Err(skip) => {
                return BalPrefetchReport {
                    skip: Some(skip),
                    ..BalPrefetchReport::default()
                };
            }
        };
        BalPrefetchReport {
            skip: None,
            seeded: self.seed_bal(&seed),
            filtered_entries: seed.filtered_entries,
            capped_entries: seed.capped_entries,
        }
    }

    /// Clone every SETTLED storage word of `parent` into this hub's memo — the
    /// carry-forward's whole transfer. Only values a `drain_round` settled at
    /// the parent cross: accounts, queued or in-flight identities, waiters, and
    /// stats stay behind, and the two hubs are independent afterwards.
    fn carry_settled_storage(&self, parent: &ReadHub) -> usize {
        let (settled, read_set) = {
            let state = parent.state.lock();
            (state.storage_values.clone(), state.read_set.clone())
        };
        let carried = settled.len();
        let mut state = self.state.lock();
        state.storage_values.extend(settled);
        state.read_set.extend(read_set);
        carried
    }

    /// Apply the child BAL's final writes to this hub's memo under the carry's
    /// write policy. A write to a slot the snapshot already holds OVERWRITES it
    /// whatever `relevance` says — a stale carried value outliving the block
    /// that changed it is the one outcome the carry must never produce, and a
    /// zero or deletion final overwrites like any other. A write to an UNKNOWN
    /// slot is seeded only when its address is in `relevance`; an irrelevant
    /// unknown write is left unknown (counted, never hidden) so a later read
    /// takes the ordinary dynamic path and unrelated mainnet state never
    /// accumulates in the snapshot.
    ///
    /// The snapshot is built before the hub is installed, so there is never a
    /// waiter to settle here (unlike [`Self::seed_bal`], there is no
    /// already-known skip and no in-flight carve-out).
    fn apply_final_writes(
        &self,
        writes: &[(Address, U256, U256)],
        relevance: &[Address],
    ) -> AppliedWrites {
        let mut state = self.state.lock();
        let mut applied = Vec::new();
        let mut overwritten = 0;
        let mut skipped = 0;
        for &(address, slot, value) in writes {
            let key = (address, slot);
            let known = state.storage_values.contains_key(&key);
            if !known && !relevance.contains(&address) {
                skipped += 1;
                continue;
            }
            state.storage_values.insert(key, value);
            if known {
                overwritten += 1;
            }
            applied.push(key);
        }
        AppliedWrites {
            applied,
            overwritten,
            skipped,
        }
    }

    /// Mark every queued identity as dispatched and return it, so the round takes
    /// exactly the queue that existed when it started.
    fn take_queued(&self) -> (Vec<Slot>, Vec<Address>) {
        let mut state = self.state.lock();
        let mut storage = Vec::new();
        for (key, pending) in &mut state.storage_pending {
            if !pending.dispatched {
                pending.dispatched = true;
                storage.push(*key);
            }
        }
        let mut accounts = Vec::new();
        for (key, pending) in &mut state.account_pending {
            if !pending.dispatched {
                pending.dispatched = true;
                accounts.push(*key);
            }
        }
        (storage, accounts)
    }

    /// One storage batch at the anchor, one member outcome per slot (empty queue
    /// dispatches nothing).
    async fn fetch_storage(
        &self,
        keys: &[Slot],
    ) -> ProviderResult<Vec<ProviderResult<StorageValue>>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        self.provider
            .get_storage_batch_results_at(keys, self.anchor.number)
            .await
    }

    /// One multi-account batch at the anchor, one member outcome per address.
    async fn fetch_accounts(
        &self,
        addresses: &[Address],
    ) -> ProviderResult<Vec<ProviderResult<AccountRead>>> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        self.provider
            .get_accounts_batch_at(addresses, self.anchor.number)
            .await
    }

    /// Clear this round's in-flight entries and fail every waiter — the
    /// cancellation path: the identities must be readable again.
    fn abandon(&self, storage: &[Slot], accounts: &[Address]) {
        let mut state = self.state.lock();
        for key in storage {
            if let Some(pending) = state.storage_pending.remove(key) {
                deliver(pending.waiters, &Err(ReadHubError::Abandoned));
            }
        }
        for address in accounts {
            if let Some(pending) = state.account_pending.remove(address) {
                deliver(pending.waiters, &Err(ReadHubError::Abandoned));
            }
        }
    }
}

/// The in-flight ownership of one drained round, and the settlement that ends it.
struct RoundGuard<'a> {
    hub: &'a ReadHub,
    storage: &'a [Slot],
    accounts: &'a [Address],
    settled: bool,
}

impl Drop for RoundGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.hub.abandon(self.storage, self.accounts);
        }
    }
}

impl RoundGuard<'_> {
    /// Hand each identity its outcome: memoize the values and answer every
    /// waiter, or fail the identities the round could not answer.
    fn settle(
        mut self,
        storage: ProviderResult<Vec<ProviderResult<StorageValue>>>,
        accounts: ProviderResult<Vec<ProviderResult<AccountRead>>>,
    ) -> RoundReport {
        self.settled = true;
        let mut report = RoundReport {
            storage_members: self.storage.len(),
            account_members: self.accounts.len(),
            storage_member_errors: 0,
            account_member_errors: 0,
        };
        // The lock is held across the sends only: waking a waiter schedules it,
        // it never runs hub code inline, and no await crosses this scope.
        let mut state = self.hub.state.lock();
        match storage {
            Ok(values) => {
                for (key, value) in self.storage.iter().zip(values) {
                    let Some(pending) = state.storage_pending.remove(key) else {
                        continue;
                    };
                    // One slot's member settles here: a value joins the anchor
                    // memo and answers its waiters, a refusal fails only them
                    // and leaves the identity readable for a later round.
                    match value {
                        Ok(value) => {
                            state.storage_values.insert(*key, value);
                            deliver(pending.waiters, &Ok(value));
                        }
                        Err(error) => {
                            report.storage_member_errors += 1;
                            deliver(pending.waiters, &Err(ReadHubError::Rpc(error)));
                        }
                    }
                }
            }
            Err(error) => {
                for key in self.storage {
                    if let Some(pending) = state.storage_pending.remove(key) {
                        deliver(pending.waiters, &Err(ReadHubError::Rpc(error.clone())));
                    }
                }
            }
        }
        match accounts {
            Ok(reads) => {
                for (address, read) in self.accounts.iter().zip(reads) {
                    let Some(pending) = state.account_pending.remove(address) else {
                        continue;
                    };
                    match read {
                        Ok(read) => {
                            state.account_values.insert(*address, read.clone());
                            deliver(pending.waiters, &Ok(read));
                        }
                        Err(error) => {
                            report.account_member_errors += 1;
                            deliver(pending.waiters, &Err(ReadHubError::Rpc(error)));
                        }
                    }
                }
            }
            Err(error) => {
                for address in self.accounts {
                    if let Some(pending) = state.account_pending.remove(address) {
                        deliver(pending.waiters, &Err(ReadHubError::Rpc(error.clone())));
                    }
                }
            }
        }
        state.stats.rounds += 1;
        state.stats.storage_members += count_u64(report.storage_members);
        state.stats.account_members += count_u64(report.account_members);
        state.stats.storage_member_errors += count_u64(report.storage_member_errors);
        state.stats.account_member_errors += count_u64(report.account_member_errors);
        report
    }
}

/// Hand one outcome to every requester of an identity. A dropped receiver is a
/// requester that went away with its read future; the outcome is still the
/// round's, so a failed send is not an error.
fn deliver<T: Clone>(
    waiters: Vec<oneshot::Sender<Result<T, ReadHubError>>>,
    outcome: &Result<T, ReadHubError>,
) {
    for waiter in waiters {
        let _ = waiter.send(outcome.clone());
    }
}

/// A member count as a saturating counter increment.
fn count_u64(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

/// Why a carry-forward refused to link the child to the installed parent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CarryRefusal {
    /// Nothing is installed, so there is no settled chain to carry.
    #[error("no parent anchor is installed, so there is no settled chain to carry")]
    NoParent,
    /// The installed parent is not exactly one block below the child: a gap, a
    /// backwards step, or a child at the parent's own height.
    #[error("the installed parent anchor is not exactly one block below the child")]
    NotConsecutive,
    /// The installed parent is number-only: its hash cannot be compared against
    /// the child's parent hash, so the link is unverifiable, never assumed.
    #[error(
        "the installed parent anchor is number-only, so the child's parent hash cannot be verified against it"
    )]
    ParentHashUnknown,
    /// The installed parent's hash is not the child's parent hash — a wrong
    /// hash, or a same-height reorg whose abandoned block the child still names.
    #[error("the installed parent anchor's hash is not the child's parent hash")]
    ParentHashMismatch,
}

impl CarryRefusal {
    /// The closed-vocabulary label this refusal reports.
    ///
    /// One label per VARIANT, so a diagnostic row can name the cause a caller's
    /// collapsed `lineage-refused` label deliberately hides: the label is the
    /// dispatch contract (a closed set a scrape and a metric share), while this
    /// vocabulary is the explanation of why the chain did not carry.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::NoParent => "no-parent",
            Self::NotConsecutive => "not-consecutive",
            Self::ParentHashUnknown => "parent-hash-unknown",
            Self::ParentHashMismatch => "parent-hash-mismatch",
        }
    }
}

/// What one parent-linked carry-forward advance did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarryOutcome {
    /// The child anchor was already installed: the same hub handed back, and no
    /// BAL work repeated.
    AlreadyCurrent,
    /// The settled parent chain was cloned into the child snapshot and the
    /// child's final writes applied over it.
    Carried {
        /// Settled parent storage words cloned into the child snapshot.
        carried_values: usize,
    },
    /// The child snapshot holds no parent values: the lineage refused, the BAL
    /// stage stepped aside, or both. The child's final writes may still be
    /// applied under the same relevance rule (they are the child's own
    /// post-block words); every other read follows the ordinary dynamic path.
    Cold {
        /// Why the chain was not carried; `None` when the lineage matched and
        /// the BAL stage is what stepped aside.
        lineage: Option<CarryRefusal>,
        /// Why the child's final writes were not read; `None` when the child's
        /// BAL decoded.
        bal: Option<BalSkip>,
    },
}

/// One child BAL's fetch and full decode — the prepare phase of a parent-linked
/// advance, runnable before the head that will use it is applied.
///
/// Nothing here moves snapshot state: the list is fetched at `anchor`'s exact
/// block hash and decoded wholesale, so a prepared list that completes after its
/// head was superseded is simply dropped. A list that cannot be read is carried
/// as its [`BalSkip`], which is what makes the resulting child cold — the skip is
/// prepared, never discovered at the apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedBal {
    anchor: ReadAnchor,
    final_writes: Vec<(Address, U256, U256)>,
    skip: Option<BalSkip>,
    latency: Duration,
}

impl PreparedBal {
    /// Fetch and FULLY decode the block access list at `anchor`'s exact block
    /// hash — the fetch/decode stage of
    /// [`ReadHubCache::advance_bal`] / [`DeltaSnapshotCache::advance_bal`](super::delta_snapshot::DeltaSnapshotCache::advance_bal),
    /// with no snapshot moved.
    ///
    /// A number-only anchor ([`BalSkip::NoAnchorHash`]), a refused or failed
    /// request ([`BalSkip::Rpc`]), and malformed bytes ([`BalSkip::Decode`]) are
    /// all prepared as a skip with no writes: the caller that later applies them
    /// gets exactly the cold child a synchronous advance would have produced.
    ///
    /// Carry-forward is write-only: read-only identities are not prefetched, so
    /// unknown reads take the ordinary dynamic path.
    pub async fn prepare(provider: &AlloyProvider, anchor: ReadAnchor) -> Self {
        let started = Instant::now();
        let (final_writes, skip) = match anchor.hash {
            None => (Vec::new(), Some(BalSkip::NoAnchorHash)),
            Some(hash) => match fetch_bal_seed(provider, hash, &BalPlan::default()).await {
                Ok(seed) => (seed.writes, None),
                Err(skip) => (Vec::new(), Some(skip)),
            },
        };
        Self {
            anchor,
            final_writes,
            skip,
            latency: started.elapsed(),
        }
    }

    /// The exact anchor this list was fetched at: its number, and the hash the
    /// request was made by.
    #[must_use]
    pub const fn anchor(&self) -> ReadAnchor {
        self.anchor
    }

    /// The block hash the list was fetched by, when the anchor carried one.
    #[must_use]
    pub const fn hash(&self) -> Option<B256> {
        self.anchor.hash
    }

    /// Why the list could not be read; `None` when it decoded. A prepared skip
    /// is applied as a cold child, never as a missing one.
    #[must_use]
    pub const fn skip(&self) -> Option<&BalSkip> {
        self.skip.as_ref()
    }

    /// Whether the list decoded: the final writes are this anchor's own
    /// post-block words.
    #[must_use]
    pub const fn is_decoded(&self) -> bool {
        self.skip.is_none()
    }

    /// Wall time of the fetch and the full decode — the cost the prepared phase
    /// took off the applier's critical path.
    #[must_use]
    pub const fn latency(&self) -> Duration {
        self.latency
    }

    /// The decoded final writes `(address, slot, value)`, in list order.
    #[must_use]
    pub fn final_writes(&self) -> &[(Address, U256, U256)] {
        &self.final_writes
    }
}

/// A snapshot's retention split — what candidates read, versus what only the
/// BAL wrote (see [`ReadHub::snapshot_breakdown`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotRetention {
    /// Snapshot entries whose slot a candidate read at this anchor or anywhere
    /// earlier in the carried chain.
    pub working_set: usize,
    /// Snapshot entries no candidate has read — seeded because their address
    /// was relevant when the write landed, never because an irrelevant write
    /// was kept.
    pub bal_only: usize,
}

/// What one application of a child's final writes did.
struct AppliedWrites {
    /// The identities applied, in BAL order.
    applied: Vec<(Address, U256)>,
    /// How many applied writes replaced a value the snapshot already held.
    overwritten: usize,
    /// Unknown-slot writes left unknown: address outside the relevance set.
    skipped: usize,
}

/// One parent-linked carry-forward advance: its outcome, what its writes did,
/// and where the time went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarryReport {
    /// The transition that happened.
    pub outcome: CarryOutcome,
    /// The identities of the child's final writes APPLIED to the snapshot
    /// `(address, slot)`, in BAL order: a write to a slot the snapshot held
    /// (overwritten whatever the relevance set says) or to an unknown slot
    /// whose address is in the relevance set. Empty when the BAL stage stepped
    /// aside or the call was idempotent.
    pub writes: Vec<(Address, U256)>,
    /// How many of `writes` replaced a value the snapshot already held.
    pub writes_overwritten: usize,
    /// Final writes left UNKNOWN: slots the snapshot did not hold whose address
    /// is outside the relevance set. A later read takes the dynamic path.
    pub writes_skipped: usize,
    /// Wall time of the child BAL's fetch and full decode.
    pub bal_latency: Duration,
    /// Wall time of the settled-parent clone.
    pub clone_time: Duration,
}

/// Classify the child→installed-parent link: carried only when the parent is
/// exactly one block below the child and its hash is the child's parent hash.
/// A number-only parent's hash is unknown, so the link is unverifiable rather
/// than matched.
pub(crate) fn classify_link(
    parent: Option<&ReadAnchor>,
    child: ReadAnchor,
    child_parent_hash: B256,
) -> Option<CarryRefusal> {
    let Some(parent) = parent else {
        return Some(CarryRefusal::NoParent);
    };
    if parent.number.checked_add(1) != Some(child.number) {
        return Some(CarryRefusal::NotConsecutive);
    }
    match parent.hash {
        None => Some(CarryRefusal::ParentHashUnknown),
        Some(hash) if hash != child_parent_hash => Some(CarryRefusal::ParentHashMismatch),
        Some(_) => None,
    }
}

/// The session-scoped owner of the per-anchor [`ReadHub`] — the migration entry
/// point for a caller that used to pin a block number per read.
pub struct ReadHubCache {
    provider: AlloyProvider,
    current: Mutex<Option<(ReadAnchor, Arc<ReadHub>)>>,
}

impl ReadHubCache {
    /// An empty owner: no anchor installed yet.
    #[must_use]
    pub fn new(provider: &AlloyProvider) -> Self {
        Self {
            provider: provider.clone(),
            current: Mutex::new(None),
        }
    }

    /// The hub for `anchor`: the installed one when the whole anchor (number AND
    /// hash) is unchanged, otherwise a fresh hub that becomes the installed one.
    ///
    /// A replacement drops the retired hub's every value with it — the same
    /// invalidation contract as
    /// [`StorageMemoCache::for_anchor`](crate::sim::storage_memo::StorageMemoCache::for_anchor),
    /// and for the same reason: a same-height reorg leaves the number alone and
    /// changes only the hash.
    pub fn for_anchor(&self, anchor: ReadAnchor) -> Arc<ReadHub> {
        let mut current = self.current.lock();
        if let Some((installed, hub)) = current.as_ref() {
            if *installed == anchor {
                return Arc::clone(hub);
            }
        }
        let hub = Arc::new(ReadHub::new(&self.provider, anchor));
        *current = Some((anchor, Arc::clone(&hub)));
        hub
    }

    /// The opt-in parent-linked carry-forward: install `child` as the current
    /// anchor with a snapshot of the settled parent chain values plus EVERY
    /// final write of the child's exact-hash canonical block access list.
    ///
    /// The link is exact and supplied by the caller: `child` is the child
    /// block's own anchor (number AND hash) and `child_parent_hash` is the
    /// parent hash from that block's ACTUAL header. The chain is carried only
    /// when the installed anchor is exactly one block below the child AND its
    /// hash equals `child_parent_hash`. Without a match — nothing installed, a
    /// gap, a number-only parent, a same-height reorg naming the abandoned
    /// block — the child starts cold rather than guessing lineage.
    ///
    /// The child BAL is fetched at the child's exact hash and FULLY decoded
    /// before anything is cloned: a missing, unsupported, RPC-failed, or
    /// malformed list returns a cold child with the skip reported, and parent
    /// values are never served at a child whose list could not be read. What is
    /// cloned is only the parent's SETTLED storage words. Carry-forward V1 is
    /// write-only — no read-only prefetch, no plan, no caps — and unknown child
    /// reads queue and settle through the ordinary batch path.
    ///
    /// # The write policy: update known keys, seed only relevant new ones
    ///
    /// `relevance` is the caller's current-candidate (or tracked MEV) address
    /// set. Every final write to a slot the snapshot already holds — carried
    /// parent value included, zero or deletion final included — OVERWRITES it
    /// whatever `relevance` says: a stale carried value outliving the block
    /// that changed it is the one outcome the carry must never produce, so
    /// relevance may add entries but never veto updates. A final write to an
    /// UNKNOWN slot is seeded only when its address is in `relevance`; anything
    /// else is left unknown — counted in [`CarryReport::writes_skipped`], never
    /// hidden — so a later read takes the ordinary dynamic path and unrelated
    /// mainnet state never accumulates. Cold children follow the same rule:
    /// with no carried values, only relevant new writes seed.
    ///
    /// Same-anchor calls are idempotent: the installed hub is handed back and
    /// no BAL work is repeated. Two CONCURRENT advances to the same child may
    /// duplicate the BAL fetch — there is no single-flight here.
    ///
    /// # The write-coverage trust boundary
    ///
    /// A syntactically valid BAL from the trusted node is assumed COMPLETE for
    /// writes. A write the list omits is undetectable here — the carried parent
    /// value for that slot would silently outlive the block that changed it —
    /// so this is a TRUST boundary, not a fallback path. Read omissions are
    /// safe: a slot no source mentions stays dynamic and misses nothing.
    pub async fn advance_bal(
        &self,
        child: ReadAnchor,
        child_parent_hash: B256,
        relevance: &[Address],
    ) -> (Arc<ReadHub>, CarryReport) {
        // Idempotence precedes any RPC: the same child anchor repeats no BAL
        // work, and no lock guard crosses the fetch.
        {
            let current = self.current.lock();
            if let Some((installed, hub)) = current.as_ref() {
                if *installed == child {
                    return (
                        Arc::clone(hub),
                        CarryReport {
                            outcome: CarryOutcome::AlreadyCurrent,
                            writes: Vec::new(),
                            writes_overwritten: 0,
                            writes_skipped: 0,
                            bal_latency: Duration::ZERO,
                            clone_time: Duration::ZERO,
                        },
                    );
                }
            }
        }

        // The child BAL is fetched and FULLY decoded before anything is
        // cloned: a skip here can never return a child serving parent values.
        let prepared = PreparedBal::prepare(&self.provider, child).await;
        let final_writes = prepared.final_writes();
        let bal = prepared.skip().cloned();
        let bal_latency = prepared.latency();

        // Link, snapshot, and install under one lock hold, so the lineage check
        // and the anchor move are one transition. The clone reads the SAME
        // installed parent the check validated: a reorg or a concurrent
        // transition while the BAL is in flight re-classifies against what is
        // actually installed, and the reported lineage can never disagree with
        // the hub the values were cloned from.
        let mut current = self.current.lock();
        if let Some((installed, hub)) = current.as_ref() {
            if *installed == child {
                return (
                    Arc::clone(hub),
                    CarryReport {
                        outcome: CarryOutcome::AlreadyCurrent,
                        writes: Vec::new(),
                        writes_overwritten: 0,
                        writes_skipped: 0,
                        bal_latency,
                        clone_time: Duration::ZERO,
                    },
                );
            }
        }
        let lineage = classify_link(
            current.as_ref().map(|(anchor, _)| anchor),
            child,
            child_parent_hash,
        );
        let linked = lineage.is_none() && bal.is_none();
        let hub = ReadHub::new(&self.provider, child);
        let mut carried_values = 0;
        let mut clone_time = Duration::ZERO;
        if linked {
            if let Some((_, parent_hub)) = current.as_ref() {
                let clone_started = Instant::now();
                carried_values = hub.carry_settled_storage(parent_hub);
                clone_time = clone_started.elapsed();
            }
        }
        let applied = hub.apply_final_writes(final_writes, relevance);
        let outcome = if linked {
            CarryOutcome::Carried { carried_values }
        } else {
            CarryOutcome::Cold { lineage, bal }
        };
        let hub = Arc::new(hub);
        *current = Some((child, Arc::clone(&hub)));
        (
            hub,
            CarryReport {
                outcome,
                writes: applied.applied,
                writes_overwritten: applied.overwritten,
                writes_skipped: applied.skipped,
                bal_latency,
                clone_time,
            },
        )
    }

    /// The installed anchor (`None` before the first `for_anchor`).
    #[must_use]
    pub fn anchor(&self) -> Option<ReadAnchor> {
        self.current.lock().as_ref().map(|(anchor, _)| *anchor)
    }
}
