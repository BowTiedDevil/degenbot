#![expect(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a benchmark probe prints its report and fails loudly on a broken fixture"
)]

//! The session-scheduler captured-data benchmark: the recorded production
//! fan-out read walks driven through the committed `ReadHub` under the
//! committed W/F bound, walled against the serial and per-worker reads.
//!
//! # What is driven, and what the trace cannot prove
//!
//! `/tmp/fanout_trace.log` (444 rows, 14 anchors) is `logs/bot_run.log`'s
//! `fanout-trace` stream: per candidate, the EIP-2930 access list the
//! simulation seam drained (`reads` = `address|slot,slot,...`) plus the wall the
//! seam measured for that candidate (`sim_ns`, encode + sequence + assess). It
//! carries **no calldata and no chain snapshot**, so it cannot reconstruct an
//! EVM call: no opcode runs, and no recorded read is *caused* by a recorded
//! frame. This probe therefore drives the read SCHEDULE — the hub's coalescing
//! and batching under a bounded slot budget — and charges the recorded
//! execution time as wall. Actual EVM call/slot parity on this same scheduler is
//! covered by the committed `tests/async_fanout.rs` / `tests/session_async.rs`
//! suite, which owns real frames; nothing here claims to reproduce a production
//! verdict. `AsyncEvmFanout` itself cannot be driven honestly from these fields
//! (its `PreparedCandidate` carries `Evm2SimStep`s that need calldata), so this
//! probe drives the read-hub scheduling seam one layer below it: the same
//! `ReadHub` rounds, the same `FanoutLimits` bound, the same drain discipline.
//!
//! # The modeled node
//!
//! One stub JSON-RPC transport answers every recorded key with a value derived
//! from that key alone (so a value is identical in every leg: the digest gate
//! catches a dropped, duplicated, or reordered read, never a value drift). The
//! transport is reached through the real `AlloyProvider` batch primitives the
//! hub dispatches, so the batches, member sets, and single-flight joins counted
//! here are the production adapter's own.
//!
//! The latency model is one round trip per dispatched packet:
//!
//! - a storage batch costs `storage_us`, a multi-account batch costs
//!   `account_us`;
//! - `ReadHub::drain_round` issues its storage and account batches
//!   CONCURRENTLY, so a round costs the max of the kinds it carried;
//! - a serial single read costs its own kind's latency (one packet each).
//!
//! The model is charged by the drivers, not by the transport, so every leg pays
//! identical prices for identical work. The round is charged once its drain
//! settled (the waiters are woken but not polled during the charge), which is
//! the same wall a round trip in flight would cost.
//!
//! # Exec as wall
//!
//! The recorded `sim_ns` is SPENT, not merely summed: every leg sleeps it inside
//! the candidate's own slot. That is what makes the walls comparable — a leg
//! that overlaps candidates overlaps their execution too — and it is the same
//! rule the earlier `worker_fanout_probe` replay used. Execution is CPU work on
//! the lane thread (the committed prototype runs each slot's EVM in place), so
//! exec overlap is bounded by the lane count W, while F bounds how many
//! candidates hold a slot (reads in flight plus lane queueing).
//!
//! # Legs
//!
//! - `serial` — today's shape: one read in flight at a time, accounts always
//!   forwarded, storage served from the leg's anchor memo (`StorageMemo`'s
//!   contract; accounts have no per-anchor memo on the old path).
//! - `hybrid` — the earlier probe's hybrid re-measured here: W OS threads, each
//!   with its own hub, its own rounds, and its own slot set (every candidate of
//!   the lane in flight).
//! - `fibers` — one lane, one shared hub, every candidate in flight (no slot
//!   bound): the committed `fibers` leg's shape.
//! - `session` — W lanes over ONE shared hub and F total slots
//!   (`FanoutLimits::lane_capacity`), one drain role for the whole session, a
//!   lane draining only when nothing it owns can progress.
//!
//! # Gate
//!
//! Every leg's per-candidate read digest (`keccak256` of the account triples and
//! storage words in recorded order) must equal the serial leg's.
//! `SESSION_FANOUT_PLANT_DIVERGENCE=1` makes the modeled node answer storage one
//! wei high during the `session` leg only, which must turn the gate RED and the
//! process exit nonzero (the negative-probe idiom).
//!
//! # Usage
//!
//! ```sh
//! just session-fanout-probe                  # the full report
//! SESSION_FANOUT_TRACE=/tmp/fanout_trace.log \
//! SESSION_FANOUT_STORAGE_US=700 SESSION_FANOUT_ACCOUNT_US=1200 \
//! SESSION_FANOUT_W=2 SESSION_FANOUT_F=8 just session-fanout-probe
//! SESSION_FANOUT_PLANT_DIVERGENCE=1 just session-fanout-probe   # exits 1
//! ```

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use serde_json::value::to_raw_value;
use serde_json::{json, Value};

use degenbot_rpc::provider::{AccountRead, AlloyProvider};
use degenbot_simulation::sim::evm2::async_fanout::{FanoutLimits, MAX_IDLE_ROUNDS};
use degenbot_simulation::sim::evm2::read_hub::{ReadHub, ReadHubStats};
use degenbot_simulation::sim::ReadAnchor;

// ── Configuration ─────────────────────────────────────────────────────────

/// The tag the bot's tracing formatter prefixes each recorded row with.
const TRACE_TAG: &str = "fanout-trace ";

