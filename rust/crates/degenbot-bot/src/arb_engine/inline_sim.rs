//! The inline-sim hook seam (SIMPIPE2 T1, epic WMMABS).
//!
//! ADR-019 D7 keeps the engine free of strategy-simulation types — so the
//! inline simulation (SIMPIPE: run the per-path EVM sim inside the Rust
//! solver sandwich instead of the post-batch Python FFI round-trip) is
//! expressed as a **dependency inversion**: this crate defines the
//! [`InlineSimulator`] trait + the primitive payload struct
//! ([`SimulatedPathResult`]) and the engine exposes the construction-time
//! setter; `degenbot-python` installs the real implementation (a closure
//! over the `AlloyProvider` + session sim config + warm-code cache) at
//! engine construction, exactly mirroring the `set_result_channel`
//! plumbing.
//!
//! Every type here is PRIMITIVE (alloy `Address`/`U256`/`I256`/`u64`/
//! `u128`, `Vec<u8>` calldata, plain rows): no `degenbot-arbitrage` or
//! `degenbot-simulation` type appears in this module, and the bot crate
//! gains no dependency on either.
//!
//! ## Parity with the FFI contract
//!
//! [`SimulatedPathResult`] carries, field-for-field, everything
//! `degenbot_arbitrage::SimResult` exposes through
//! `degenbot_python::simulation::dispatch::join_sim_result` →
//! `SubmitCandidate`:
//!
//! | SimResult / SubmitCandidate | SimulatedPathResult |
//! |---|---|
//! | `path_id` | `path_id` |
//! | `gross_profit` | `gross_profit` |
//! | `net_profit` | `net_profit` |
//! | `gas_used` (UN-inflated; 1.5× at submit) | `gas_used` (same contract) |
//! | `priority_fee` | `priority_fee` |
//! | `base_fee_next` | `base_fee_next` |
//! | `execute_calldata` (`alloy Bytes`) | `execute_calldata: Vec<u8>` |
//! | `access_list` (`Option<AccessList>`) | `access_list: Option<Vec<AccessListRow>>` |
//! | `captured_swaps` (`Vec<CapturedSwap>`) | `captured_swaps: Vec<CapturedSwapRow>` |
//! | `hop_count` | `hop_count` |
//! | — (dispatch failures only) | `failure: Option<InlineSimFailure>` |

use crate::arb_engine::ArbitrageEngine;
use crate::arb_engine::BlockMetadata;
use alloy::primitives::{Address, I256, U256};
use degenbot_solvers::mixed::{MixedPoolRef, SolvePathResult};

/// The engine → simulator request for ONE clamp-admitted path.
///
/// Primitive by design: `hops` are the to_solve-aligned
/// [`MixedPoolRef`]s straight from the engine's immutable
/// `path_pools` snapshot (`family` + `BotState` pool id + direction), the
/// amounts are the clamp-committed wei. The installed implementation
/// resolves pool identity (addresses/fees/hooks) from its own registry —
/// the same tables `PyBot::register_v*_pool` populated — so the engine
/// never handles pool-construction types it doesn't already own.
#[derive(Clone, Debug)]
pub struct InlineSimRequest {
    /// The path id (the engine's registered path identity).
    pub path_id: u64,
    /// One ref per hop, in path order (the clamp-admitted alignment).
    pub hops: Vec<MixedPoolRef>,
    /// The clamp-committed optimal input (wei) for hop 0.
    pub optimal_input: U256,
    /// The clamp-committed per-hop consumed inputs (wei), path order.
    pub consumed_inputs: Vec<U256>,
    /// The solver's per-hop outputs (wei), path order — the step outputs the
    /// encoder must feed forward (T4: the sim's `SolveStep` rows).
    pub hop_outputs: Vec<U256>,
    /// The per-hop solve-time state nonces (AV42C7 staleness parity).
    pub state_nonces: Vec<u64>,
    // ---- The block env (the sim's `SimulateContext` primitives) ----
    /// The block to simulate against (the cycle's solve block — the
    /// head-anchored promoted block, MQIZ5M).
    pub sim_block: u64,
    /// The block timestamp (the pump's header; XPPMQG — the default `1`
    /// timestamp forks Solidity-0.8 pair updates).
    pub block_timestamp: u64,
    /// The parent block's base fee + gas used/limit — the EIP-1559
    /// `next_base_fee` inputs (the worker's `BlockMetadata`). `base_fee
    /// None` (pre-EIP-1559) rides as `parent_base_fee = 0`.
    pub parent_base_fee: u64,
    pub parent_gas_used: u64,
    pub parent_gas_limit: u64,
}

