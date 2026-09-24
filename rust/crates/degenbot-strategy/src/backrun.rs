//! The backrun arm's decision layer and typed knobs.
//!
//! The arm owns the full edge — `MEVBlocker` feed -> hub classification ->
//! exact-sim oracle gate -> budgeted bid via the submission leaf — and runs
//! as the hosted `BackrunDriver` on the one-process `StrategyHost`. The
//! DECISION layer lives here as a pure function ([`decide`]) so the safety
//! invariants are testable without a network.
//!
//! Safety model (task acceptance):
//! - observe-only default: no bid unless `bid_mode` is set AND the budget is
//!   non-zero (bid mode gated behind an explicit flag AND a non-zero budget);
//! - no bid without a passing sim (the oracle gate is upstream of `decide`);
//! - hard per-bundle + cumulative budget caps;
//! - STOP kill-switch file: existence halts all bidding, then the loop;
//! - a target whose tx already mined never dispatches (the bid-path receipt
//!   probe observes `already_settled`).

use std::path::PathBuf;

use alloy::primitives::U256;
use degenbot_bot::bot_core::pool_ingress::VerifyLevel;
use degenbot_config::BotConfig;
use degenbot_decoders::target_class::TargetClass;

use crate::strategy_plane::{Strategy, StrategyName};

/// One ecosystem's submission slot: the half of the strategy composition the
/// two backrun arms differ in. The reaction machinery (frame feed, anchored
/// discovery, decide gate, sim, dispatch) is shared; only the values named
/// here vary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionSlot {
    /// The `MEVBlocker` bundle auction. The signed backrun is bid through
    /// `eth_sendBundle` on the searcher WebSocket, and the raw-broadcast
    /// fan-out leads with the private endpoint (when set) before the read
    /// provider fallback.
    Mevblocker {
        /// The searcher WebSocket the bundle is anchored on.
        bundle_url: String,
        /// Private-broadcast RPC. `None` leaves the raw fan-out to the read
        /// provider alone; the bundle arm is unaffected either way.
        private_url: Option<String>,
    },
    /// The public-mempool composition: no auction, the signed backrun goes raw
    /// across the public relay fan-out with the read provider as fallback.
    PublicFanOut {
        /// The public relay URLs, in fan-out order.
        relays: Vec<String>,
    },
}

impl SubmissionSlot {
    /// The ordered raw-broadcast relay URLs this slot names, private-first for
    /// the `MEVBlocker` slot. Empty means the submit leaf falls back to the read
    /// provider alone.
    #[must_use]
    pub fn raw_relay_urls(&self) -> Vec<String> {
        match self {
            Self::Mevblocker {
                private_url: Some(url),
                ..
            } => vec![url.clone()],
            Self::Mevblocker {
                private_url: None, ..
            } => Vec::new(),
            Self::PublicFanOut { relays } => relays.clone(),
        }
    }

    /// Whether this slot names a private (non-public) broadcast endpoint.
    #[must_use]
    pub const fn names_private_endpoint(&self) -> bool {
        matches!(
            self,
            Self::Mevblocker {
                private_url: Some(_),
                ..
            }
        )
    }
}