/// The recorded corpus this benchmark is defined over.
const DEFAULT_TRACE: &str = "/tmp/fanout_trace.log";

/// The optimistic model (the earlier probe's calibrated point).
const OPTIMISTIC: (u64, u64) = (200, 400);

/// The pessimistic model (the earlier probe's slow point).
const PESSIMISTIC: (u64, u64) = (700, 1200);

/// The fan-out size the multi-fan-out submission splits one anchor into.
const FANOUTS_PER_SUBMISSION: usize = 6;

fn env_us(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_micros)
}

fn env_count(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or(fallback)
}

/// What one invocation drives.
struct Config {
    trace: String,
    /// The `(storage, account)` latency points to report.
    points: Vec<(Duration, Duration)>,
    limits: FanoutLimits,
    /// Some(flag) when the negative probe is armed: the flag is raised for the
    /// `session` leg only, and the matrix narrows to the first point.
    plant: Option<Arc<AtomicBool>>,
}

impl Config {
    fn from_env() -> Self {
        let storage = env_us("SESSION_FANOUT_STORAGE_US");
        let account = env_us("SESSION_FANOUT_ACCOUNT_US");
        let points = match (storage, account) {
            (Some(storage), Some(account)) => vec![(storage, account)],
            (Some(storage), None) => vec![(storage, Duration::from_micros(OPTIMISTIC.1))],
            (None, Some(account)) => vec![(Duration::from_micros(OPTIMISTIC.0), account)],
            (None, None) => vec![
                (
                    Duration::from_micros(OPTIMISTIC.0),
                    Duration::from_micros(OPTIMISTIC.1),
                ),
                (
                    Duration::from_micros(PESSIMISTIC.0),
                    Duration::from_micros(PESSIMISTIC.1),
                ),
            ],
        };
        let limits = FanoutLimits::new(
            env_count("SESSION_FANOUT_W", 2),
            env_count("SESSION_FANOUT_F", 8),
        );
        let plant = std::env::var("SESSION_FANOUT_PLANT_DIVERGENCE")
            .is_ok()
            .then(|| Arc::new(AtomicBool::new(false)));
        Self {
            trace: std::env::var("SESSION_FANOUT_TRACE").unwrap_or_else(|_| DEFAULT_TRACE.into()),
            points,
            limits,
            plant,
        }
    }
}

// ── The recorded rows ─────────────────────────────────────────────────────

/// One recorded candidate: the read walk the simulation seam drained and the
/// execution wall it measured. The trace carries no calldata, so this is the
/// whole of what a replay can know about the candidate.
#[derive(Clone)]
struct Recorded {
    block: u64,
    sim_ns: u64,
    reads: Vec<(Address, Vec<U256>)>,
}

/// Parse one log line's trace row (the substring after `fanout-trace `).
fn parse_trace_line(line: &str) -> Option<Recorded> {
    let start = line.find(TRACE_TAG)? + TRACE_TAG.len();
    let value: Value = serde_json::from_str(line[start..].trim()).ok()?;
    let reads = value
        .get("reads")?
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let mut parts = entry.as_str()?.split('|');
            let address = parts.next()?.parse::<Address>().ok()?;
            let slots = parts
                .next()?
                .split(',')
                .filter(|slot| !slot.is_empty())
                .filter_map(|slot| slot.parse::<U256>().ok())
                .collect();
            Some((address, slots))
        })
        .collect();
    Some(Recorded {
        block: value.get("block")?.as_u64()?,
        sim_ns: value.get("sim_ns")?.as_u64()?,
        reads,
    })
}

/// Load every recorded row, failing loudly when the corpus is absent.
fn load_rows(path: &str) -> Result<Vec<Recorded>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        format!("read the recorded fan-out trace {path}: {error} (this benchmark has no corpus without it)")
    })?;
    let rows: Vec<Recorded> = text.lines().filter_map(parse_trace_line).collect();
    if rows.is_empty() {
        return Err(format!("no `{TRACE_TAG}` rows in {path}"));
    }
    Ok(rows)
}

/// Group the rows into fan-outs: one per recorded anchor, in file order.
fn group_by_anchor(rows: Vec<Recorded>) -> Vec<(u64, Vec<Recorded>)> {
    let mut groups: Vec<(u64, Vec<Recorded>)> = Vec::new();
    for row in rows {
        match groups.last_mut() {
            Some((block, group)) if *block == row.block => group.push(row),
            _ => groups.push((row.block, vec![row])),
        }
    }
    groups
}

// ── The modeled node ──────────────────────────────────────────────────────

/// The stub JSON-RPC transport behind the modeled node: every answer is a
/// function of the read key, and nothing is memoized here (the memo belongs to
/// the hub or to the leg's own map, as it does in production).
#[derive(Clone, Debug)]
struct StubChain {
    /// Raised by the negative probe: storage answers one wei high.
    plant: Arc<AtomicBool>,
}