/// One EIP-2930 access-list row (primitive mirror of `AccessListItem`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessListRow {
    /// The touched contract address.
    pub address: Address,
    /// The touched storage keys, in access order.
    pub storage_keys: Vec<U256>,
}

/// The swap-family tag of a captured swap, as a local primitive enum (the
/// `CapturedSwap` original lives in `degenbot-simulation` and is forbidden
/// here by ADR-019 D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineSwapFamily {
    V2,
    V3,
    V4,
}

/// Primitive mirror of `degenbot_simulation::CapturedSwap` — the swap events
/// the sim inspector decoded. See the original doc for field semantics.
#[derive(Clone, Debug, PartialEq)]
pub struct CapturedSwapRow {
    /// The pool/PoolManager contract that emitted the event.
    pub emitter: Address,
    /// Which Uniswap family emitted the event.
    pub family: InlineSwapFamily,
    /// Amount of token0/currency0 (signed — negative for exact-in).
    pub amount0: I256,
    /// Amount of token1/currency1 (signed — negative for exact-in).
    pub amount1: I256,
    /// Post-swap `sqrtPriceX96` (V3/V4) or `U256::ZERO` (V2 `Sync`).
    pub sqrt_price_x96: U256,
    /// Post-swap active liquidity (V3/V4) or `U256::ZERO` (V2 `Sync`).
    pub liquidity: U256,
    /// Post-swap tick (V3/V4) or `0` (V2 `Sync`).
    pub tick: i32,
}

/// A failed inline simulation, rendered for the `[sim-fail]` rows (T3's
/// render contract). `fail_index` is `Some(call idx)` when one specific
/// simulated call reverted; `revert_data` is the raw revert payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineSimFailure {
    /// The index of the failing call in the simulated call vector, when
    /// attributable to one call.
    pub fail_index: Option<usize>,
    /// The raw revert data bytes (empty for orchestration-only failures).
    pub revert_data: Vec<u8>,
    /// The bucket label for the `[sim] by reason` breakdown + the
    /// `[sim-fail]` render (the `fail_buckets` key parity).
    pub bucket: String,
}

/// The inline simulation result — THE primitive payload (module doc has the
/// field-parity table against `SimResult`/`SubmitCandidate`). `None` from
/// [`InlineSimulator::simulate_path`] = the path failed simulation without a
/// payload worth carrying (the entry is treated as sim-failed).
#[derive(Clone, Debug, PartialEq)]
pub struct SimulatedPathResult {
    /// The path id this payload belongs to.
    pub path_id: u64,
    /// Gross profit wei (C2 accounting — WETH + ETH + ERC-6909 deltas).
    pub gross_profit: U256,
    /// Net profit wei = `gross − gas·(base_fee_next + priority_fee)`.
    pub net_profit: U256,
    /// The simulate's `gasUsed` for `execute()`, UN-inflated (the 1.5×
    /// safety margin is applied at submit time — the `SimResult` contract).
    pub gas_used: u64,
    /// The market-aware priority fee.
    pub priority_fee: u128,
    /// The next block's base fee.
    pub base_fee_next: u128,
    /// The `execute()` calldata (selector + ABI-wrapped `(bytes, uint256)`).
    pub execute_calldata: Vec<u8>,
    /// The EIP-2930 access list for `execute()`, if any.
    pub access_list: Option<Vec<AccessListRow>>,
    /// The captured swap events (per-hop ground truth, inspector-decoded).
    pub captured_swaps: Vec<CapturedSwapRow>,
    /// The hop count (the caller's `path_info` reshape input).
    pub hop_count: usize,
    /// `Some` = the path failed simulation (`[sim-fail]` render through the
    /// payload; T3). A failed payload carries no useful profit/gas values.
    pub failure: Option<InlineSimFailure>,
}

/// A scheduled-but-maybe-unfinished inline sim (7LV6VN T5). The solve
/// worker schedules the eager EVM simulation and keeps walking paths; the
/// receipt is polled non-blockingly (delivery as soon as each sim lands)
/// and joined at bin end. The in-flight slot (budget-derived pacing cap)
/// moves into the driver thread and releases when the sim finishes.
#[must_use]
pub struct PendingSim {
    rx: std::sync::mpsc::Receiver<Option<SimulatedPathResult>>,
}

/// Poll verdict for a scheduled sim (the `Box` keeps variants balanced;
/// the inner `Option` is the payload contract — `None` = sim-failed).
#[derive(Debug)]
pub(crate) enum SimPoll {
    /// The sim finished.
    Ready(Option<Box<SimulatedPathResult>>),
    /// Still in flight.
    InFlight,
}