/// The backrun driver's typed config, built from one per-ecosystem facet plus
/// the chain-node join its boot resolves.
///
/// The key never leaves the signer — only `key_file` is named here.
#[derive(Debug, Clone)]
pub struct BackrunConfig {
    /// The shared pending-transaction source: the `MEVBlocker` searcher feed.
    pub feed_url: String,
    pub rpc_url: String,
    pub key_file: Option<PathBuf>,
    /// Explicit bid-mode flag. Off = observe-only.
    pub bid_mode: bool,
    /// Cumulative bid budget cap in wei; bid mode REQUIRES non-zero.
    pub budget_wei: U256,
    /// Hard per-submission cap in wei.
    pub max_bundle_wei: U256,
    /// Kill-switch path: if the file exists, bidding halts (then the loop).
    pub stop_file: PathBuf,
    /// Sign-nothing dispatch (`strategy.*.dry_run`).
    pub dry_run: bool,
    /// The builder's bribe share in bips, clamped to the `10_000` ceiling.
    pub bribe_bips: u16,
    /// Composed-bundle gas estimate priced into the net-of-gas bid gate.
    pub bundle_gas_est: u64,
    /// The envelope gate's profit floor (wei): a declared chain whose solver
    /// bound tops out below this skips without a simulation.
    pub gas_floor_wei: u64,
    /// Chain-sample verification policy for ingress V3 tick-map admission.
    pub verify_ticks: VerifyLevel,
    /// Max Db→head lag (blocks) the ingress closes by backfilling a pool's
    /// Mint/Burn events before deferring to the sparse Chain arm.
    pub ingress_backfill_max_blocks: u64,
    /// The operator's priority fee in gwei.
    pub priority_fee_gwei: u64,
    /// Bundle-sim endpoint; unset reuses the chain node.
    pub sim_url: Option<String>,
    /// Live deep-pair ranking sanity-probe gate.
    pub rank_evidence: bool,
    /// Discovery fan-out cap (cycles per frame).
    pub connectors: usize,
    /// Hop-depth cap per discovered cycle (the WETH pin plus up to
    /// `cycle_max_hops - 1` connectors).
    pub cycle_max_hops: usize,
    /// Offline dry-run's pinned head block.
    pub fixture_head: Option<u64>,
    /// Executor contract address (validated at the driver boot).
    pub executor: String,
    /// Executor owner / sim caller; unset falls back to
    /// `EXECUTOR_OWNER_ADDRESS`, then the built-in default.
    pub operator: Option<String>,
    /// Dry-run fixture frames path (`logging.dry_run_jsonl`).
    pub dry_run_jsonl: Option<PathBuf>,
    /// The submission slot this composition binds.
    pub submission: SubmissionSlot,
}

/// The knob set shared by both per-ecosystem facets. The two facet types are
/// distinct, so this is the one place their common fields collapse.
struct BackrunKnobs {
    active_endpoints: Option<String>,
    key_file: Option<PathBuf>,
    bid_mode: bool,
    budget_wei: u128,
    max_bundle_wei: u128,
    stop_file: PathBuf,
    dry_run: bool,
    bribe_bips: u64,
    bundle_gas_est: u64,
    gas_floor_wei: u64,
    verify_ticks: VerifyLevel,
    ingress_backfill_max_blocks: u64,
    priority_fee_gwei: u64,
    sim_url: Option<String>,
    rank_evidence: bool,
    connectors: usize,
    cycle_max_hops: usize,
    fixture_head: Option<u64>,
    executor: String,
    operator: Option<String>,
}

impl BackrunKnobs {
    fn into_config(
        self,
        cfg: &BotConfig,
        rpc_url: String,
        submission: SubmissionSlot,
    ) -> BackrunConfig {
        BackrunConfig {
            // The source slot is shared: both backrun arms consume the
            // MEVBlocker searcher pending-tx feed.
            feed_url: degenbot_config::DEFAULT_BACKRUN_STREAM_URL.to_string(),
            rpc_url,
            key_file: self.key_file,
            bid_mode: self.bid_mode,
            budget_wei: U256::from(self.budget_wei),
            max_bundle_wei: U256::from(self.max_bundle_wei),
            stop_file: self.stop_file,
            dry_run: self.dry_run,
            bribe_bips: u16::try_from(self.bribe_bips.min(10_000)).unwrap_or(10_000),
            bundle_gas_est: self.bundle_gas_est,
            gas_floor_wei: self.gas_floor_wei,
            verify_ticks: self.verify_ticks,
            ingress_backfill_max_blocks: self.ingress_backfill_max_blocks,
            priority_fee_gwei: self.priority_fee_gwei,
            sim_url: self.sim_url,
            rank_evidence: self.rank_evidence,
            connectors: self.connectors,
            cycle_max_hops: self.cycle_max_hops,
            fixture_head: self.fixture_head,
            executor: self.executor,
            operator: self.operator,
            dry_run_jsonl: cfg.logging.dry_run_jsonl.clone(),
            submission,
        }
    }
}

