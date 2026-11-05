//! The smoke-gated live registration arm of the G3 pipeline — an ADAPTER over
//! Registration admission.
//!
//! The stage order a candidate takes (memoization → construction → eligibility →
//! Verify lifecycle → registration) is the core's ONE implementation
//! ([`RegistrationAdmission`]). This module contributes what an adapter owns:
//! the batched graph enumeration, this driver's pre-registration policy gate
//! and progress cadence, and the folding of each admission receipt into the
//! core `PipelineReport` — the same fold the crawl adapter's receipts fold
//! through, so both adapters answer the same report for the same candidate
//! sequence.
//!
//! A loud admission abort (a family-level construction refusal, a direction
//! invariant violation, a verification mismatch past the retry policy) stops
//! the crawl: it is folded as a counted engine rejection and reported, never
//! disguised as a benign skip.
//!
//! This module is only reached behind `SMOKE_RPC_URL`; the offline CI gate
//! exercises the shared enumeration instead.

use std::time::Instant;

use crate::discovery::{BatchedPathFinder, BuiltGraph, DiscoveryParams};
use crate::pipeline::{admitted_hops, node_indices, resolve_path};
use crate::policy::PathPolicy;
use crate::progress::{progress_line, ProgressCadence};
use alloy::primitives::Address;
use degenbot::bot::bot_core::{PipelineReport, RegistrationOutcome, RegistrationUnitOutcome};
use degenbot::bot::crawl::RegistrationAdmission;
use degenbot::bot_core::ConstructionRoute;
use degenbot::db::discovery_read::DiscoveryPoolRow;
use degenbot::db::snapshot::TickMapDb;

/// Run the live arm over the batched discovery enumeration.
///
/// `db` is the per-use tick-map handle a head construction seeds its map from;
/// the admission owns everything else it needs.
#[expect(
    clippy::too_many_arguments,
    reason = "linear live driver: the enumeration's inputs plus the shared admission"
)]
pub async fn run_live(
    admission: &RegistrationAdmission,
    built: &BuiltGraph,
    params: &DiscoveryParams,
    policy: &PathPolicy,
    input_token_lower: &str,
    weth_lower: &str,
    db: Option<&dyn TickMapDb>,
    progress: &mut ProgressCadence,
) -> PipelineReport {
    let mut report = PipelineReport::default();
    let mut finder = BatchedPathFinder::new(&built.graph, params);
    let mut seq: u64 = 0;
    // The Registered-path cap (RSP-11) is a BENIGN STOP of discovery: the first
    // `Cap` receipt ends the crawl, exactly as Python's `run_registration`
    // checks `self.capped` at the top of its loop. Without this the loop would
    // keep grinding through every remaining candidate for no engine growth (the
    // observed 12.6M-path silent crawl).
    'crawl: while let Some(batch) = finder.next_batch() {
        for path in &batch {
            let mut stop = false;
            match node_indices(built, path) {
                Ok(pool_indices) => {
                    match resolve_path(built, &pool_indices, input_token_lower, weth_lower) {
                        Ok((_zfos, hops)) => match policy.evaluate(&hops) {
                            Err(rejection) => {
                                report.policy_rejected += 1;
                                let outcome = RegistrationUnitOutcome::Reject {
                                    detail: Some(rejection.to_string()),
                                };
                                observe_folded(&outcome);
                                report.absorb(&outcome);
                            }
                            Ok(()) => match admitted_hops(built, &pool_indices) {
                                Ok(hops) => match admission.admit(seq, &hops, None, db).await {
                                    Ok(receipt) => {
                                        stop = crawl_stops_on(&receipt.outcome);
                                        observe_folded(&receipt.outcome);
                                        report.absorb(&receipt.outcome);
                                    }
                                    Err(abort) => {
                                        // The core's loud contract: admission refuses
                                        // to continue, so this adapter offers no next
                                        // candidate. Counted as an engine rejection
                                        // (never a skip) and named for the operator.
                                        degenbot::op_warn!(
                                            domain = path,
                                            error = %abort,
                                            "g3-live: registration admission aborted the crawl"
                                        );
                                        println!("[g3-live] admission aborted the crawl: {abort}");
                                        stop = true;
                                        let outcome = RegistrationUnitOutcome::Reject {
                                            detail: Some(abort.to_string()),
                                        };
                                        observe_folded(&outcome);
                                        report.absorb(&outcome);
                                    }
                                },
                                Err(outcome) => {
                                    observe_folded(&outcome);
                                    report.absorb(&outcome);
                                }
                            },
                        },
                        Err(message) => {
                            report.direction_errors += 1;
                            *report
                                .skip_reasons
                                .entry(format!("direction-fatal:{message}"))
                                .or_insert(0) += 1;
                        }
                    }
                }
                Err(outcome) => {
                    observe_folded(&outcome);
                    report.absorb(&outcome);
                }
            }
            seq += 1;
            // INN6TK: the time-throttled summary fires even when nothing
            // registers, so a discovery-heavy skip-fest stays visible.
            if progress.due(Instant::now()) {
                println!("{}", progress_line(&report));
            }
            if stop {
                break 'crawl;
            }
        }
        tokio::task::yield_now().await;
    }
    // The completion summary is forced regardless of the cadence (Python's
    // `pipeline.emit_registration_progress(force=True)`).
    println!("{}", progress_line(&report));
    report
}