impl PendingSim {
    pub(crate) fn new(rx: std::sync::mpsc::Receiver<Option<SimulatedPathResult>>) -> Self {
        Self { rx }
    }

    /// Non-blocking poll (the `Box` flattens back to the payload on join).
    pub(crate) fn try_result(&self) -> SimPoll {
        match self.rx.try_recv() {
            Ok(payload) => SimPoll::Ready(payload.map(Box::new)),
            Err(_) => SimPoll::InFlight,
        }
    }

    /// Blocking join (bin-tail collection).
    pub(crate) fn result(self) -> Option<SimulatedPathResult> {
        self.rx.recv().ok().flatten()
    }
}

/// The hook the outer driver installs (ADR-019 D7 dependency inversion).
/// `Send + Sync + 'static` so the engine can hold it as
/// `Arc<dyn InlineSimulator>` and call it from any solve/merge context.
pub trait InlineSimulator: Send + Sync + 'static {
    /// Simulate ONE clamp-admitted path. `None` = failed without a payload
    /// (the entry's batch payload slot stays empty — T3's map decides the
    /// legacy FFI sim path per entry).
    fn simulate_path(&self, request: InlineSimRequest) -> Option<SimulatedPathResult>;

    /// Eagerly START the sim for ONE clamp-admitted path and return a
    /// receipt the worker polls/joins later (7LV6VN T5 pipelining). The
    /// default runs the synchronous [`InlineSimulator::simulate_path`]
    /// immediately and hands the finished value back through the receipt
    /// (stubs and sync-only hooks stay correct without threads); the
    /// production hook starts the sim on the dedicated runtime and only
    /// blocks when the receipt is collected.
    fn simulate_path_pipelined(
        &self,
        request: InlineSimRequest,
    ) -> std::sync::mpsc::Receiver<Option<SimulatedPathResult>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(self.simulate_path(request));
        rx
    }
}

impl ArbitrageEngine {
    /// Install the inline-sim hook (engine construction/wiring, from the
    /// outer driver; mirrors `set_result_channel`).
    pub fn set_inline_simulator(&mut self, sim: std::sync::Arc<dyn InlineSimulator>) {
        self.inline_sim = Some(std::sync::Arc::clone(&sim));
        // ADR-045 T4: the cycle owns its own handle for the solve dispatch.
        self.cycle.inline_sim = Some(sim);
    }

    /// Simulate one clamp-admitted path through the installed hook. `None` =
    /// no hook installed, the path is unknown, or the hook reported failure-
    /// without-payload. `metadata` supplies the sim's block env (the same
    /// fields the worker path fills from its cycle snapshot).
    #[must_use]
    pub fn inline_simulate(
        &self,
        path_id: u64,
        clamp_admitted: &SolvePathResult,
        metadata: &BlockMetadata,
    ) -> Option<SimulatedPathResult> {
        let sim = self.inline_sim.as_ref()?;
        let hops = self.registry.get(path_id)?.pools.clone();
        let request = InlineSimRequest {
            path_id,
            hops,
            optimal_input: clamp_admitted.optimal_input,
            consumed_inputs: clamp_admitted.consumed_inputs.clone(),
            hop_outputs: clamp_admitted.hop_outputs.clone(),
            state_nonces: clamp_admitted.state_nonces.clone(),
            sim_block: self.cycle.cursor.results_block(),
            block_timestamp: metadata.timestamp,
            parent_base_fee: metadata.base_fee_per_gas.unwrap_or(0),
            parent_gas_used: metadata.gas_used,
            parent_gas_limit: metadata.gas_limit,
        };
        sim.simulate_path(request)
    }
}

#[cfg(test)]
mod inline_sim_tests {
    use super::*;
    use crate::bot_core::RegisterV4PoolParams;
    use alloy::primitives::U256;
    use degenbot_solvers::mixed::{HopType, PoolHop};
    use hashbrown::HashMap;
    use parking_lot::Mutex;
    use std::sync::Arc;

    struct RecordingSim {
        requests: Mutex<Vec<InlineSimRequest>>,
        payload: SimulatedPathResult,
    }

    impl InlineSimulator for RecordingSim {
        fn simulate_path(&self, request: InlineSimRequest) -> Option<SimulatedPathResult> {
            self.requests.lock().push(request);
            let mut payload = self.payload.clone();
            payload.path_id = 42; // echo the engine's identity on return
            Some(payload)
        }
    }