impl From<&degenbot_config::StrategyMevblockerBackrunConfig> for BackrunKnobs {
    fn from(f: &degenbot_config::StrategyMevblockerBackrunConfig) -> Self {
        Self {
            active_endpoints: f.endpoints.clone(),
            key_file: f.key_file.clone(),
            bid_mode: f.bid_mode,
            budget_wei: f.budget_wei,
            max_bundle_wei: f.max_bundle_wei,
            stop_file: f.stop_file.clone(),
            dry_run: f.dry_run,
            bribe_bips: f.bribe_bips,
            bundle_gas_est: f.bundle_gas_est,
            gas_floor_wei: f.gas_floor_wei,
            verify_ticks: to_verify_level(f.verify_ticks),
            ingress_backfill_max_blocks: f.ingress_backfill_max_blocks,
            priority_fee_gwei: f.priority_fee_gwei,
            sim_url: f.sim_url.clone(),
            rank_evidence: f.rank_evidence,
            connectors: f.connectors,
            cycle_max_hops: f.cycle_max_hops,
            fixture_head: f.fixture_head,
            executor: f.executor.clone(),
            operator: f.operator.clone(),
        }
    }
}

/// Project the config facet's `verify_ticks` enum onto the ingress policy.
fn to_verify_level(v: degenbot_config::VerifyTicks) -> VerifyLevel {
    match v {
        degenbot_config::VerifyTicks::Strict => VerifyLevel::Strict,
        degenbot_config::VerifyTicks::Bootstrap => VerifyLevel::Bootstrap,
        degenbot_config::VerifyTicks::Off => VerifyLevel::Off,
    }
}

impl From<&degenbot_config::StrategyPeerBackrunConfig> for BackrunKnobs {
    fn from(f: &degenbot_config::StrategyPeerBackrunConfig) -> Self {
        Self {
            active_endpoints: f.endpoints.clone(),
            key_file: f.key_file.clone(),
            bid_mode: f.bid_mode,
            budget_wei: f.budget_wei,
            max_bundle_wei: f.max_bundle_wei,
            stop_file: f.stop_file.clone(),
            dry_run: f.dry_run,
            bribe_bips: f.bribe_bips,
            bundle_gas_est: f.bundle_gas_est,
            gas_floor_wei: f.gas_floor_wei,
            verify_ticks: to_verify_level(f.verify_ticks),
            ingress_backfill_max_blocks: f.ingress_backfill_max_blocks,
            priority_fee_gwei: f.priority_fee_gwei,
            sim_url: f.sim_url.clone(),
            rank_evidence: f.rank_evidence,
            connectors: f.connectors,
            cycle_max_hops: f.cycle_max_hops,
            fixture_head: f.fixture_head,
            executor: f.executor.clone(),
            operator: f.operator.clone(),
        }
    }
}

impl BackrunKnobs {
    /// The `MEVBlocker` submission slot: the bundle bound to the facet's
    /// searcher WebSocket (or the pinned default) plus the private endpoint.
    fn mevblocker_slot(
        &self,
        f: &degenbot_config::StrategyMevblockerBackrunConfig,
    ) -> SubmissionSlot {
        SubmissionSlot::Mevblocker {
            bundle_url: resolve_arm_endpoint(
                "mevblocker_backrun",
                self.active_endpoints.as_deref(),
                degenbot_config::DEFAULT_BACKRUN_STREAM_URL,
            ),
            private_url: f.mevblocker_url.clone(),
        }
    }

    /// The peer submission slot: the facet's public relay fan-out (or the
    /// pinned default) with the read provider as fallback.
    fn peer_slot(&self) -> SubmissionSlot {
        SubmissionSlot::PublicFanOut {
            relays: resolve_arm_endpoints(
                "peer_backrun",
                self.active_endpoints.as_deref(),
                degenbot_config::DEFAULT_PEER_BACKRUN_RELAYS,
            ),
        }
    }
}

/// Resolve one activated facet's first endpoint, falling back to a pinned
/// default. The boots refuse an unsettled active facet before this runs, so
/// the fallback only serves an inactive or fixture construction.
fn resolve_arm_endpoint(facet: &'static str, endpoints: Option<&str>, default: &str) -> String {
    split_arm_endpoints(facet, endpoints, std::slice::from_ref(&default))
        .into_iter()
        .next()
        .unwrap_or_else(|| default.to_string())
}

