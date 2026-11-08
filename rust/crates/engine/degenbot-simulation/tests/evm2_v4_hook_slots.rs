//! ADR-090 H2's acceptance evidence: a hook's learned slot set turns its
//! SECOND quote into warm reads.
//!
//! One question: after a black-box quote has learned a hook's touched-slot
//! set (and the store has persisted it), does the next quote of the same leg —
//! at a NEW anchor, over a fresh scratch — forward ~0 hook-attributed cold
//! reads, because the learned set rode the anchor's one-batch prefetch?
//!
//! The census is the H1 fixture's real mainnet hooked pools. For each of the
//! first few hooks that can be priced:
//!
//! 1. **quote #1** over an anchor-pinned scratch with an EMPTY memo. Its hook
//!    reads reach the node one slot at a time, and the quote's own touched set
//!    teaches the store.
//! 2. **the store reloads from disk** — the set is a fact about the hook, not
//!    about the process that learned it.
//! 3. **one prefetch batch** at the NEXT anchor: the path derivation PLUS the
//!    learned hook set, in one round trip ([`prefetch_paths_into_memo`]).
//! 4. **quote #2** at that anchor over a fresh scratch sharing the seeded
//!    memo. Hook-attributed cold reads must be zero: every hook slot the quote
//!    reads is a member the batch already seeded, and every value is the
//!    NODE's at that anchor (nothing is cached across anchors).
//!
//! Then the degrade contract: a corrupted record is an empty set (no error, no
//! panic), the quote still prices identically, and the set re-learns.
//!
//! # Running it
//!
//! ```sh
//! cargo test -p degenbot-simulation --test evm2_v4_hook_slots \
//!   hook_slots_warm_the_second_quote -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `DEGENBOT_RPC_HTTP_CHAINID_1` (or `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`)
//! selects the node; both anchors need archive state.

#![expect(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines
)]

use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use alloy::primitives::{address, Address, Bytes, B256, U256};

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, CacheDB, Database, Db};

use degenbot_execution::{HopInfo, PathInfo, V4HopInfo};
use degenbot_executor::compute_simulation_warmup_slots;
use degenbot_pools::{v4_pool_id, V4PoolKey};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::bot_state_db::BotStateDb;
use degenbot_simulation::sim::evm2::frame_replay::Evm2ScratchEvm;
use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;
use degenbot_simulation::sim::evm2::hook_slots::{HookSlotStore, MAX_HOOK_SLOTS};
use degenbot_simulation::sim::evm2::v4_black_box::{
    leg_state_slots, V4BlackBoxQuoter, V4Leg, V4_POOL_MANAGER,
};
use degenbot_simulation::sim::evm2::warm_code_cache::{WarmCodeCache, WarmCodeCacheInner};
use degenbot_simulation::sim::frame_replay::ScratchBlock;
use degenbot_simulation::sim::prefetch::{
    prefetch_paths_into_memo, FamilySlotLayout, LearnedSets, PrefetchFacts,
};
use degenbot_simulation::sim::{HotSlotStore, StorageMemo};
use degenbot_substrate::NO_SIM_ANCHOR;

// ─────────────────────────────────────────────────────────────────────────
// The anchors + the census
// ─────────────────────────────────────────────────────────────────────────

/// The archived anchor every first quote reads: the H1 census's own block.
const PIN: u64 = 26_320_000;

/// The anchor the second quote reads — one block on, so the memo the prefetch
/// seeds is a NEW anchor's and every value it holds is that block's.
const NEXT: u64 = PIN + 1;

/// How many hooked pools the run walks (the first priceable ones in census
/// order — census order is the top hooks by pool count first).
const TARGET_HOOKS: usize = 4;

/// `hook  currency0  currency1  fee  tick_spacing  pool_id` (the H1 fixture).
const CENSUS: &str = include_str!("fixtures/v4_hook_census.tsv");

/// WETH9 — the executor-side WETH `balanceOf` word lives at its address.
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

/// `decimal()` — the only token-metadata read the census needs.
const DECIMALS_CALLDATA: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

