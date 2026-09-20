//! The standalone backrun sidecar (epic 6ZOGIT, task OQQCQO; FORK-1 =
//! standalone binary, zero touches to the live engine block pump).
//!
//! The sidecar owns the full edge: `MEVBlocker` feed -> hub classification ->
//! exact-sim oracle gate -> budgeted bid via the submission leaf. The DECISION
//! layer lives here as a pure function ([`decide`]) so the safety invariants
//! are testable without a network; the bin lives in `degenbot-submission`
//! (the only crate allowed to depend on both this crate and the submission
//! leaf - see the dependency one-way note in that crate's Cargo.toml).
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
use degenbot_config::BotConfig;
use degenbot_decoders::target_class::TargetClass;

/// Typed sidecar config, built from the loaded [`BotConfig`]'s
/// `strategy.backrun` facet plus the chain-node join the bin resolves.
///
/// The key never leaves the signer - only `key_file` is named here. Every
/// legacy sidecar knob migrated onto the typed schema lands as a field: the
/// decision layer reads the first group, the bin reads the rest.
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    pub stream_url: String,
    pub rpc_url: String,
    pub key_file: Option<PathBuf>,
    /// Explicit bid-mode flag. Off = observe-only.
    pub bid_mode: bool,
    /// Cumulative bid budget cap in wei; bid mode REQUIRES non-zero.
    pub budget_wei: U256,
    /// Hard per-bundle cap in wei.
    pub max_bundle_wei: U256,
    /// Kill-switch path: if the file exists, bidding halts (then the loop).
    pub stop_file: PathBuf,
    /// Sign-nothing dispatch (`strategy.backrun.dry_run`).
    pub dry_run: bool,
    /// The builder's bribe share in bips, clamped to the `10_000` ceiling.
    pub bribe_bips: u16,
    /// Composed-bundle gas estimate priced into the net-of-gas bid gate.
    pub bundle_gas_est: u64,
    /// The operator's priority fee in gwei.
    pub priority_fee_gwei: u64,
    /// Bundle-sim endpoint; unset reuses the chain node.
    pub sim_url: Option<String>,
    /// Private-broadcast RPC for the raw relay fan-out; unset keeps the
    /// bundle-only bid and the read-provider broadcast.
    pub mevblocker_url: Option<String>,
    /// Live deep-pair ranking sanity-probe gate.
    pub rank_evidence: bool,
    /// Discovery fan-out cap (connectors per frame).
    pub connectors: usize,
    /// Offline dry-run's pinned head block.
    pub fixture_head: Option<u64>,
    /// Executor contract address (validated at the sidecar boot).
    pub executor: String,
    /// Executor owner / sim caller; unset falls back to
    /// `EXECUTOR_OWNER_ADDRESS`, then the built-in default.
    pub operator: Option<String>,
    /// Dry-run fixture frames path (`logging.dry_run_jsonl`).
    pub dry_run_jsonl: Option<PathBuf>,
}

impl SidecarConfig {
    /// Build the full sidecar config from the typed `strategy.backrun` facet
    /// plus the `logging.dry_run_jsonl` artifact knob. `rpc_url` is the
    /// chain-node join the bin resolves through the
    /// `DEGENBOT_RPC_HTTP_CHAINID_<id>` cascade - the resolver family owns
    /// every endpoint, so the facet carries no URL.
    #[must_use]
    pub fn from_config(cfg: &BotConfig, rpc_url: String) -> Self {
        let backrun = &cfg.strategy.backrun;
        Self {
            // The bundle channel URL (searcher WS) from the strategy
            // readiness: the persisted `endpoints` list (the
            // `--endpoints-default` activation stamps the pinned searcher
            // WS into it), or empty when the facet is inactive/unsettled (the
            // boots refuse that state before the driver runs).
            stream_url: degenbot_config::strategy_readiness(cfg)
                .ok()
                .and_then(|readiness| match &readiness.backrun {
                    degenbot_config::Arm::Active(urls) => urls.first().cloned(),
                    degenbot_config::Arm::Inactive => None,
                })
                .unwrap_or_default(),
            rpc_url,
            key_file: backrun.key_file.clone(),
            bid_mode: backrun.bid_mode,
            budget_wei: U256::from(backrun.budget_wei),
            max_bundle_wei: U256::from(backrun.max_bundle_wei),
            stop_file: backrun.stop_file.clone(),
            dry_run: backrun.dry_run,
            bribe_bips: u16::try_from(backrun.bribe_bips.min(10_000)).unwrap_or(10_000),
            bundle_gas_est: backrun.bundle_gas_est,
            priority_fee_gwei: backrun.priority_fee_gwei,
            sim_url: backrun.sim_url.clone(),
            mevblocker_url: backrun.mevblocker_url.clone(),
            rank_evidence: backrun.rank_evidence,
            connectors: backrun.connectors,
            fixture_head: backrun.fixture_head,
            executor: backrun.executor.clone(),
            operator: backrun.operator.clone(),
            dry_run_jsonl: cfg.logging.dry_run_jsonl.clone(),
        }
    }

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
    cfg: &SidecarConfig,
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