    #[expect(clippy::unwrap_used)]
    fn stub_payload() -> SimulatedPathResult {
        SimulatedPathResult {
            path_id: 0,
            gross_profit: U256::from(1_000u64),
            net_profit: U256::from(900u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: vec![0xab, 0x58, 0x98, 0xe8, 0x1, 0x2, 0x3],
            access_list: Some(vec![AccessListRow {
                address: Address::from([0x7au8; 20]),
                storage_keys: vec![U256::from(7u64)],
            }]),
            captured_swaps: vec![CapturedSwapRow {
                emitter: Address::from([0x11u8; 20]),
                family: InlineSwapFamily::V2,
                amount0: I256::try_from(-5_000_000_000i64).unwrap(),
                amount1: I256::try_from(4_900_000_000i64).unwrap(),
                sqrt_price_x96: U256::ZERO,
                liquidity: U256::ZERO,
                tick: 0,
            }],
            hop_count: 2,
            failure: None,
        }
    }

    #[expect(clippy::expect_used)]
    fn two_hop_engine() -> (ArbitrageEngine, u64) {
        use crate::arb_engine::PoolTickCoverage;
        use crate::bot_core::TickInfo;
        let mut engine = ArbitrageEngine::new();
        let to_u112 = |v: u64| {
            (U256::from(v) * U256::from(10u64).pow(U256::from(18)))
                .to::<alloy::primitives::Uint<112, 2>>()
        };
        let v2 = engine.register_v2_pool(
            Address::from([0x11u8; 20]),
            to_u112(1_500),
            to_u112(20_000_000),
            997,
            1000,
        );
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: 150i128,
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: -100i128,
                block: 0,
            },
        );
        let v4_id = engine
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: Address::from([0x44u8; 20]),
                pool_id: [0xabu8; 32],
                pool_key: crate::bot_core::V4PoolKey {
                    currency0: Address::from([0x30u8; 20]),
                    currency1: Address::from([0x31u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
            })
            .expect("V4 registration failed");
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v4_id,
                    zero_for_one: false,
                },
            ])
            .expect("path registers");
        (engine, path_id)
    }

    fn admitted() -> SolvePathResult {
        SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64),
            profit: U256::from(1_000u64),
            hop_outputs: vec![U256::from(900_000_000u64), U256::from(1_001_000_000_000u64)],
            consumed_inputs: vec![U256::from(1_000_000_000u64), U256::from(900_000_000u64)],
            state_nonces: vec![0, 0],
            solver_pool_states: Vec::new(),
        }
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn engine_calls_hook_with_aligned_primitive_request() {
        let (mut engine, path_id) = two_hop_engine();
        let sim = Arc::new(RecordingSim {
            requests: Mutex::new(Vec::new()),
            payload: stub_payload(),
        });
        engine.set_inline_simulator(sim.clone());
        let got = engine
            .inline_simulate(path_id, &admitted(), &BlockMetadata::default())
            .expect("hook returns the payload");

        // The request the engine built: hops in path order, engine-native
        // primitives only.
        let reqs = sim.requests.lock();
        assert_eq!(reqs.len(), 1, "one request per admitted path");
        let req = &reqs[0];
        assert_eq!(req.path_id, path_id);
        assert_eq!(req.hops.len(), 2, "one ref per hop");
        assert_eq!(req.hops[0].hop_type, HopType::V2);
        assert!(req.hops[0].zero_for_one);
        assert_eq!(req.hops[1].hop_type, HopType::V4);
        assert!(!req.hops[1].zero_for_one);
        assert_eq!(
            req.optimal_input,
            U256::from(1_000_000_000u64),
            "clamp-committed optimal input passed through"
        );
        assert_eq!(req.consumed_inputs.len(), 2, "per-hop consumed inputs");

        // The payload round-trips untouched (field parity — module doc table).
        assert_eq!(got.execute_calldata, stub_payload().execute_calldata);
        assert_eq!(got.gross_profit, U256::from(1_000u64));
        assert_eq!(got.net_profit, U256::from(900u64));
        assert_eq!(got.gas_used, 300_000);
        assert_eq!(got.access_list.as_ref().map(Vec::len), Some(1));
        assert_eq!(got.captured_swaps.len(), 1);
        assert_eq!(got.hop_count, 2);
        assert!(got.failure.is_none());
    }

    #[test]
    fn no_hook_or_unknown_path_is_none() {
        let (engine, path_id) = two_hop_engine();
        assert!(
            engine
                .inline_simulate(path_id, &admitted(), &BlockMetadata::default())
                .is_none(),
            "no hook installed → None"
        );
        let mut engine2 = engine;
        engine2.set_inline_simulator(Arc::new(RecordingSim {
            requests: Mutex::new(Vec::new()),
            payload: stub_payload(),
        }));
        assert!(
            engine2
                .inline_simulate(99_999, &admitted(), &BlockMetadata::default())
                .is_none(),
            "unknown path → None (no request built)"
        );
    }
}