impl StubChain {
    fn provider(&self) -> AlloyProvider {
        let client = ClientBuilder::default().transport(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    fn answer(&self, request: &SerializedRequest) -> Response {
        let payload = match self.serve(request) {
            Ok(value) => {
                ResponsePayload::Success(to_raw_value(&value).expect("a served answer serializes"))
            }
            Err(message) => ResponsePayload::Failure(ErrorPayload {
                code: -32_000,
                message: message.into(),
                data: None,
            }),
        };
        Response {
            id: request.id().clone(),
            payload,
        }
    }

    fn serve(&self, request: &SerializedRequest) -> Result<Value, String> {
        let params = request.params().ok_or("a member carries params")?;
        let params: Value = serde_json::from_str(params.get())
            .map_err(|error| format!("member params: {error}"))?;
        let address = params
            .get(0)
            .and_then(Value::as_str)
            .ok_or("the address is the first param")?
            .parse::<Address>()
            .map_err(|error| format!("member address: {error}"))?;
        match request.method() {
            "eth_getStorageAt" => {
                let slot = params
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or("the slot is the second param")?
                    .parse::<U256>()
                    .map_err(|error| format!("member slot: {error}"))?;
                let mut word = word_for(address, slot);
                if self.plant.load(Ordering::Relaxed) {
                    word = word.wrapping_add(U256::from(1u8));
                }
                Ok(json!(format!("{word:#x}")))
            }
            "eth_getBalance" => Ok(json!(format!("{:#x}", account_read(address).balance))),
            "eth_getTransactionCount" => Ok(json!(format!("{:#x}", account_read(address).nonce))),
            "eth_getCode" => Ok(json!(format!(
                "0x{}",
                alloy::hex::encode(&account_read(address).code)
            ))),
            other => Err(format!("the modeled node serves no {other}")),
        }
    }
}

impl tower::Service<RequestPacket> for StubChain {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let chain = self.clone();
        Box::pin(async move {
            let responses = match &request {
                RequestPacket::Single(one) => ResponsePacket::Single(chain.answer(one)),
                RequestPacket::Batch(many) => {
                    ResponsePacket::Batch(many.iter().map(|one| chain.answer(one)).collect())
                }
            };
            Ok(responses)
        })
    }
}

/// One byte-tagged derivation off a key's seed hash.
fn derived(tag: u8, seed: B256) -> [u8; 32] {
    let mut preimage = [0u8; 33];
    preimage[0] = tag;
    preimage[1..].copy_from_slice(seed.as_slice());
    keccak256(preimage).0
}

/// The word the modeled node serves for one slot.
fn word_for(address: Address, slot: U256) -> U256 {
    let mut preimage = [0u8; 52];
    preimage[..20].copy_from_slice(address.as_slice());
    preimage[20..].copy_from_slice(&slot.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// The balance/nonce/code triple the modeled node serves for one account.
fn account_read(address: Address) -> AccountRead {
    let seed = keccak256(address.as_slice());
    let nonce = derived(2, seed);
    let mut nonce_bytes = [0u8; 8];
    nonce_bytes.copy_from_slice(&nonce[..8]);
    AccountRead {
        balance: U256::from_be_bytes(derived(1, seed)),
        nonce: u64::from_be_bytes(nonce_bytes),
        code: Bytes::copy_from_slice(&derived(3, seed)),
    }
}

// ── The cost model ────────────────────────────────────────────────────────

/// Which round trip a read costs.
#[derive(Clone, Copy, Debug)]
enum ReadKind {
    Storage,
    Account,
}

/// The latency model every leg pays: one round trip per dispatched packet.
#[derive(Clone, Copy, Debug)]
struct Node {
    storage: Duration,
    account: Duration,
}

impl Node {
    const fn new(storage: Duration, account: Duration) -> Self {
        Self { storage, account }
    }

    const fn latency(self, kind: ReadKind) -> Duration {
        match kind {
            ReadKind::Storage => self.storage,
            ReadKind::Account => self.account,
        }
    }

    /// One serial read's round trip.
    ///
    /// The model spends the wait on the thread that owns the read (today's
    /// `BlockingAsyncDb` shape). A blocking sleep, not a timer: the modeled
    /// prices are hundreds of microseconds and a timer wheel's tick would round
    /// them up to whole milliseconds.
    fn charge_read(self, kind: ReadKind) {
        std::thread::sleep(self.latency(kind));
    }

    /// One drained round's round trip: the hub issues its storage and account
    /// batches concurrently, so the round costs the most expensive kind it
    /// carried. Charged on the lane that owns the round — the read lane is
    /// occupied for the round's whole duration either way.
    fn charge_round(self, storage: bool, account: bool) {
        let wait = match (storage, account) {
            (true, true) => self.storage.max(self.account),
            (true, false) => self.storage,
            (false, true) => self.account,
            (false, false) => Duration::ZERO,
        };
        std::thread::sleep(wait);
    }
}

// ── The leg facts ─────────────────────────────────────────────────────────

/// Per-candidate read digests, keyed by `(anchor, index within the anchor)`.
type Digests = BTreeMap<(u64, usize), B256>;

/// One leg's measured shape over one or more fan-outs.
struct LegFacts {
    label: &'static str,
    wall: Duration,
    /// Round trips paid: one per drained hub round, one per serial read.
    rounds: u64,
    storage_fetches: u64,
    account_fetches: u64,
    memo_hits: u64,
    coalesced: u64,
    peak_active: usize,
    lanes: usize,
    slots: Option<usize>,
    digests: Digests,
}

#[derive(Clone, Default)]
struct Totals {
    label: &'static str,
    wall: Duration,
    rounds: u64,
    storage_fetches: u64,
    account_fetches: u64,
    memo_hits: u64,
    coalesced: u64,
    peak_active: usize,
    lanes: usize,
    slots: Option<usize>,
}

impl Totals {
    fn add(&mut self, facts: &LegFacts) {
        self.label = facts.label;
        self.wall += facts.wall;
        self.rounds += facts.rounds;
        self.storage_fetches += facts.storage_fetches;
        self.account_fetches += facts.account_fetches;
        self.memo_hits += facts.memo_hits;
        self.coalesced += facts.coalesced;
        self.peak_active = self.peak_active.max(facts.peak_active);
        self.lanes = self.lanes.max(facts.lanes);
        self.slots = facts.slots.or(self.slots);
    }
}

/// The digest material of one account read.
fn extend_account(material: &mut Vec<u8>, read: &AccountRead) {
    material.extend_from_slice(&read.balance.to_be_bytes::<32>());
    material.extend_from_slice(&read.nonce.to_be_bytes());
    material.extend_from_slice(&read.code);
}

/// Spend a candidate's recorded execution cost as wall (see the module docs).
fn exec_pause(row: &Recorded) {
    std::thread::sleep(Duration::from_nanos(row.sim_ns));
}

// ── Lanes ─────────────────────────────────────────────────────────────────

/// How a leg schedules its candidates.
#[derive(Clone, Copy, Debug)]
struct LanePlan {
    limits: FanoutLimits,
    /// One shared hub for the whole leg, or one hub per lane.
    shared_hub: bool,
    /// Bound each lane to `FanoutLimits::lane_capacity`, or leave it open.
    bounded: bool,
}

/// Whether `SESSION_FANOUT_DEBUG` asks for the lane progress trace.
fn debug_enabled() -> bool {
    std::env::var("SESSION_FANOUT_DEBUG").is_ok()
}

/// The session-wide in-flight gauge: the peak is a real temporal peak across
/// lanes, never a sum of per-lane maxima.
#[derive(Default)]
struct Gauge {
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl Gauge {
    fn admitted(&self) {
        let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak.fetch_max(active, Ordering::Relaxed);
    }

    fn released(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }
}

type WalkFuture<'h> = Pin<Box<dyn Future<Output = Result<B256, String>> + 'h>>;

/// One recorded candidate's read walk on one fiber: every read registers against
/// the hub and parks until a round settles it, then the recorded execution is
/// spent in the slot.
fn walk_fiber<'h>(hub: &'h ReadHub<'_>, row: &'h Recorded) -> WalkFuture<'h> {
    Box::pin(async move {
        let mut material: Vec<u8> = Vec::new();
        for (address, slots) in &row.reads {
            let read = hub
                .read_account(*address)
                .await
                .map_err(|error| format!("hub account read of {address}: {error}"))?;
            extend_account(&mut material, &read);
            for slot in slots {
                let value = hub
                    .read_storage(*address, *slot)
                    .await
                    .map_err(|error| format!("hub storage read of {address}/{slot}: {error}"))?;
                material.extend_from_slice(&value.to_be_bytes::<32>());
            }
        }
        exec_pause(row);
        Ok(keccak256(&material))
    })
}

/// Drain one round and charge its modeled round trip. `false` when nothing was
/// queued (another round already owns every pending identity).
async fn settle(hub: &ReadHub<'_>, node: Node) -> bool {
    let report = hub.drain_round().await;
    if report.is_empty() {
        return false;
    }
    node.charge_round(report.storage_members > 0, report.account_members > 0);
    true
}

/// One lane's inputs: its plan, its rows, and the read lane it shares.
struct LaneRun<'r, 'p> {
    plan: LanePlan,
    base: usize,
    rows: &'r [Recorded],
    node: Node,
    provider: &'p AlloyProvider,
    shared: Option<&'r ReadHub<'p>>,
    role: Option<&'r tokio::sync::Mutex<()>>,
    gauge: &'r Gauge,
}

struct LaneOutcome {
    digests: Digests,
    stats: Option<ReadHubStats>,
}

/// Drive one lane: admit into free slots, poll, and settle a round whenever
/// nothing this lane owns can progress.
async fn drive_lane(run: &LaneRun<'_, '_>, worker: usize) -> Result<LaneOutcome, String> {
    let lanes = run.plan.limits.effective_workers();
    let capacity = if run.plan.bounded {
        run.plan.limits.lane_capacity(worker)
    } else {
        usize::MAX
    };
    let mut queue: VecDeque<usize> = (worker..run.rows.len()).step_by(lanes).collect();
    let local = run.shared.is_none().then(|| {
        ReadHub::new(
            run.provider,
            ReadAnchor::at(run.rows.first().map_or(0, |r| r.block)),
        )
    });
    let hub: &ReadHub<'_> = run
        .shared
        .unwrap_or_else(|| local.as_ref().expect("a per-worker lane owns its hub"));
    // A no-op waker: this driver polls every parked fiber on every pass and
    // drains a round whenever the pass neither completed nor admitted anything,
    // so it never needs the wake hint. (A counting waker is unusable here:
    // re-polling a read that is waiting on its round re-arms a wake each time,
    // which would make "somebody woke me" permanently true and starve the
    // drain.)
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut active: Vec<(usize, WalkFuture<'_>)> = Vec::new();
    let mut digests = Digests::new();
    let mut idle_rounds = 0usize;
    let mut passes = 0u64;
    let debug = debug_enabled();
    while !queue.is_empty() || !active.is_empty() {
        passes += 1;
        if debug && passes % 200 == 1 {
            println!(
                "    debug: lane {worker} pass {passes} active={} queued={} done={}",
                active.len(),
                queue.len(),
                digests.len()
            );
        }
        while active.len() < capacity {
            let Some(index) = queue.pop_front() else {
                break;
            };
            active.push((index, walk_fiber(hub, &run.rows[index])));
            run.gauge.admitted();
        }
        let mut ready: Vec<(usize, usize, B256)> = Vec::new();
        for (position, entry) in active.iter_mut().enumerate() {
            let index = entry.0;
            match entry.1.as_mut().poll(&mut cx) {
                Poll::Ready(Ok(digest)) => ready.push((position, index, digest)),
                Poll::Ready(Err(error)) => return Err(error),
                Poll::Pending => {}
            }
        }
        let completed = !ready.is_empty();
        for (position, index, digest) in ready.into_iter().rev() {
            let _dropped = active.swap_remove(position);
            run.gauge.released();
            digests.insert((run.rows[index].block, run.base + index), digest);
        }
        if debug && passes % 200 == 1 {
            let stats = hub.stats();
            println!(
                "    debug: lane {worker} pass {passes} later completed={completed} \
                 rounds={} memo={} coalesced={} slots={}",
                stats.rounds,
                stats.memo_hits,
                stats.coalesced,
                hub.memoized_slots()
            );
        }
        if completed {
            idle_rounds = 0;
            continue;
        }
        let settled = match run.role {
            Some(role) => {
                let _only_one_round = role.lock().await;
                settle(hub, run.node).await
            }
            None => settle(hub, run.node).await,
        };
        if settled {
            idle_rounds = 0;
            continue;
        }
        idle_rounds += 1;
        if idle_rounds > MAX_IDLE_ROUNDS {
            let mut waiting: Vec<usize> = queue.iter().copied().collect();
            waiting.extend(active.iter().map(|(index, _)| *index));
            waiting.sort_unstable();
            return Err(format!(
                "lane {worker} stalled with candidates {waiting:?} waiting and no read round to settle"
            ));
        }
    }
    Ok(LaneOutcome {
        digests,
        stats: local.as_ref().map(ReadHub::stats),
    })
}

/// The summed hub counters of a per-worker-hub leg.
fn sum_stats(left: ReadHubStats, right: ReadHubStats) -> ReadHubStats {
    ReadHubStats {
        memo_hits: left.memo_hits + right.memo_hits,
        coalesced: left.coalesced + right.coalesced,
        rounds: left.rounds + right.rounds,
        storage_members: left.storage_members + right.storage_members,
        account_members: left.account_members + right.account_members,
        storage_member_errors: left.storage_member_errors + right.storage_member_errors,
        account_member_errors: left.account_member_errors + right.account_member_errors,
    }
}

/// Drive `rows` with `plan`'s shape and report what the schedule did.
///
/// `base` is the index of `rows[0]` within its anchor's full fan-out, so a
/// chunked submission keys its digests exactly as the whole fan-out would.
async fn drive_lanes(
    label: &'static str,
    plan: LanePlan,
    rows: &[Recorded],
    base: usize,
    provider: &AlloyProvider,
    node: Node,
) -> Result<LegFacts, String> {
    let limits = plan.limits;
    let lanes = limits.effective_workers();
    let gauge = Gauge::default();
    let block = rows.first().map_or(0, |row| row.block);
    let shared = plan
        .shared_hub
        .then(|| ReadHub::new(provider, ReadAnchor::at(block)));
    let role = tokio::sync::Mutex::new(());
    let run = LaneRun {
        plan,
        base,
        rows,
        node,
        provider,
        shared: shared.as_ref(),
        role: plan.shared_hub.then_some(&role),
        gauge: &gauge,
    };
    let started = Instant::now();
    let outcomes: Vec<Result<LaneOutcome, String>> = if lanes == 1 {
        vec![drive_lane(&run, 0).await]
    } else {
        // The lane's futures are thread-local by construction (they borrow the
        // hub), so each lane is built and polled on the thread that owns it, the
        // way the committed driver does it.
        let handle = tokio::runtime::Handle::current();
        std::thread::scope(|scope| {
            let joins: Vec<_> = (0..lanes)
                .map(|worker| {
                    let handle = handle.clone();
                    let run = &run;
                    scope.spawn(move || handle.block_on(drive_lane(run, worker)))
                })
                .collect();
            joins
                .into_iter()
                .map(|join| match join.join() {
                    Ok(outcome) => outcome,
                    Err(panic) => std::panic::resume_unwind(panic),
                })
                .collect()
        })
    };
    let wall = started.elapsed();
    let mut digests = Digests::new();
    let mut stats = ReadHubStats::default();
    for outcome in outcomes {
        let outcome = outcome?;
        digests.extend(outcome.digests);
        if let Some(lane_stats) = outcome.stats {
            stats = sum_stats(stats, lane_stats);
        }
    }
    if let Some(hub) = shared.as_ref() {
        // A shared hub's counters are already session-wide: read them once.
        stats = hub.stats();
    }
    Ok(LegFacts {
        label,
        wall,
        rounds: stats.rounds,
        storage_fetches: stats.storage_members,
        account_fetches: stats.account_members,
        memo_hits: stats.memo_hits,
        coalesced: stats.coalesced,
        peak_active: gauge.peak(),
        lanes,
        slots: plan.bounded.then_some(limits.slots()),
        digests,
    })
}

// ── The serial leg ────────────────────────────────────────────────────────

/// Today's shape: one read in flight at a time over the production provider
/// primitives, accounts always forwarded, storage memoized per anchor.
async fn drive_serial(
    rows: &[Recorded],
    provider: &AlloyProvider,
    node: Node,
) -> Result<LegFacts, String> {
    let block = rows.first().map_or(0, |row| row.block);
    let mut memo: HashMap<(Address, U256), U256> = HashMap::new();
    let mut digests = Digests::new();
    let mut rounds = 0u64;
    let mut storage_fetches = 0u64;
    let mut account_fetches = 0u64;
    let mut memo_hits = 0u64;
    let started = Instant::now();
    for (index, row) in rows.iter().enumerate() {
        let mut material: Vec<u8> = Vec::new();
        for (address, slots) in &row.reads {
            let read = provider
                .get_account_at(address, block)
                .await
                .map_err(|error| format!("serial account read of {address}: {error}"))?;
            node.charge_read(ReadKind::Account);
            account_fetches += 1;
            rounds += 1;
            extend_account(&mut material, &read);
            for slot in slots {
                let value = if let Some(value) = memo.get(&(*address, *slot)).copied() {
                    memo_hits += 1;
                    value
                } else {
                    let word = provider
                        .get_storage_at(address, *slot, Some(block))
                        .await
                        .map_err(|error| {
                            format!("serial storage read of {address}/{slot}: {error}")
                        })?;
                    node.charge_read(ReadKind::Storage);
                    storage_fetches += 1;
                    rounds += 1;
                    let value = U256::from_be_bytes(word.0);
                    memo.insert((*address, *slot), value);
                    value
                };
                material.extend_from_slice(&value.to_be_bytes::<32>());
            }
        }
        exec_pause(row);
        digests.insert((row.block, index), keccak256(&material));
    }
    Ok(LegFacts {
        label: "serial",
        wall: started.elapsed(),
        rounds,
        storage_fetches,
        account_fetches,
        memo_hits,
        coalesced: 0,
        peak_active: 1,
        lanes: 1,
        slots: None,
        digests,
    })
}

// ── One anchor's four legs ────────────────────────────────────────────────

async fn drive_fanout(
    config: &Config,
    block: u64,
    rows: &[Recorded],
    node: Node,
    provider: &AlloyProvider,
) -> Result<Vec<LegFacts>, String> {
    let debug = debug_enabled();
    if debug {
        println!(
            "    debug: anchor {block} {} candidates: serial",
            rows.len()
        );
    }
    let serial = drive_serial(rows, provider, node).await?;
    if debug {
        println!("    debug: anchor {block}: hybrid");
    }
    let hybrid = drive_lanes(
        "hybrid",
        LanePlan {
            limits: config.limits,
            shared_hub: false,
            bounded: false,
        },
        rows,
        0,
        provider,
        node,
    )
    .await?;
    let fibers = drive_lanes(
        "fibers",
        LanePlan {
            limits: FanoutLimits::new(1, 1),
            shared_hub: true,
            bounded: false,
        },
        rows,
        0,
        provider,
        node,
    )
    .await?;
    if debug {
        println!("    debug: anchor {block}: session");
    }
    if let Some(flag) = &config.plant {
        flag.store(true, Ordering::Relaxed);
    }
    let session = drive_lanes(
        "session",
        LanePlan {
            limits: config.limits,
            shared_hub: true,
            bounded: true,
        },
        rows,
        0,
        provider,
        node,
    )
    .await?;
    if let Some(flag) = &config.plant {
        flag.store(false, Ordering::Relaxed);
    }
    Ok(vec![serial, hybrid, fibers, session])
}

// ── Report ────────────────────────────────────────────────────────────────

fn slots_label(slots: Option<usize>) -> String {
    slots.map_or_else(|| "-".to_string(), |slots| slots.to_string())
}

fn print_leg(block: u64, cands: usize, facts: &LegFacts) {
    println!(
        "  {:>8}  {:>5}  {:<8} {:>10}us {:>8} {:>8} {:>7} {:>7} {:>8} {}/{} {:>5}",
        block,
        cands,
        facts.label,
        facts.wall.as_micros(),
        facts.rounds,
        facts.storage_fetches,
        facts.account_fetches,
        facts.memo_hits,
        facts.coalesced,
        facts.lanes,
        slots_label(facts.slots),
        facts.peak_active,
    );
}

/// `leg / whole` as a speedup multiple, in hundredths (no float casts).
fn speedup(whole: Duration, part: Duration) -> String {
    if part.is_zero() {
        return "-".to_string();
    }
    let hundredths = whole.as_micros() * 100 / part.as_micros().max(1);
    format!("x{}.{:02}", hundredths / 100, hundredths % 100)
}

fn print_totals(totals: &[Totals]) {
    let base = totals.first().map_or(Duration::ZERO, |one| one.wall);
    print!("  totals  ");
    for total in totals {
        print!(
            "| {:<8} {:>9}us {}  ",
            total.label,
            total.wall.as_micros(),
            speedup(base, total.wall)
        );
    }
    println!();
    for total in totals {
        println!(
            "    {:<8} rounds={} storage_fetches={} account_fetches={} memo_hits={} \
             coalesced={} lanes={} slots={} peak={}",
            total.label,
            total.rounds,
            total.storage_fetches,
            total.account_fetches,
            total.memo_hits,
            total.coalesced,
            total.lanes,
            slots_label(total.slots),
            total.peak_active,
        );
    }
}

// ── The matrix ────────────────────────────────────────────────────────────

/// Every leg of one latency point over every recorded anchor.
async fn drive_point(
    config: &Config,
    fanouts: &[(u64, Vec<Recorded>)],
    node: Node,
    provider: &AlloyProvider,
    observed: &mut Vec<(String, Digests)>,
) -> Result<[Totals; 4], String> {
    let mut totals: [Totals; 4] = Default::default();
    for (block, rows) in fanouts {
        let legs = drive_fanout(config, *block, rows, node, provider).await?;
        for (index, facts) in legs.iter().enumerate() {
            print_leg(*block, rows.len(), facts);
            totals[index].add(facts);
            observed.push((format!("{}/{block}", facts.label), facts.digests.clone()));
        }
    }
    Ok(totals)
}

/// The slot-count sensitivity: the same anchor at a range of slot budgets.
async fn sensitivity(
    config: &Config,
    anchor: &(u64, Vec<Recorded>),
    node: Node,
    provider: &AlloyProvider,
    observed: &mut Vec<(String, Digests)>,
) -> Result<(), String> {
    let (block, rows) = anchor;
    println!(
        "\nslot-count sensitivity (anchor {block}, {} candidates, W={}):",
        rows.len(),
        config.limits.workers()
    );
    println!(
        "  {:>4} {:>6} {:>11} {:>8} {:>8} {:>8} {:>6} {:>10}",
        "F", "lanes", "wall_us", "rounds", "storage", "account", "peak", "vs F=1"
    );
    let mut serial_wall = Duration::ZERO;
    for slots in [1usize, 2, 4, 8, 16, 32] {
        let plan = LanePlan {
            limits: FanoutLimits::new(config.limits.workers(), slots),
            shared_hub: true,
            bounded: true,
        };
        let facts = drive_lanes("session", plan, rows, 0, provider, node).await?;
        if slots == 1 {
            serial_wall = facts.wall;
        }
        println!(
            "  {:>4} {:>6} {:>11} {:>8} {:>8} {:>8} {:>6} {:>10}",
            slots,
            facts.lanes,
            facts.wall.as_micros(),
            facts.rounds,
            facts.storage_fetches,
            facts.account_fetches,
            facts.peak_active,
            speedup(serial_wall, facts.wall),
        );
        observed.push((format!("session-f{slots}/{block}"), facts.digests));
    }
    Ok(())
}

/// Merge several legs of one submission into one report line.
fn merge_legs(label: &'static str, facts: Vec<LegFacts>, lanes: usize, slots: usize) -> LegFacts {
    let mut merged = LegFacts {
        label,
        wall: Duration::ZERO,
        rounds: 0,
        storage_fetches: 0,
        account_fetches: 0,
        memo_hits: 0,
        coalesced: 0,
        peak_active: 0,
        lanes,
        slots: Some(slots),
        digests: Digests::new(),
    };
    for one in facts {
        merged.wall = merged.wall.max(one.wall);
        merged.rounds += one.rounds;
        merged.storage_fetches += one.storage_fetches;
        merged.account_fetches += one.account_fetches;
        merged.memo_hits += one.memo_hits;
        merged.coalesced += one.coalesced;
        merged.peak_active += one.peak_active;
        merged.digests.extend(one.digests);
    }
    merged
}

/// The multi-fan-out submission: the same candidates and the same anchor, once
/// as ONE submission under the session's single bound and once as one session
/// per fan-out (the shape whose workers multiply).
async fn multi_fan_out(
    config: &Config,
    anchor: &(u64, Vec<Recorded>),
    node: Node,
    provider: &AlloyProvider,
    observed: &mut Vec<(String, Digests)>,
) -> Result<(), String> {
    let (block, rows) = anchor;
    let chunk = rows.len().div_ceil(FANOUTS_PER_SUBMISSION);
    let chunks: Vec<Vec<Recorded>> = rows.chunks(chunk).map(<[Recorded]>::to_vec).collect();
    let plan = LanePlan {
        limits: config.limits,
        shared_hub: true,
        bounded: true,
    };
    println!(
        "\nmulti-fan-out submission (anchor {block}: {} candidates as {} fan-outs of <= {chunk}):",
        rows.len(),
        chunks.len()
    );
    let pooled = drive_lanes("pooled", plan, rows, 0, provider, node).await?;
    print_leg(*block, rows.len(), &pooled);
    println!(
        "    one submission: {} fan-outs share ONE hub, {} lanes and {} slots",
        chunks.len(),
        pooled.lanes,
        slots_label(pooled.slots)
    );
    observed.push((format!("pooled/{block}"), pooled.digests.clone()));

    let started = Instant::now();
    let futures: Vec<_> = chunks
        .iter()
        .enumerate()
        .map(|(index, part)| drive_lanes("chunk", plan, part, index * chunk, provider, node))
        .collect();
    let legs = futures_util::future::join_all(futures)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, String>>()?;
    let mut split = merge_legs(
        "per-fanout",
        legs,
        pooled.lanes * chunks.len(),
        config.limits.slots() * chunks.len(),
    );
    split.wall = started.elapsed();
    print_leg(*block, rows.len(), &split);
    println!(
        "    per-fan-out sessions: {} x ({} lanes, {} slots) = {} lanes, {} slots; peak is the \
         sum of the sessions' own peaks",
        chunks.len(),
        pooled.lanes,
        slots_label(pooled.slots),
        split.lanes,
        slots_label(split.slots)
    );
    println!(
        "    pooled rounds={} vs per-fan-out rounds={} for the same reads",
        pooled.rounds, split.rounds
    );
    observed.push((format!("per-fanout/{block}"), split.digests));
    Ok(())
}

/// Compare every leg's read digests against the serial baseline.
fn identity_gate(baseline: &Digests, legs: &[(String, Digests)]) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (label, digests) in legs {
        if digests.is_empty() {
            mismatches.push(format!("{label}: replayed no candidate"));
            continue;
        }
        for (key, got) in digests {
            match baseline.get(key) {
                None => mismatches.push(format!("{label}: {key:?} is not in the baseline")),
                Some(want) if want != got => {
                    mismatches.push(format!("{label}: {key:?} digest {got} != baseline {want}"));
                }
                Some(_) => {}
            }
        }
    }
    mismatches
}

// ── Entry ─────────────────────────────────────────────────────────────────

/// The run's preamble: what was loaded, the bound, and the model's two rules.
fn print_header(config: &Config, candidates: usize, anchors: usize) {
    let limits = config.limits;
    println!(
        "session_fanout_probe: trace={} rows={candidates} anchors={anchors} W={} F={} \
         (lane capacity {}..{})",
        config.trace,
        limits.workers(),
        limits.slots(),
        limits.lane_capacity(0),
        limits.lane_capacity(limits.effective_workers() - 1),
    );
    println!(
        "  model: one round trip per dispatched packet; a hub round costs the max kind it \
         carried; the recorded sim_ns is SPENT as wall inside the candidate's slot"
    );
    println!(
        "  driven: the read-hub scheduling seam (the trace carries no calldata, so no EVM \
         sequence is reconstructable; call/slot parity is the committed test suite's)"
    );
}

/// Every latency point over every recorded anchor.
async fn drive_points(
    config: &Config,
    fanouts: &[(u64, Vec<Recorded>)],
    provider: &AlloyProvider,
    observed: &mut Vec<(String, Digests)>,
) -> Result<(), String> {
    let points: &[(Duration, Duration)] = if config.plant.is_some() {
        &config.points[..1]
    } else {
        &config.points
    };
    for (storage, account) in points {
        let node = Node::new(*storage, *account);
        println!(
            "\nlatency point: storage {}us / account {}us",
            storage.as_micros(),
            account.as_micros()
        );
        println!(
            "  {:>8}  {:>5}  {:<8} {:>10}   {:>8} {:>8} {:>7} {:>7} {:>8} {:>5} {:>5}",
            "anchor",
            "cands",
            "leg",
            "wall_us",
            "rounds",
            "st-fetch",
            "ac-fetch",
            "memo",
            "single",
            "W/F",
            "peak"
        );
        let totals = drive_point(config, fanouts, node, provider, observed).await?;
        print_totals(&totals);
    }
    Ok(())
}

/// Compare every leg against the serial baseline and report the gate.
fn report_gate(observed: Vec<(String, Digests)>, candidates: usize) -> Result<(), String> {
    // The baseline is every serial leg over every recorded anchor: one entry per
    // recorded candidate, keyed by `(anchor, index within the anchor)`.
    let baseline: Digests = observed
        .iter()
        .filter(|(label, _)| label.starts_with("serial/"))
        .flat_map(|(_, digests)| digests.iter().map(|(key, digest)| (*key, *digest)))
        .collect();
    if baseline.len() != candidates {
        return Err(format!(
            "the serial baseline replayed {} of {candidates} recorded candidates",
            baseline.len()
        ));
    }
    let legs: Vec<(String, Digests)> = observed
        .into_iter()
        .filter(|(label, _)| !label.starts_with("serial/"))
        .collect();
    let mismatches = identity_gate(&baseline, &legs);
    println!();
    if mismatches.is_empty() {
        println!(
            "identity gate: GREEN ({} legs x {} candidates)",
            legs.len(),
            baseline.len()
        );
        return Ok(());
    }
    println!("identity gate: RED ({} mismatches)", mismatches.len());
    for mismatch in mismatches.iter().take(5) {
        println!("  {mismatch}");
    }
    Err("replay identity gate red".to_string())
}

async fn run() -> Result<(), String> {
    let config = Config::from_env();
    let rows = load_rows(&config.trace)?;
    let fanouts = group_by_anchor(rows);
    let candidates: usize = fanouts.iter().map(|(_, rows)| rows.len()).sum();
    let plant = config
        .plant
        .clone()
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let provider = StubChain { plant }.provider();
    print_header(&config, candidates, fanouts.len());

    let mut observed: Vec<(String, Digests)> = Vec::new();
    drive_points(&config, &fanouts, &provider, &mut observed).await?;

    if config.plant.is_none() {
        let largest = fanouts
            .iter()
            .max_by_key(|(_, rows)| rows.len())
            .expect("a non-empty trace has at least one anchor");
        let node = Node::new(config.points[0].0, config.points[0].1);
        sensitivity(&config, largest, node, &provider, &mut observed).await?;
        multi_fan_out(&config, largest, node, &provider, &mut observed).await?;
    }
    report_gate(observed, candidates)
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("session_fanout_probe: {error}");
        std::process::exit(1);
    }
}