/// Whether the crawl must stop after this unit outcome.
///
/// The typed [`RegistrationUnitOutcome::Cap`] is the fold of the engine
/// registry's `PathRegistrationError::RegistryFull` refusal: the BENIGN STOP of
/// discovery (Python latches `capped` and breaks `run_registration`), never
/// counted as a per-candidate error and never a reason to keep crawling a full
/// registry.
#[must_use]
pub fn crawl_stops_on(outcome: &RegistrationUnitOutcome) -> bool {
    matches!(outcome, RegistrationUnitOutcome::Cap)
}

/// The construction route this deployment's enumeration serves: the DEX
/// factories the discovery read named, plus the core's generic rung.
///
/// The route is a VALUE the driver resolves (the core never invents one): a
/// pool whose on-chain `factory()` matches an enumerated exchange constructs at
/// that rung, and the generic rung serves any family the core deployment tables
/// resolve.
#[must_use]
pub fn construction_route(rows: &[DiscoveryPoolRow]) -> ConstructionRoute {
    let mut factories: Vec<Address> = Vec::new();
    for row in rows {
        let factory = match row {
            DiscoveryPoolRow::V2(r) => Some(r.exchange.factory),
            DiscoveryPoolRow::V3(r) => Some(r.exchange.factory),
            DiscoveryPoolRow::V4(_) => None,
        };
        if let Some(factory) = factory {
            if !factories.contains(&factory) {
                factories.push(factory);
            }
        }
    }
    ConstructionRoute {
        factories,
        generic: true,
    }
}

/// The adapter's observation policy over one folded outcome: the bounded,
/// env-gated diagnostic sample. Purely observational — it never mutates a
/// counter, memo, or classification.
fn observe_folded(outcome: &RegistrationUnitOutcome) {
    match outcome {
        RegistrationUnitOutcome::RegisterFailed {
            detail: Some(detail),
        } => emit_sample(RegistrationOutcome::RegisterFailed.as_str(), detail),
        RegistrationUnitOutcome::Skip {
            label,
            detail: Some(detail),
            ..
        } => emit_sample(label.as_label(), detail),
        RegistrationUnitOutcome::Skip { .. }
        | RegistrationUnitOutcome::Reject { .. }
        | RegistrationUnitOutcome::Cap
        | RegistrationUnitOutcome::RegisterFailed { detail: None }
        | RegistrationUnitOutcome::Registered { .. } => {}
    }
}

/// At most this many distinct samples are ever retained (and printed) per tag,
/// so the sampler's cardinality stays bounded even across a multi-hour crawl.
const REG_DEBUG_MAX_DISTINCT: usize = 3;

/// Diagnostic-only sampler gated by `DEGENBOT_REG_DEBUG_SAMPLES=1`: prints up to
/// [`REG_DEBUG_MAX_DISTINCT`] distinct `(tag, detail)` pairs so a folded outcome
/// class can be traced to the underlying refusal that produced it.
///
/// Purely observational: it never mutates a counter, memo, or classification,
/// and does nothing unless the env gate is set, so production behavior is
/// unchanged.
fn emit_sample(tag: &str, detail: &str) {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, usize>>> =
        std::sync::OnceLock::new();
    if std::env::var("DEGENBOT_REG_DEBUG_SAMPLES").ok().as_deref() != Some("1") {
        return;
    }
    let seen = SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let mut guard = match seen.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let emitted = guard.entry(tag.to_string()).or_insert(0);
    if *emitted < REG_DEBUG_MAX_DISTINCT {
        *emitted += 1;
        println!("[reg-debug] tag={tag} detail={detail}");
    }
}