fn rpc_url() -> String {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .expect(
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) \
             to an archive mainnet endpoint",
        )
}

/// Parse the census, re-deriving each pool id from its key (the row's
/// integrity check).
fn census() -> Vec<(Address, V4PoolKey)> {
    let mut rows = Vec::new();
    for line in CENSUS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 6, "census row shape: {line:?}");
        let hook = Address::from_str(fields[0]).expect("hook address");
        let key = V4PoolKey {
            currency0: Address::from_str(fields[1]).expect("currency0"),
            currency1: Address::from_str(fields[2]).expect("currency1"),
            fee: fields[3].parse().expect("fee"),
            tick_spacing: fields[4].parse().expect("tick spacing"),
            hooks: hook,
        };
        let pinned = B256::from_str(fields[5]).expect("pool id");
        assert_eq!(
            v4_pool_id(&key),
            pinned,
            "census row's key does not hash to its recorded pool id: {line:?}"
        );
        rows.push((hook, key));
    }
    rows
}

/// A token's decimals at the anchor, defaulting to 18 where the metadata is
/// absent.
async fn token_decimals(provider: &AlloyProvider, token: Address) -> u32 {
    if token.is_zero() {
        return 18;
    }
    let Ok(output) = provider
        .eth_call(&token, Bytes::from_static(&DECIMALS_CALLDATA), Some(PIN))
        .await
    else {
        return 18;
    };
    output.get(..32).map_or(18, |word| {
        U256::from_be_slice(word).saturating_to::<u32>().min(30)
    })
}

// ─────────────────────────────────────────────────────────────────────────
// The scratch stack + the node-read counter
// ─────────────────────────────────────────────────────────────────────────

/// Every storage read that reaches the NODE, by `(address, slot)`. The
/// production scratch layers its counter ABOVE the storage memo, so a memo hit
/// is still a "cold read" there; this probe sits below the memo (the last layer
/// before the RPC adapter), so what it records is exactly what the node was
/// asked — the number the prefetch is supposed to drive to zero.
struct CountingDb<Ext> {
    inner: Ext,
    reads: Arc<Mutex<Vec<(Address, U256)>>>,
}

impl<Ext: Database> Database for CountingDb<Ext> {
    type Error = Ext::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.inner.get_account(address)
    }

    fn get_code_by_hash(&mut self, hash: &B256) -> Result<Bytecode, Self::Error> {
        self.inner.get_code_by_hash(hash)
    }

    fn get_storage(&mut self, address: &Address, key: &U256) -> Result<U256, Self::Error> {
        self.reads.lock().unwrap().push((*address, *key));
        self.inner.get_storage(address, key)
    }

    fn get_block_hash(&mut self, number: &U256) -> Result<B256, Self::Error> {
        self.inner.get_block_hash(number)
    }
}

/// The production scratch stack's ext (the handle's `Evm2ScratchDb` shape) with
/// the node-read probe in the cold path.
type ScratchExt<'a> =
    CacheDB<Db<WarmCodeCache<BotStateDb<'static, CountingDb<BlockingAsyncDb<'a>>>>>>;

/// A fresh anchor-pinned scratch over `memo` — the probe's read recording goes
/// to `reads`.
fn scratch<'a>(
    provider: &'a AlloyProvider,
    anchor: u64,
    block: &ScratchBlock,
    memo: Arc<StorageMemo>,
    warm: &Arc<parking_lot::RwLock<WarmCodeCacheInner>>,
    reads: Arc<Mutex<Vec<(Address, U256)>>>,
) -> Evm2ScratchEvm<ScratchExt<'a>> {
    let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(provider, anchor))
        .expect("a multi-threaded runtime is ambient");
    let counting = CountingDb {
        inner: blocking,
        reads,
    };
    let memo_db = BotStateDb::new(&NO_SIM_ANCHOR, counting).with_storage_memo(memo);
    let warm_cache = WarmCodeCache::with_owner(Arc::clone(warm), anchor, memo_db);
    Evm2ScratchEvm::new(CacheDB::new(Db::new(warm_cache)), *block)
}