    fn cfg() -> SidecarConfig {
        let mut c = SidecarConfig::from_config(&BotConfig::default(), String::new());
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
    fn from_config_maps_every_migrated_knob() {
        use std::collections::BTreeMap;

        use degenbot_config::{BotConfigLoader, MapEnv};

        let raw = BTreeMap::from([
            ("DEGENBOT_STRATEGY_BACKRUN_ACTIVE", "1"),
            ("DEGENBOT_STRATEGY_BACKRUN_BID_MODE", "1"),
            ("DEGENBOT_STRATEGY_BACKRUN_BUDGET_WEI", "42"),
            ("DEGENBOT_STRATEGY_BACKRUN_MAX_BUNDLE_WEI", "99"),
            ("DEGENBOT_STRATEGY_BACKRUN_BRIBE_BIPS", "9500"),
            ("DEGENBOT_STRATEGY_BACKRUN_PRIORITY_FEE_GWEI", "7"),
            ("DEGENBOT_STRATEGY_BACKRUN_BUNDLE_GAS_EST", "333000"),
            ("DEGENBOT_STRATEGY_BACKRUN_DRY_RUN", "1"),
            ("DEGENBOT_STRATEGY_BACKRUN_KEY_FILE", "/tmp/k.key"),
            (
                "DEGENBOT_STRATEGY_BACKRUN_EXECUTOR",
                "0x00000000000000000000000000000000000000aa",
            ),
            (
                "DEGENBOT_STRATEGY_BACKRUN_OPERATOR",
                "0x00000000000000000000000000000000000000bb",
            ),
            ("DEGENBOT_STRATEGY_BACKRUN_SIM_URL", "http://sim.local:8545"),
            ("DEGENBOT_STRATEGY_BACKRUN_ENDPOINTS", "wss://stream.local"),
            ("DEGENBOT_STRATEGY_BACKRUN_RANK_EVIDENCE", "1"),
            ("DEGENBOT_STRATEGY_BACKRUN_CONNECTORS", "5"),
            ("DEGENBOT_STRATEGY_BACKRUN_FIXTURE_HEAD", "26001272"),
            ("DEGENBOT_STRATEGY_BACKRUN_STOP_FILE", "/tmp/stop"),
            ("DEGENBOT_DRY_RUN_JSONL", "/tmp/f.jsonl"),
        ])
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let loaded = BotConfigLoader::new()
            .with_env(Box::new(MapEnv::new(raw)))
            .load()
            .expect("typed load");
        let c = SidecarConfig::from_config(&loaded.config, "http://node.local".to_string());
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
        assert_eq!(c.stream_url, "wss://stream.local");
        assert!(c.rank_evidence);
        assert_eq!(c.connectors, 5);
        assert_eq!(c.fixture_head, Some(26_001_272));
        assert_eq!(c.stop_file, PathBuf::from("/tmp/stop"));
        assert_eq!(c.dry_run_jsonl, Some(PathBuf::from("/tmp/f.jsonl")));
    }
}