/// The V4 `StateView` contract address the registration verify lifecycle must
/// use, resolved from the discovered `pool_managers` rows — the same per-manager
/// column the V4 build path already trusts (`DiscoveryV4Row::manager.state_view`).
///
/// Python sources this fact from a chain deployment constant
/// (`EthereumMainnetUniswapV4.state_view.address`) and passes it to
/// `EngineRegistry.start(..., verify_state_view=...)`
/// (`src/degenbot/runner/bot_runner.py:470`). The fresh Rust driver boots with no
/// deployment registry, so the example resolves the same fact from the snapshot
/// enumeration instead of hard-coding a chain address; the first manager row that
/// carries one wins (one `StateView` per `pool_manager`, and the verify lifecycle
/// takes a single address).
///
/// `None` when the enumeration carries no V4 manager row with a `state_view`; the
/// caller then leaves the driver unconfigured and every V4 verify fails fast with
/// its D-C no-config refusal (loud, never silently skipped).
#[must_use]
pub fn resolve_verify_state_view(rows: &[DiscoveryPoolRow]) -> Option<Address> {
    rows.iter().find_map(|row| match row {
        DiscoveryPoolRow::V4(v4) => v4.manager.state_view,
        DiscoveryPoolRow::V2(_) | DiscoveryPoolRow::V3(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use degenbot::bot::bot_core::{OutcomeLabel, RegistrationOutcome};
    use degenbot::db::rows::{Erc20TokenRow, ExchangeRow, PoolManagerRow};

    /// A V4 manager row carrying `state_view` supplies the driver's
    /// scalar/bootstrap configuration. Full-map verification uses the manager
    /// address and `PoolId`, so this setting does not select the RPC target.
    #[test]
    fn verify_state_view_resolves_from_the_first_v4_manager_row() {
        let view = Address::from([0x77u8; 20]);
        assert_eq!(
            resolve_verify_state_view(&[v4_row_with_state_view(Some(view))]),
            Some(view)
        );
        assert_eq!(resolve_verify_state_view(&[]), None);
    }

    /// A V4 manager row without a `state_view` resolves to `None`; this only
    /// means optional scalar/bootstrap configuration is absent. Full-map
    /// verification still targets the manager address and `PoolId`.
    #[test]
    fn verify_state_view_is_none_without_a_manager_state_view() {
        assert_eq!(
            resolve_verify_state_view(&[v4_row_with_state_view(None)]),
            None
        );
    }

    /// A minimal V4 discovery row whose manager carries `state_view`.
    fn v4_row_with_state_view(state_view: Option<Address>) -> DiscoveryPoolRow {
        use degenbot::db::discovery_read::DiscoveryV4Row;
        let token = |id: i64, byte: u8| Erc20TokenRow {
            id,
            chain: 1,
            address: Address::from([byte; 20]),
            name: None,
            symbol: None,
            decimals: Some(18),
        };
        DiscoveryPoolRow::V4(DiscoveryV4Row {
            managed_pool_id: 1,
            pool_hash: alloy::primitives::B256::ZERO,
            hooks: Address::ZERO,
            manager: PoolManagerRow {
                id: 1,
                address: Address::from([0x44u8; 20]),
                chain: 1,
                kind: "uniswap_v4".to_string(),
                state_view,
                exchange_id: 1,
            },
            token0: token(1, 0xaa),
            token1: token(2, 0xbb),
            exchange: ExchangeRow {
                id: 1,
                chain_id: 1,
                name: "uniswap_v4".to_string(),
                active: true,
                last_update_block: None,
                factory: Address::ZERO,
                deployer: None,
            },
            fee_currency0: 0,
            fee_currency1: 0,
            fee_denominator: 1_000_000,
            tick_spacing: 10,
            liquidity_update_block: None,
            liquidity_update_log_index: None,
        })
    }

    #[test]
    fn registry_full_is_a_benign_crawl_stop() {
        // The admission folds the engine registry's `RegistryFull` refusal to
        // the core's `RegistrationUnitOutcome::Cap`; that outcome stops the
        // crawl and latches the report's capped witness via the core fold.
        assert!(crawl_stops_on(&RegistrationUnitOutcome::Cap));
        assert!(!crawl_stops_on(&RegistrationUnitOutcome::Registered {
            created: true,
            v4_hops: 0,
        }));
        assert!(!crawl_stops_on(&RegistrationUnitOutcome::Skip {
            label: OutcomeLabel::Vocabulary(RegistrationOutcome::V4NoHash),
            counts_as_skip: true,
            detail: None,
        }));

        let mut report = PipelineReport::default();
        report.absorb(&RegistrationUnitOutcome::Cap);
        assert!(report.capped, "the cap stop latches the benign witness");
        assert_eq!(report.cap_skip_count, 1);
        assert_eq!(report.skip_count, 1);
    }

    #[test]
    fn the_construction_route_is_the_enumerated_factories_plus_the_generic_rung() {
        let route = construction_route(&[v4_row_with_state_view(None)]);
        assert!(
            route.generic,
            "the generic rung serves every family the deployment tables resolve"
        );
        assert!(
            route.factories.is_empty(),
            "a V4 manager names no factory rung"
        );
    }
}