/// The storage reads the node served at `hook` since `from`.
fn reads_at(reads: &Arc<Mutex<Vec<(Address, U256)>>>, from: usize, hook: Address) -> usize {
    reads
        .lock()
        .unwrap()
        .iter()
        .skip(from)
        .filter(|(address, _)| *address == hook)
        .count()
}

/// A unique scratch directory removed on drop.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "degenbot-hook-slot-parity-{}-{sequence}-{label}",
            std::process::id()
        )))
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn record_path(&self, hook: Address) -> PathBuf {
        self.path().join(format!(
            "{}-s{}.json",
            alloy::hex::encode(hook),
            u32::from(EVM2_PRODUCTION_SPEC)
        ))
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One census row's outcome, for the table and the assertions.
struct Row {
    hook: Address,
    pool_id: B256,
    zero_for_one: bool,
    amount_in: U256,
    learned: usize,
    /// The learned hook slots from the FIRST quote.
    set: Vec<U256>,
    /// Hook-attributed storage reads the node served for quote #1.
    first_reads: usize,
    /// The prefetch batch's hook members.
    batch: usize,
    /// Hook-attributed storage reads the node served for quote #2.
    second_reads: usize,
}

// ─────────────────────────────────────────────────────────────────────────
// The run
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC hook-slot learning over archive mainnet state; run with --ignored"]
async fn hook_slots_warm_the_second_quote() {
    let started = std::time::Instant::now();
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    assert_eq!(
        provider.get_chain_id().await.unwrap(),
        1,
        "the census pools are mainnet fixtures"
    );
    let block = provider.get_block(PIN).await.unwrap().expect("archived");
    let next_block = provider.get_block(NEXT).await.unwrap().expect("archived");
    let scratch_block = |number: u64, timestamp: u64, base_fee: Option<u64>| ScratchBlock {
        number,
        timestamp,
        base_fee_next: u128::from(base_fee.unwrap_or(0)),
    };
    let at_pin = scratch_block(PIN, block.header.timestamp, block.header.base_fee_per_gas);
    let at_next = scratch_block(
        NEXT,
        next_block.header.timestamp,
        next_block.header.base_fee_per_gas,
    );

    let dir = ScratchDir::new("census");
    let store = Arc::new(
        HookSlotStore::new(dir.path(), EVM2_PRODUCTION_SPEC).expect("the scratch store opens"),
    );
    let warm = WarmCodeCacheInner::shared_default();
    let facts = PrefetchFacts {
        settlement: Some(degenbot_simulation::SettlementWarmup {
            weth_address: WETH,
            pool_manager_address: Some(V4_POOL_MANAGER),
            slots: compute_simulation_warmup_slots(Address::ZERO, WETH),
        }),
    };

    // Pick the first priceable pool per hook (the H1 fixture's own order).
    let rows = census();
    let mut chosen: Vec<(Address, V4PoolKey)> = Vec::new();
    let probe_reads = Arc::new(Mutex::new(Vec::new()));
    let mut probe = scratch(
        &provider,
        PIN,
        &at_pin,
        Arc::new(StorageMemo::new()),
        &warm,
        Arc::clone(&probe_reads),
    );
    for (hook, key) in &rows {
        if chosen.iter().any(|(seen, _)| seen == hook) {
            continue;
        }
        let liquidity_slot = leg_state_slots(key)[1];
        let liquidity = probe
            .ext_mut()
            .get_storage(&V4_POOL_MANAGER, &liquidity_slot)
            .unwrap_or(U256::ZERO);
        if liquidity.is_zero() {
            continue;
        }
        chosen.push((*hook, key.clone()));
        if chosen.len() == TARGET_HOOKS {
            break;
        }
    }

    println!("=== hook slot learning: quote #1 cold, quote #2 warm from the prefetch ===");
    println!(
        "anchors   : quote #1 at {PIN}, prefetch + quote #2 at {NEXT} \
         (spec {EVM2_PRODUCTION_SPEC:?}, per-hook cap {MAX_HOOK_SLOTS} slots)"
    );
    println!(
        "{:<12} {:<14} {:>8} {:>7} {:>11} {:>11} {:>11}",
        "hook", "pool", "learned", "batch", "1st reads", "2nd reads", "2nd touched"
    );

    let mut results: Vec<Row> = Vec::new();
    for (hook, key) in &chosen {
        let pool_id = v4_pool_id(key);
        let decimals = [
            token_decimals(&provider, key.currency0).await,
            token_decimals(&provider, key.currency1).await,
        ];

        let mut priced = None;
        for zero_for_one in [true, false] {
            let leg = V4Leg::canonical(key.clone(), zero_for_one);
            let input_decimals = decimals[usize::from(!zero_for_one)];
            let amount_in = U256::from(10u64).pow(U256::from(input_decimals.max(1)));
            let reads = Arc::new(Mutex::new(Vec::new()));
            let mut quoter = V4BlackBoxQuoter::default().with_hook_slots(Arc::clone(&store));
            let mut quote_scratch = scratch(
                &provider,
                PIN,
                &at_pin,
                Arc::new(StorageMemo::new()),
                &warm,
                Arc::clone(&reads),
            );
            if let Ok(quote) = quoter.quote(&mut quote_scratch, &leg, amount_in) {
                priced = Some((leg, amount_in, quote, reads));
                break;
            }
        }
        let Some((leg, amount_in, first_quote, first_reads_log)) = priced else {
            println!(
                "  hook 0x{} refused both directions at the anchor — not a learning row",
                alloy::hex::encode(&hook.as_slice()[..3])
            );
            continue;
        };

        // The set the first quote taught, and the store's persistence of it.
        let set = store.slots_for(*hook);
        assert_eq!(
            first_quote.hook_slots.len(),
            set.len(),
            "a cold store learns exactly the first quote's touched set at the hook's address"
        );
        let first_reads = reads_at(&first_reads_log, 0, *hook);

        // A fresh process over the same directory: the set is on the disk.
        let reopened = Arc::new(
            HookSlotStore::new(dir.path(), EVM2_PRODUCTION_SPEC).expect("the store reopens"),
        );
        assert_eq!(
            reopened.slots_for(*hook),
            set,
            "the learned set persists across store instances"
        );

        // ONE batch at the next anchor: the path derivation plus the learned
        // hook set, both read from the node at NEXT.
        let path = PathInfo::new(vec![HopInfo::V4(V4HopInfo {
            pool_manager_address: V4_POOL_MANAGER,
            pool_id_hex: format!("{pool_id}"),
            currency0_address: key.currency0,
            currency1_address: key.currency1,
            fee: key.fee,
            tick_spacing: key.tick_spacing,
            hook_address: *hook,
            zfo: leg.zero_for_one,
        })]);
        let next_memo = Arc::new(StorageMemo::new());
        let report = prefetch_paths_into_memo(
            &provider,
            NEXT,
            std::slice::from_ref(&path),
            &FamilySlotLayout::PRODUCTION,
            &facts,
            LearnedSets {
                hot: &HotSlotStore::new(),
                hooks: Some(&reopened),
            },
            &next_memo,
        );
        assert_eq!(report.skipped, None, "the prefetch batch answered");
        assert_eq!(
            report.hook,
            set.len(),
            "every learned hook slot rode the one batch"
        );

        // Quote #2: a fresh scratch at the new anchor, sharing the seeded memo.
        let second_reads_log = Arc::new(Mutex::new(Vec::new()));
        let mut second_scratch = scratch(
            &provider,
            NEXT,
            &at_next,
            Arc::clone(&next_memo),
            &warm,
            Arc::clone(&second_reads_log),
        );
        let mut second_quoter = V4BlackBoxQuoter::default().with_hook_slots(Arc::clone(&reopened));
        let second_quote = second_quoter
            .quote(&mut second_scratch, &leg, amount_in)
            .expect("the leg prices at the next anchor");
        let second_reads = reads_at(&second_reads_log, 0, *hook);
        assert_eq!(
            second_quote.hook_slots_learned, 0,
            "the learned set already held every slot the re-quote touched"
        );
        assert!(
            second_quote.hook_slots.len() >= set.len(),
            "the re-quote must actually reach the hook's storage ({}) — a hook that              short-circuited would report zero reads for the wrong reason",
            second_quote.hook_slots.len()
        );

        println!(
            "{:<12} {:<14} {:>8} {:>7} {:>11} {:>11} {:>11}",
            format!("0x{}", alloy::hex::encode(&hook.as_slice()[..3])),
            format!("0x{}", alloy::hex::encode(&pool_id[..3])),
            set.len(),
            report.hook,
            first_reads,
            second_reads,
            second_quote.hook_slots.len()
        );

        results.push(Row {
            hook: *hook,
            pool_id,
            zero_for_one: leg.zero_for_one,
            amount_in,
            learned: set.len(),
            set,
            first_reads,
            batch: report.hook,
            second_reads,
        });
    }

    // ── the assert: the second quote pays ~nothing for hook storage ──────
    assert!(
        results.len() >= 2,
        "the census must price at least two hooks ({})",
        results.len()
    );
    assert!(
        results.iter().all(|row| row.learned > 0),
        "at least one hook family must touch its own storage"
    );
    let learned_hooks = results.iter().filter(|row| row.learned > 0).count();
    assert!(learned_hooks >= 2, "the learning path must be exercised");
    for row in &results {
        assert_eq!(
            row.second_reads,
            0,
            "hook 0x{} forwarded {} hook-attributed cold read(s) on its second quote \
             (learned {} slot(s), {} batch member(s))",
            alloy::hex::encode(row.hook),
            row.second_reads,
            row.learned,
            row.batch
        );
    }

    println!();
    println!(
        "totals: {} hooks priced | first-quote hook reads {} | second-quote hook reads {} \
         | learned slots across the table {} | wall {:.1} s",
        results.len(),
        results.iter().map(|row| row.first_reads).sum::<usize>(),
        results.iter().map(|row| row.second_reads).sum::<usize>(),
        results.iter().map(|row| row.learned).sum::<usize>(),
        started.elapsed().as_secs_f64()
    );

    // ── the degrade contract: a damaged record re-learns ────────────────
    let damaged_row = &results[0];
    let record = dir.record_path(damaged_row.hook);
    assert!(record.exists(), "the first hook's record is on the disk");
    fs::write(&record, b"{ a damaged record").expect("the record is corrupted");

    let damaged =
        Arc::new(HookSlotStore::new(dir.path(), EVM2_PRODUCTION_SPEC).expect("the store reopens"));
    assert!(
        damaged.slots_for(damaged_row.hook).is_empty(),
        "a damaged record is an empty set, never an error"
    );

    let heal_reads = Arc::new(Mutex::new(Vec::new()));
    let mut heal_quoter = V4BlackBoxQuoter::default().with_hook_slots(Arc::clone(&damaged));
    let mut heal_scratch = scratch(
        &provider,
        PIN,
        &at_pin,
        Arc::new(StorageMemo::new()),
        &warm,
        Arc::clone(&heal_reads),
    );
    let leg = V4Leg::canonical(
        chosen
            .iter()
            .find(|(hook, _)| *hook == damaged_row.hook)
            .map(|(_, key)| key.clone())
            .expect("the damaged row's pool is in the census selection"),
        damaged_row.zero_for_one,
    );
    let rehealed = heal_quoter
        .quote(&mut heal_scratch, &leg, damaged_row.amount_in)
        .expect("a damaged store never fails a quote");
    assert_eq!(
        damaged.slots_for(damaged_row.hook),
        damaged_row.set,
        "the re-learned set is identical to the one the damage destroyed"
    );
    assert_eq!(
        rehealed.hook_slots.len(),
        damaged_row.set.len(),
        "the re-quote touched the same slots"
    );
    println!(
        "degrade: hook 0x{} re-learned {} slot(s) after its record was corrupted \
         (pool 0x{}, {} cold read(s) paid once)",
        alloy::hex::encode(damaged_row.hook),
        damaged_row.set.len(),
        alloy::hex::encode(&damaged_row.pool_id[..3]),
        reads_at(&heal_reads, 0, damaged_row.hook)
    );
}