/// Resolve an activated facet's endpoint list, falling back to a pinned
/// default set.
fn resolve_arm_endpoints(
    facet: &'static str,
    endpoints: Option<&str>,
    default: &[&str],
) -> Vec<String> {
    split_arm_endpoints(facet, endpoints, default)
}

fn split_arm_endpoints(
    _facet: &'static str,
    endpoints: Option<&str>,
    default: &[&str],
) -> Vec<String> {
    let urls: Vec<String> = endpoints
        .into_iter()
        .flat_map(|raw| raw.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if urls.is_empty() {
        default.iter().map(|url| (*url).to_string()).collect()
    } else {
        urls
    }
}

/// The MEVBlocker-ecosystem backrun strategy: the shared pending-transaction
/// reaction composed over per-ecosystem facets.
///
/// | Slot | Binding |
/// |---|---|
/// | **source** | the MEVBlocker searcher pending-tx feed |
/// | **infrastructure** | the connector-index route registry |
/// | **calculation** | the anchored-dfs WETH-closing solver |
/// | **encoder** | the composed executor calldata |
/// | **simulator** | the frame replay + `eth_callMany` oracle gate |
/// | **submission** | the MEVBlocker searcher bundle, private broadcast first |
#[derive(Debug, Clone)]
pub struct MevblockerBackrun {
    config: BackrunConfig,
}

impl MevblockerBackrun {
    /// Build the composition from the loaded config's
    /// `strategy.mevblocker_backrun` facet plus the chain-node join.
    #[must_use]
    pub fn from_config(cfg: &BotConfig, rpc_url: String) -> Self {
        let facet = &cfg.strategy.mevblocker_backrun;
        let knobs = BackrunKnobs::from(facet);
        let submission = knobs.mevblocker_slot(facet);
        Self {
            config: knobs.into_config(cfg, rpc_url, submission),
        }
    }

    /// The composed driver config.
    #[must_use]
    pub fn config(&self) -> &BackrunConfig {
        &self.config
    }

    /// Consume the composition into the driver config.
    #[must_use]
    pub fn into_config(self) -> BackrunConfig {
        self.config
    }
}

impl Strategy for MevblockerBackrun {
    const NAME: StrategyName = StrategyName::MevblockerBackrun;
}

/// The public-mempool backrun strategy: the same pending-transaction reaction,
/// submitting through the public relay fan-out instead of the `MEVBlocker`
/// auction.
///
/// | Slot | Binding |
/// |---|---|
/// | **source** | the MEVBlocker searcher pending-tx feed |
/// | **infrastructure** | the connector-index route registry |
/// | **calculation** | the anchored-dfs WETH-closing solver |
/// | **encoder** | the composed executor calldata |
/// | **simulator** | the frame replay + `eth_callMany` oracle gate |
/// | **submission** | the public relay fan-out, read-provider fallback |
#[derive(Debug, Clone)]
pub struct PeerBackrun {
    config: BackrunConfig,
}

impl PeerBackrun {
    /// Build the composition from the loaded config's `strategy.peer_backrun`
    /// facet plus the chain-node join.
    #[must_use]
    pub fn from_config(cfg: &BotConfig, rpc_url: String) -> Self {
        let facet = &cfg.strategy.peer_backrun;
        let knobs = BackrunKnobs::from(facet);
        let submission = knobs.peer_slot();
        Self {
            config: knobs.into_config(cfg, rpc_url, submission),
        }
    }

    /// The composed driver config.
    #[must_use]
    pub fn config(&self) -> &BackrunConfig {
        &self.config
    }

    /// Consume the composition into the driver config.
    #[must_use]
    pub fn into_config(self) -> BackrunConfig {
        self.config
    }
}

impl Strategy for PeerBackrun {
    const NAME: StrategyName = StrategyName::PeerBackrun;
}

impl BackrunConfig {
    /// The bid-mode legality gate: explicit flag AND non-zero budget.
    #[must_use]
    pub fn bid_mode_legal(&self) -> bool {
        self.bid_mode && !self.budget_wei.is_zero()
    }
}

/// Why a candidate was (or was not) bid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// All gates passed; bid the given wei.
    Bid { bid_wei: U256 },
    /// Default mode: journal/publish only, never bid.
    Observe { reason: &'static str },
    /// Not a bid candidate at all (inert/too old).
    Drop { reason: &'static str },
}

/// The pure decision function - every gate the acceptance criteria name.
///
/// - No bid without a sim pass (`sim_ok` - enforced upstream too, but the
///   decision layer re-checks so a wiring bug cannot bypass the gate).
/// - Observe-only default (`bid_mode` off -> `Observe`).
/// - Kill-switch file present -> bid *and* loop must halt ([`Halt::KillSwitch`]).
/// - Cumulative budget caps: spent + binder > budget -> Observe.
/// - Per-bundle cap: bundle > `max_bundle_wei` -> Observe.
#[must_use]
pub fn decide(
    cfg: &BackrunConfig,
    stop_file_exists: bool,
    target_class: &TargetClass,
    sim_ok: bool,
    requested_bid_wei: U256,
    spent_wei: U256,
) -> Decision {
    if stop_file_exists {
        return Decision::Drop {
            reason: "kill_switch",
        };
    }
    match target_class {
        // Inert targets never bid; decodable/opaque hubs are the candidate
        // surface and the sim gate is the truth bar for both.
        TargetClass::Inert => Decision::Drop {
            reason: "inert_target",
        },
        TargetClass::Swap(_) | TargetClass::Opaque(_) => {
            if !sim_ok {
                return Decision::Observe {
                    reason: "sim_gate_failed",
                };
            }
            if !cfg.bid_mode_legal() {
                return Decision::Observe {
                    reason: "observe_only",
                };
            }
            if requested_bid_wei.is_zero() {
                // A zero requested bid means NO composed candidate stood
                // behind this frame -- the only thing that could ever have
                // been submitted is the executor sweep probe, which pays the
                // executor's balance straight to the builder (observed live:
                // tx 0x723c25.. on block 25995965). A Bid must carry a
                // positive composed profit share.
                return Decision::Observe { reason: "zero_bid" };
            }
            let bind = requested_bid_wei.min(cfg.max_bundle_wei);
            if bind > cfg.budget_wei - spent_wei {
                return Decision::Observe {
                    reason: "budget_exhausted",
                };
            }
            Decision::Bid { bid_wei: bind }
        }
    }
}

/// The bid-path liveness gate: a target whose tx already mined cannot be
/// backrun, so its dispatch is refused with the truthful observe reason. The
/// receipt probe runs on the bid path (after `decide`, before dispatch); this
/// pure gate turns its evidence into the final decision. A probe failure
/// carries no positive evidence, so it must not kill the bid -- the `MEVBlocker`
/// bundle's block anchoring is the final backstop.
#[must_use]
pub fn gate_mined_target(decision: Decision, target_receipt_found: bool) -> Decision {
    match decision {
        Decision::Bid { .. } if target_receipt_found => Decision::Observe {
            reason: "already_settled",
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use degenbot_decoders::target_class::{PoolProtocol, SwapLeg};

    /// The facet knob reaches the driver config: the envelope-gate floor is
    /// operator-tunable (the compiled-in 5e13 wei stance is retired), and the
    /// declared default is 1 wei (solve-anything, gate at bid time).
    #[test]
    fn gas_floor_knob_flows_from_the_facet() {
        let c = MevblockerBackrun::from_config(&BotConfig::default(), String::new()).into_config();
        assert_eq!(c.gas_floor_wei, 1, "declared facet default");
        let mut cfg = BotConfig::default();
        cfg.strategy.mevblocker_backrun.gas_floor_wei = 123_456;
        let c = MevblockerBackrun::from_config(&cfg, String::new()).into_config();
        assert_eq!(c.gas_floor_wei, 123_456, "the knob is wired, not ignored");
    }

    /// The chain-sample policy reaches the driver config and maps every facet
    /// value onto the ingress policy; the default is `Bootstrap`.
    #[test]
    fn verify_ticks_knob_flows_from_the_facet() {
        let c = MevblockerBackrun::from_config(&BotConfig::default(), String::new()).into_config();
        assert_eq!(c.verify_ticks, VerifyLevel::Bootstrap, "declared default");

        for (facet, expected) in [
            (degenbot_config::VerifyTicks::Strict, VerifyLevel::Strict),
            (
                degenbot_config::VerifyTicks::Bootstrap,
                VerifyLevel::Bootstrap,
            ),
            (degenbot_config::VerifyTicks::Off, VerifyLevel::Off),
        ] {
            let mut cfg = BotConfig::default();
            cfg.strategy.mevblocker_backrun.verify_ticks = facet;
            let c = MevblockerBackrun::from_config(&cfg, String::new()).into_config();
            assert_eq!(c.verify_ticks, expected, "the knob is wired, not ignored");
        }

        let mut cfg = BotConfig::default();
        cfg.strategy.peer_backrun.verify_ticks = degenbot_config::VerifyTicks::Strict;
        let c = PeerBackrun::from_config(&cfg, String::new()).into_config();
        assert_eq!(c.verify_ticks, VerifyLevel::Strict, "peer facet wired too");
    }

    /// The ingress Db→head window cap reaches the driver config; the declared
    /// default spans ~16h of mainnet blocks.
    #[test]
    fn ingress_backfill_max_blocks_knob_flows_from_the_facet() {
        let c = MevblockerBackrun::from_config(&BotConfig::default(), String::new()).into_config();
        assert_eq!(
            c.ingress_backfill_max_blocks, 5_000,
            "declared facet default"
        );

        let mut cfg = BotConfig::default();
        cfg.strategy.mevblocker_backrun.ingress_backfill_max_blocks = 12_345;
        let c = MevblockerBackrun::from_config(&cfg, String::new()).into_config();
        assert_eq!(
            c.ingress_backfill_max_blocks, 12_345,
            "the knob is wired, not ignored"
        );

        let mut cfg = BotConfig::default();
        cfg.strategy.peer_backrun.ingress_backfill_max_blocks = 321;
        let c = PeerBackrun::from_config(&cfg, String::new()).into_config();
        assert_eq!(c.ingress_backfill_max_blocks, 321, "peer facet wired too");
    }

    fn cfg() -> BackrunConfig {
        let mut c =
            MevblockerBackrun::from_config(&BotConfig::default(), String::new()).into_config();
        c.bid_mode = true;
        c.budget_wei = U256::from(1_000_000_000_000_000u64);
        c.max_bundle_wei = U256::from(500_000_000_000_000u64);
        c.stop_file = PathBuf::from("/nonexistent");
        c
    }

    fn swap() -> TargetClass {
        TargetClass::Swap(vec![SwapLeg {
            protocol: PoolProtocol::V2,
            pool: Some(address!("11b815efb8f581194ae79006d24e0d814b7697f6")),
            token_in: Some(address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")),
            token_out: Some(address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")),
            value: U256::ZERO,
            amount_in: None,
            amount_out_min: None,
            amount_out: None,
            amount_in_max: None,
            hops: 1,
        }])
    }

    #[test]
    fn no_bid_without_sim_pass() {
        let d = decide(&cfg(), false, &swap(), false, U256::from(1), U256::ZERO);
        assert_eq!(
            d,
            Decision::Observe {
                reason: "sim_gate_failed"
            }
        );
    }

    #[test]
    fn observe_only_default_when_bid_mode_off() {
        let mut c = cfg();
        c.bid_mode = false;
        let d = decide(&c, false, &swap(), true, U256::from(1), U256::ZERO);
        assert_eq!(
            d,
            Decision::Observe {
                reason: "observe_only"
            }
        );
    }

    #[test]
    fn bid_mode_requires_nonzero_budget() {
        let mut c = cfg();
        c.budget_wei = U256::ZERO;
        let d = decide(&c, false, &swap(), true, U256::from(1), U256::ZERO);
        assert_eq!(
            d,
            Decision::Observe {
                reason: "observe_only"
            }
        );
    }

    #[test]
    fn zero_requested_bid_cannot_bid() {
        // A frame with no composed candidate carries requested_bid == 0; the
        // only artifact that ever simulated green against it was the EXECUTOR
        // SWEEP PROBE (cmd 0x15, config 2560003: check_mode 3, bribe 100% to
        // the builder) -- bidding that simply pays the executor's balance to
        // the block builder. A Bid decision must carry a positive, composed
        // profit share: zero bids are refuse-only.
        let d = decide(&cfg(), false, &swap(), true, U256::ZERO, U256::ZERO);
        assert_eq!(d, Decision::Observe { reason: "zero_bid" },);
    }

    #[test]
    fn kill_switch_halts_bidding() {
        let d = decide(&cfg(), true, &swap(), true, U256::from(1), U256::ZERO);
        assert_eq!(
            d,
            Decision::Drop {
                reason: "kill_switch"
            }
        );
    }

    #[test]
    fn mined_target_gate_observes_already_settled() {
        // A mined target can never be backrun: the receipt-gated decision is
        // `already_settled`, not a Bid, so no dispatch can ever see it.
        let bid = Decision::Bid {
            bid_wei: U256::from(9),
        };
        assert_eq!(
            gate_mined_target(bid, true),
            Decision::Observe {
                reason: "already_settled"
            }
        );
    }

    #[test]
    fn pending_target_gate_lets_a_net_positive_bid_dispatch() {
        let bid = Decision::Bid {
            bid_wei: U256::from(9),
        };
        assert_eq!(gate_mined_target(bid, false), bid);
        // An observe decision is untouched either way.
        let observe = Decision::Observe {
            reason: "observe_only",
        };
        assert_eq!(gate_mined_target(observe, true), observe);
    }

    #[test]
    fn cumulative_budget_cap_observes_not_bids() {
        let c = cfg(); // budget 1e15, max bundle 5e14
        let d = decide(
            &c,
            false,
            &swap(),
            true,
            c.max_bundle_wei,
            U256::from(600_000_000_000_000u64),
        );
        assert_eq!(
            d,
            Decision::Observe {
                reason: "budget_exhausted"
            }
        );
    }

    #[test]
    fn all_gates_pass_bids_capped() {
        let c = cfg();
        let d = decide(
            &c,
            false,
            &swap(),
            true,
            U256::from(900_000_000_000_000u64),
            U256::ZERO,
        );
        assert_eq!(
            d,
            Decision::Bid {
                bid_wei: c.max_bundle_wei
            }
        );
    }

    #[test]
    fn inert_targets_never_bid() {
        let d = decide(
            &cfg(),
            false,
            &TargetClass::Inert,
            true,
            U256::from(1),
            U256::ZERO,
        );
        assert_eq!(
            d,
            Decision::Drop {
                reason: "inert_target"
            }
        );
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "test fixtures fail loudly on an unconstructible prerequisite"
    )]
    fn mevblocker_facet_maps_every_knob_and_binds_the_private_slot() {
        use std::collections::BTreeMap;

        use degenbot_config::{BotConfigLoader, MapEnv};

        let raw = BTreeMap::from([
            ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ACTIVE", "1"),
            ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BID_MODE", "1"),
            ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BUDGET_WEI", "42"),
            ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_MAX_BUNDLE_WEI", "99"),
            ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BRIBE_BIPS", "9500"),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_PRIORITY_FEE_GWEI",
                "7",
            ),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BUNDLE_GAS_EST",
                "333000",
            ),
            ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_DRY_RUN", "1"),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_KEY_FILE",
                "/tmp/k.key",
            ),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_EXECUTOR",
                "0x00000000000000000000000000000000000000aa",
            ),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_OPERATOR",
                "0x00000000000000000000000000000000000000bb",
            ),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_SIM_URL",
                "http://sim.local:8545",
            ),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ENDPOINTS",
                "wss://stream.local",
            ),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_MEVBLOCKER_URL",
                "http://private.local:8545",
            ),
            ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_RANK_EVIDENCE", "1"),
            ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_CONNECTORS", "5"),
            ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_CYCLE_MAX_HOPS", "5"),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_FIXTURE_HEAD",
                "26001272",
            ),
            (
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_STOP_FILE",
                "/tmp/stop",
            ),
            ("DEGENBOT_DRY_RUN_JSONL", "/tmp/f.jsonl"),
        ])
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let loaded = BotConfigLoader::new()
            .with_env(Box::new(MapEnv::new(raw)))
            .load()
            .expect("typed load");
        let c = MevblockerBackrun::from_config(&loaded.config, "http://node.local".to_string())
            .into_config();
        assert_eq!(c.rpc_url, "http://node.local");
        assert!(c.bid_mode);
        assert_eq!(c.budget_wei, U256::from(42));
        assert_eq!(c.max_bundle_wei, U256::from(99));
        assert_eq!(c.bribe_bips, 9_500);
        assert_eq!(c.priority_fee_gwei, 7);
        assert_eq!(c.bundle_gas_est, 333_000);
        assert!(c.dry_run);
        assert_eq!(c.key_file, Some(PathBuf::from("/tmp/k.key")));
        assert_eq!(c.executor, "0x00000000000000000000000000000000000000aa");
        assert_eq!(
            c.operator.as_deref(),
            Some("0x00000000000000000000000000000000000000bb")
        );
        assert_eq!(c.sim_url.as_deref(), Some("http://sim.local:8545"));
        assert_eq!(c.feed_url, degenbot_config::DEFAULT_BACKRUN_STREAM_URL);
        assert_eq!(
            c.submission,
            SubmissionSlot::Mevblocker {
                bundle_url: String::from("wss://stream.local"),
                private_url: Some(String::from("http://private.local:8545")),
            }
        );
        assert!(c.submission.names_private_endpoint());
        assert!(c.rank_evidence);
        assert_eq!(c.connectors, 5);
        assert_eq!(c.cycle_max_hops, 5);
        assert_eq!(c.fixture_head, Some(26_001_272));
        assert_eq!(c.stop_file, PathBuf::from("/tmp/stop"));
        assert_eq!(c.dry_run_jsonl, Some(PathBuf::from("/tmp/f.jsonl")));
    }

    #[test]
    #[expect(clippy::expect_used, reason = "test fixtures fail loudly")]
    fn peer_facet_maps_every_knob_and_binds_the_public_slot() {
        use std::collections::BTreeMap;

        use degenbot_config::{BotConfigLoader, MapEnv};

        let raw = BTreeMap::from([
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_ACTIVE", "1"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_BID_MODE", "1"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_BUDGET_WEI", "42"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_MAX_BUNDLE_WEI", "99"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_BRIBE_BIPS", "9500"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_PRIORITY_FEE_GWEI", "7"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_BUNDLE_GAS_EST", "333000"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_DRY_RUN", "1"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_KEY_FILE", "/tmp/k.key"),
            (
                "DEGENBOT_STRATEGY_PEER_BACKRUN_ENDPOINTS",
                "https://relay.one,https://relay.two",
            ),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_CONNECTORS", "5"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_CYCLE_MAX_HOPS", "5"),
            ("DEGENBOT_STRATEGY_PEER_BACKRUN_STOP_FILE", "/tmp/stop"),
        ])
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let loaded = BotConfigLoader::new()
            .with_env(Box::new(MapEnv::new(raw)))
            .load()
            .expect("typed load");
        let c =
            PeerBackrun::from_config(&loaded.config, "http://node.local".to_string()).into_config();
        assert_eq!(c.feed_url, degenbot_config::DEFAULT_BACKRUN_STREAM_URL);
        assert_eq!(
            c.submission,
            SubmissionSlot::PublicFanOut {
                relays: vec![
                    String::from("https://relay.one"),
                    String::from("https://relay.two"),
                ],
            }
        );
        assert_eq!(c.cycle_max_hops, 5);
        assert!(!c.submission.names_private_endpoint());
        assert!(c
            .submission
            .raw_relay_urls()
            .iter()
            .all(|url| !url.contains("private")));
    }
}
