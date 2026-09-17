//! The standalone backrun sidecar (epic 6ZOGIT, task OQQCQO; FORK-1 =
//! standalone binary, zero touches to the live engine block pump).
//!
//! The sidecar owns the full edge: `MEVBlocker` feed -> hub classification ->
//! exact-sim oracle gate -> budgeted bid via the submission leaf. The DECISION
//! layer lives here as a pure function ([`decide`]) so the safety invariants
//! are testable without a network; the bin wires the live halves.
//!
//! Safety model (task acceptance):
//! - observe-only default: no bid unless `bid_mode` is set AND the budget is
//!   non-zero (bid mode gated behind an explicit flag AND a non-zero budget);
//! - no bid without a passing sim (the oracle gate is upstream of `decide`);
//! - hard per-bundle + cumulative budget caps;
//! - STOP kill-switch file: existence halts all bidding, then the loop;
//! - stale candidates dropped (bid liveness decays in ~1 block).

use std::path::PathBuf;

use alloy::primitives::U256;
use degenbot_decoders::target_classifier::TargetClass;

/// Env-driven sidecar config (bot.env convention; the key never leaves the
/// signer - only `key_file` is named here).
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    pub stream_url: String,
    pub rpc_url: String,
    pub key_file: Option<PathBuf>,
    /// Explicit bid-mode flag (`SIDECAR_BID_MODE=1`). Off = observe-only.
    pub bid_mode: bool,
    /// Cumulative bid budget cap in wei; bid mode REQUIRES non-zero.
    pub budget_wei: U256,
    /// Hard per-bundle cap in wei.
    pub max_bundle_wei: U256,
    /// Kill-switch path: if the file exists, bidding halts (then the loop).
    pub stop_file: PathBuf,
    /// Age beyond which a candidate is dropped (liveness decays in ~1 block).
    pub stale_ms: u64,
}

impl SidecarConfig {
    ///
    /// # Panics
    ///
    /// Panics if `SIDECAR_RPC_URL` is unset (the node join is required for
    /// both modes; the feed URL already has a default).
    #[must_use]
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok();
        Self {
            stream_url: env("SIDECAR_STREAM_URL")
                .unwrap_or_else(|| String::from("wss://rpc.mevblocker.io/stream")),
            rpc_url: env("SIDECAR_RPC_URL").unwrap_or_else(|| {
                tracing::error!("sidecar requires SIDECAR_RPC_URL");
                std::process::exit(2);
            }),
            key_file: env("SIDECAR_KEY_FILE").map(PathBuf::from),
            bid_mode: env("SIDECAR_BID_MODE").is_some_and(|v| v == "1"),
            budget_wei: env("SIDECAR_BUDGET_WEI")
                .and_then(|v| v.parse().ok())
                .unwrap_or_default(),
            max_bundle_wei: env("SIDECAR_MAX_BUNDLE_WEI")
                .and_then(|v| v.parse().ok())
                .unwrap_or(U256::from(1_000_000_000_000_000u64)),
            stop_file: env("SIDECAR_STOP_FILE").map_or_else(
                || PathBuf::from("/tmp/degenbot-sidecar-STOP"),
                PathBuf::from,
            ),
            stale_ms: env("SIDECAR_STALE_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1500),
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
/// - Stale candidates (`age_ms > stale_ms`) dropped.
/// - Cumulative budget caps: spent + binder > budget -> Observe.
/// - Per-bundle cap: bundle > `max_bundle_wei` -> Observe.
#[must_use]
pub fn decide(
    cfg: &SidecarConfig,
    stop_file_exists: bool,
    target_class: &TargetClass,
    sim_ok: bool,
    requested_bid_wei: U256,
    age_ms: u64,
    spent_wei: U256,
) -> Decision {
    if stop_file_exists {
        return Decision::Drop {
            reason: "kill_switch",
        };
    }
    if age_ms > cfg.stale_ms {
        return Decision::Drop {
            reason: "stale_candidate",
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use degenbot_decoders::target_classifier::{PoolProtocol, SwapLeg};

    fn cfg() -> SidecarConfig {
        SidecarConfig {
            stream_url: String::new(),
            rpc_url: String::new(),
            key_file: None,
            bid_mode: true,
            budget_wei: U256::from(1_000_000_000_000_000u64),
            max_bundle_wei: U256::from(500_000_000_000_000u64),
            stop_file: PathBuf::from("/nonexistent"),
            stale_ms: 1500,
        }
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
        let d = decide(&cfg(), false, &swap(), false, U256::from(1), 10, U256::ZERO);
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
        let d = decide(&c, false, &swap(), true, U256::from(1), 10, U256::ZERO);
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
        let d = decide(&c, false, &swap(), true, U256::from(1), 10, U256::ZERO);
        assert_eq!(
            d,
            Decision::Observe {
                reason: "observe_only"
            }
        );
    }

    #[test]
    fn kill_switch_halts_bidding() {
        let d = decide(&cfg(), true, &swap(), true, U256::from(1), 10, U256::ZERO);
        assert_eq!(
            d,
            Decision::Drop {
                reason: "kill_switch"
            }
        );
    }

    #[test]
    fn stale_candidates_dropped() {
        let d = decide(
            &cfg(),
            false,
            &swap(),
            true,
            U256::from(1),
            2_000,
            U256::ZERO,
        );
        assert_eq!(
            d,
            Decision::Drop {
                reason: "stale_candidate"
            }
        );
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
            10,
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
            10,
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
            10,
            U256::ZERO,
        );
        assert_eq!(
            d,
            Decision::Drop {
                reason: "inert_target"
            }
        );
    }
}
