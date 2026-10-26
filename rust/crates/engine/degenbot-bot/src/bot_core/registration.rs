//! The registration cluster on the `Bot` facade. Moved from the PyO3 shell
//! (`degenbot-python/src/bot/mod.rs`): the shell keeps only arg parsing +
//! `PyErr` mapping, while the CREATE2 verification, deployer/init-hash
//! resolution, params assembly, registry-of-record payload shaping,
//! skip-label collapsing, and tick-row normalization live here so the
//! standalone-Rust path gets the same registrations.
//!
//! Invariants preserved from the shell:
//! - PRG-2: the keyed immutable V4 admission verdicts are consulted BEFORE
//!   any registry read on the already-registered fast path
//!   ([`Bot::registered_v4_payload`]), with the same typed refusal the live
//!   registration would raise.
//! - PRG-1: the single-flight build table stays on the shell (`PyBot`);
//!   these functions take what they need as parameters and never touch
//!   flight state.
//! - The retained `SnapshotDb` handle is borrowed (`&dyn TickMapDb`) by the
//!   `assemble_*` entry points — the shell clones its `PyBot.db` Arc and
//!   passes the borrow, so the shared frozen-WAL-snapshot read is unchanged.

use std::fmt;

use alloy::primitives::Address;
use degenbot_pools::sim::tick_fetch::TickBootstrapRpc;
use degenbot_substrate::registration_gate::AdmissionVerdict;
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::tick_assembly::TickMapAssemblyError;
use degenbot_substrate::{PoolLookup, PoolRegistration};

use super::bot::Bot;
use super::registration_ledger::{
    BuildFailure, RefusalKind, RegisterRefusal, RegisteredPoolFamily, RegistrationPhase,
};
use crate::bot_core::pool_builder::parity;
use crate::bot_core::{
    ConcentratedLiquiditySlotLayout, PoolTickCoverage, RegisterAerodromePoolError,
    RegisterAerodromeV2PoolParams, RegisterBalancerStablePoolParams,
    RegisterBalancerWeightedPoolParams, RegisterCurvePoolParams, RegisterV2PoolError,
    RegisterV2PoolParams, RegisterV3PoolError, RegisterV3PoolParams, RegisterV4PoolError,
    RegisterV4PoolParams, TickInfo,
};

/// The PRG-3 stance refusal: the process's fleet stance is not `fleet`, so
/// the registration intake is not hosted here and the Python driver must
/// keep the incumbent worker pool.
#[derive(Debug)]
pub struct RegistrationIntakeNotHosted;

impl fmt::Display for RegistrationIntakeNotHosted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "registration intake is not fleet-hosted (fleet.stance != fleet)"
        )
    }
}

impl std::error::Error for RegistrationIntakeNotHosted {}

/// One normalized tick-map row: `(tick, liquidity_gross: u128,
/// liquidity_net: i128, block: u64)` — the dict-entry shape the shell's
/// `build_tick_rows_py` turns into a Python `{tick: (gross, net, block)}`.
pub type TickRow = (i32, u128, i128, u64);

/// The already-registered V4 registry-of-record echo: `(pool_id,
/// currency0, currency1, pool_manager)` — identity straight off the core's
/// immutable pool key. The fee/state-view values a consumer needs read off
/// the registered handle itself (the handle is self-describing, ADR-085
/// D4); this echo answers the ONE construction verb's GET arm.
pub type V4RegisteredEcho = (u64, String, String, String);

/// Parse one address string with the shell's historical message vocabulary.
///
/// # Errors
/// [`RefusalKind::MalformedInput`] (the intake phase) when `s` is not a
/// parseable address; its `detail` carries the historical
/// `Invalid address '<input>': <source>` rendering.
pub fn parse_address_str(s: &str) -> Result<Address, RefusalKind> {
    s.parse::<Address>().map_err(|source| {
        RefusalKind::malformed_input(s.to_string(), format!("Invalid address '{s}': {source}"))
    })
}

/// Parse a list of address strings (the shell extracts the `Vec<String>`
/// from the Python list, keeping its per-item "token address must be a str"
/// surface and per-item error precedence).
///
/// # Errors
/// [`RefusalKind::MalformedInput`] for the first unparseable entry.
pub fn parse_address_list(items: &[String]) -> Result<Vec<Address>, RefusalKind> {
    items.iter().map(|s| parse_address_str(s)).collect()
}

/// PRG-3: refuse the registration-intake submit when the process's fleet
/// stance is not `fleet` (the shell's submit-path guard, moved core-side so
/// a pure-Rust driver sees the same refusal).
///
/// # Errors
/// [`RegistrationIntakeNotHosted`] under the legacy stance.
pub fn ensure_registration_fleet_hosted() -> Result<(), RegistrationIntakeNotHosted> {
    if crate::fleet_intake::registration_boot_installed() {
        Ok(())
    } else {
        Err(RegistrationIntakeNotHosted)
    }
}

/// Collapse a registration-skip reason onto the closed label set (the
/// instruments' cardinality discipline) — per-error-class detail stays in
/// the greppable `[build_paths] Progress` breakdown, not label cardinality.
#[must_use]
pub fn registration_skip_kind(reason: &str) -> &'static str {
    match reason {
        "v4-hook-rejected" | "v4-dynamic-fee-rejected" | "v4-high-fee-rejected" => "v4-admission",
        "path-cap" => "path-cap",
        "dup" => "dup",
        "direction-mismatch" | "v4-no-hash" | "unknown-pool-type" => "candidate-invalid",
        "engine-reject" => "engine-reject",
        other if other.starts_with("register-fail") => "register-fail",
        other if other.starts_with("build-v2:") => "pool-build-error",
        other if other.starts_with("build-v3:") => "pool-build-error",
        other if other.starts_with("build-v4:") => "pool-build-error",
        _ => "other",
    }
}

/// Normalize assembled ticks into the dict-row shape the shell marshals into
/// a Python `{tick: (gross, net, block)}` dict, in the map's own iteration
/// order (the historical dict ordering — no sort is introduced).
/// `liquidity_gross` narrows `U128 → u128` (infallible for valid on-chain
/// values — Uniswap's `type(uint128).max` cap).
#[must_use]
pub fn tick_rows(ticks: &hashbrown::HashMap<i32, TickInfo>) -> Vec<TickRow> {
    ticks
        .iter()
        .map(|(&tick, info)| {
            (
                tick,
                info.liquidity_gross.to::<u128>(),
                info.liquidity_net,
                info.block,
            )
        })
        .collect()
}

impl Bot {
    // === Registration cluster (moved from the PyO3 shell): ===
    // === the shell keeps only arg parsing + PyErr mapping; the CREATE2    ===
    // === verification, deployer/init-hash resolution, params assembly,   ===
    // === and the lock discipline live here so the standalone-Rust path   ===
    // === gets the same registrations.                                    ===

    /// The ONE registration verb on the engine `Bot` (ADR-078 D3 step 3): the
    /// FFI shell translates Python kwargs into one [`PoolRegistration`] and
    /// dispatches here — a new family adds a variant arm, never a pymethod.
    /// Each family's engine-side depth (the get-or-register guard, the
    /// CREATE2/EIP-1167 identity verify, the derived deployer/init-hash
    /// completion) is the request interior behind this verb; the insert
    /// itself is the substrate door (`BotState::register_pool`), so every
    /// refusal crosses as the one [`RegistrationVerdict`] vocabulary and
    /// folds onto [`RefusalKind`] at this seam.
    ///
    /// # Errors
    /// The family's typed [`RefusalKind`] refusal.
    pub fn register(&self, registration: PoolRegistration) -> Result<u64, RefusalKind> {
        match registration {
            PoolRegistration::V2(params) => self.register_v2_request(*params),
            PoolRegistration::V3(params) => self.register_v3_request(*params),
            PoolRegistration::V4(params) => self.register_v4_request(*params),
            PoolRegistration::Curve(params) => self.register_curve_request(*params),
            PoolRegistration::BalancerWeighted(params) => {
                self.register_balancer_weighted_request(*params)
            }
            PoolRegistration::BalancerStable(params) => {
                self.register_balancer_stable_request(*params)
            }
            PoolRegistration::AerodromeV2(params) => self.register_aerodrome_request(*params),
        }
    }

    /// The V2 request interior (see [`Self::register`]): the get-or-register
    /// family guard, the JSON-sourced CREATE2 verification (Fork A) — skipped
    /// when `(chain, factory)` is not in the shipped deployments JSON,
    /// preserving the manual/ad-hoc path — the derived deployer + init-hash
    /// completion, and the door insert under ONE core write guard.
    fn register_v2_request(&self, mut params: RegisterV2PoolParams) -> Result<u64, RefusalKind> {
        // get-or-register, family-guarded: the same address + same family +
        // same identity answers the EXISTING pool id (the `route.rs`
        // `registered_address_pool` discipline lifted to the register entry);
        // a different family or a disagreeing identity is wire drift and stays
        // the loud typed `AlreadyRegistered` refusal.
        {
            let core = self.state_arc();
            let registry = core.read_at(LockSite::Orchestrator);
            if let Some((pool_id, family)) = registry.registered_pool_by_address(&params.address) {
                let hit = family == RegisteredPoolFamily::V2
                    && registry
                        .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(pool_id)
                        .is_some_and(|ident| {
                            ident.token0 == params.token0
                                && ident.token1 == params.token1
                                && ident.factory == params.factory
                        });
                if hit {
                    return Ok(pool_id);
                }
                return Err(RefusalKind::from_register_v2_error(
                    &RegisterV2PoolError::AlreadyRegistered {
                        address: params.address,
                    },
                ));
            }
        }
        parity::verify_v2_create2(
            self.chain_id(),
            params.factory,
            params.address,
            params.token0,
            params.token1,
        )
        .map_err(|divergence| {
            RefusalKind::from_identity_divergence(Some(RegisteredPoolFamily::V2), &divergence)
        })?;
        // Derived identity completes at the request interior: the deployer +
        // init hash are a pure function of (chain_id, factory), so a caller
        // never has to carry them (the request's carried values, if any, are
        // overwritten with the resolved ones).
        params.deployer =
            degenbot_uniswap::deployments::resolve_deployer(self.chain_id(), params.factory);
        params.init_hash =
            degenbot_uniswap::deployments::resolve_v2_init_hash(self.chain_id(), params.factory);
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::V2(Box::new(params))),
        )
    }

    /// Resolve a V3 pool's concrete storage-slot layout for `factory`.
    ///
    /// [`RegisterV3PoolParams::slot_layout`] is deliberately concrete, so the
    /// deployment-table deferral resolves HERE, once, for every consumer: a
    /// caller passes `None` to defer to the shipped JSON (a `PancakeSwap`
    /// factory), or an explicit layout to force it.
    #[must_use]
    pub fn v3_slot_layout_for(
        &self,
        factory: Address,
        explicit: Option<ConcentratedLiquiditySlotLayout>,
    ) -> ConcentratedLiquiditySlotLayout {
        explicit.unwrap_or_else(|| {
            if degenbot_uniswap::deployments::is_pancakeswap_v3_factory(self.chain_id(), factory) {
                ConcentratedLiquiditySlotLayout::PancakeV3
            } else {
                ConcentratedLiquiditySlotLayout::UniswapV3
            }
        })
    }

    /// The V3 request interior (see [`Self::register`]): the get-or-register
    /// family guard, the CREATE2 verify, the derived deployer + init-hash
    /// completion, and the door insert under ONE core write guard. The
    /// request's `slot_layout` is the caller-resolved concrete layout (the
    /// params type carries it as mandatory; [`Self::v3_slot_layout_for`] owns
    /// the deployment-table deferral).
    fn register_v3_request(&self, mut params: RegisterV3PoolParams) -> Result<u64, RefusalKind> {
        // get-or-register, family-guarded (see `register_v2_request`).
        {
            let core = self.state_arc();
            let registry = core.read_at(LockSite::Orchestrator);
            if let Some((pool_id, family)) = registry.registered_pool_by_address(&params.address) {
                let hit = family == RegisteredPoolFamily::V3
                    && registry
                        .pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(pool_id)
                        .is_some_and(|ident| {
                            ident.token0 == params.token0
                                && ident.token1 == params.token1
                                && ident.factory == params.factory
                                && ident.fee == params.fee
                                && ident.tick_spacing == params.tick_spacing
                        });
                if hit {
                    return Ok(pool_id);
                }
                return Err(RefusalKind::from_register_v3_error(
                    &RegisterV3PoolError::AlreadyRegistered {
                        address: params.address,
                    },
                ));
            }
        }
        parity::verify_v3_create2(
            self.chain_id(),
            params.factory,
            params.address,
            params.token0,
            params.token1,
            params.fee,
        )
        .map_err(|divergence| {
            RefusalKind::from_identity_divergence(Some(RegisteredPoolFamily::V3), &divergence)
        })?;
        // Derived identity completes at the request interior (see
        // `register_v2_request`).
        params.deployer =
            degenbot_uniswap::deployments::resolve_deployer(self.chain_id(), params.factory);
        params.init_hash =
            degenbot_uniswap::deployments::resolve_v3_init_hash(self.chain_id(), params.factory);
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::V3(Box::new(params))),
        )
    }

    /// The V4 request interior (see [`Self::register`]): the get-or-register
    /// guard and the door insert under ONE core write guard. The guard stays
    /// core-side: the substrate door's V4 arm refuses every duplicate
    /// `(pool_manager, pool_id)` without the currency-match nuance, so the
    /// reuse rule cannot live behind it.
    fn register_v4_request(&self, params: RegisterV4PoolParams) -> Result<u64, RefusalKind> {
        // get-or-register on the `(pool_manager, pool_id)` pair: a matching
        // pool key answers the EXISTING pool id; a disagreeing key (same pair,
        // different currencies) is the loud `AlreadyRegistered`.
        {
            let core = self.state_arc();
            let registry = core.read_at(LockSite::Orchestrator);
            if let Some(existing) = registry.try_registered(params.pool_manager, &params.pool_id) {
                let hit = existing.pool_key.currency0 == params.pool_key.currency0
                    && existing.pool_key.currency1 == params.pool_key.currency1;
                if hit {
                    return Ok(existing.pool_id);
                }
                let refused = RegisterV4PoolError::AlreadyRegistered {
                    pool_manager: params.pool_manager,
                    pool_id: params.pool_id,
                };
                return Err(RefusalKind::RegisterRefused {
                    family: Some(RegisteredPoolFamily::V4),
                    phase: RegistrationPhase::Register,
                    reason: RegisterRefusal::AlreadyRegistered {
                        address: None,
                        pool_key: Some((params.pool_manager, params.pool_id)),
                        detail: format!("{refused:?}"),
                    },
                });
            }
        }
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::V4(Box::new(params))),
        )
    }

    /// The Curve request interior (see [`Self::register`]): the
    /// get-or-register guard and the door insert.
    fn register_curve_request(&self, params: RegisterCurvePoolParams) -> Result<u64, RefusalKind> {
        // get-or-register: a same-address entry with the same coin list reuses
        // the existing pool; a disagreeing entry is refused by the door's
        // typed duplicate-address arm (ADR-078 D1 — the state insert's
        // already-registered panic no longer fires behind this verb).
        {
            let core = self.state_arc();
            let registry = core.read_at(LockSite::Orchestrator);
            if let Some((pool_id, RegisteredPoolFamily::Curve)) =
                registry.registered_pool_by_address(&params.address)
            {
                if registry
                    .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(pool_id)
                    .is_some_and(|ident| ident.tokens.as_ref() == params.tokens.as_slice())
                {
                    return Ok(pool_id);
                }
            }
        }
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::Curve(Box::new(params))),
        )
    }

    /// The Balancer-weighted request interior (see [`Self::register`]): the
    /// get-or-register guard and the door insert.
    fn register_balancer_weighted_request(
        &self,
        params: RegisterBalancerWeightedPoolParams,
    ) -> Result<u64, RefusalKind> {
        // get-or-register: a same-address entry with the same vault + pool id
        // + coin list reuses the existing pool (see `register_curve_request`
        // for the conflict discipline).
        {
            let core = self.state_arc();
            let registry = core.read_at(LockSite::Orchestrator);
            if let Some((pool_id, RegisteredPoolFamily::BalancerWeighted)) =
                registry.registered_pool_by_address(&params.address)
            {
                if registry
                    .pool_slice::<degenbot_pools::state::balancer_weighted_state::BalancerWeightedPoolIdentity>(pool_id)
                    .is_some_and(|ident| {
                        ident.vault == params.vault
                            && ident.pool_id == params.pool_id
                            && ident.tokens.as_ref() == params.tokens.as_slice()
                    })
                {
                    return Ok(pool_id);
                }
            }
        }
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::BalancerWeighted(Box::new(params))),
        )
    }

    /// The Balancer-stable request interior (see [`Self::register`]): the
    /// get-or-register guard and the door insert.
    fn register_balancer_stable_request(
        &self,
        params: RegisterBalancerStablePoolParams,
    ) -> Result<u64, RefusalKind> {
        // get-or-register: see `register_balancer_weighted_request`.
        {
            let core = self.state_arc();
            let registry = core.read_at(LockSite::Orchestrator);
            if let Some((pool_id, RegisteredPoolFamily::BalancerStable)) =
                registry.registered_pool_by_address(&params.address)
            {
                if registry
                    .pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolIdentity>(pool_id)
                    .is_some_and(|ident| {
                        ident.vault == params.vault
                            && ident.pool_id == params.pool_id
                            && ident.tokens.as_ref() == params.tokens.as_slice()
                    })
                {
                    return Ok(pool_id);
                }
            }
        }
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::BalancerStable(Box::new(params))),
        )
    }

    /// The Aerodrome request interior (see [`Self::register`]): the
    /// get-or-register family guard, the JSON-sourced EIP-1167
    /// deployer/implementation verification (Fork A follow-on; skipped for
    /// non-JSON `(chain, factory)` rows), and the door insert under ONE core
    /// write guard.
    fn register_aerodrome_request(
        &self,
        params: RegisterAerodromeV2PoolParams,
    ) -> Result<u64, RefusalKind> {
        // get-or-register, family-guarded (see `register_v2_request`).
        {
            let core = self.state_arc();
            let registry = core.read_at(LockSite::Orchestrator);
            if let Some((pool_id, family)) = registry.registered_pool_by_address(&params.address) {
                let hit = family == RegisteredPoolFamily::AerodromeV2
                    && registry
                        .pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                            pool_id,
                        )
                        .is_some_and(|ident| {
                            ident.token0 == params.token0
                                && ident.token1 == params.token1
                                && ident.factory == params.factory
                        });
                if hit {
                    return Ok(pool_id);
                }
                return Err(RefusalKind::from_register_aerodrome_error(
                    &RegisterAerodromePoolError::AlreadyRegistered {
                        address: params.address,
                    },
                ));
            }
        }
        parity::verify_aerodrome_v2_create2(
            self.chain_id(),
            params.factory,
            params.address,
            params.token0,
            params.token1,
            params.stable,
        )
        .map_err(|divergence| {
            RefusalKind::from_identity_divergence(
                Some(RegisteredPoolFamily::AerodromeV2),
                &divergence,
            )
        })?;
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::AerodromeV2(Box::new(params))),
        )
    }

    /// Register a token (the pure-Rust insertion the shell released the GIL
    /// across; the handle construction stays at the shell).
    pub fn register_token(
        &self,
        address: Address,
        name: String,
        symbol: String,
        decimals: u8,
        chain_id: u64,
    ) {
        self.state_arc()
            .write_at(LockSite::Orchestrator)
            .register_token(address, name, symbol, decimals, chain_id);
    }

    /// Seed the Rust-owned V4 `StateView` registry (ADR-005 / Option 2).
    pub fn register_state_view(&self, pool_manager: Address, state_view: Address) {
        self.state_arc()
            .write_at(LockSite::Orchestrator)
            .register_state_view(pool_manager, state_view);
    }

    /// Unregister a V2/V3 pool by its contract address (ADR-007 U3): drops
    /// the entry, journal, index entry, and buffered V3 events. `false` when
    /// the address was never registered (silent no-op). V4 tuple-key
    /// unregister is engine-side and never routed here.
    #[must_use]
    pub fn unregister_pool(&self, key: PoolLookup) -> bool {
        self.state_arc()
            .write_at(LockSite::Orchestrator)
            .unregister_pool(key)
    }

    /// The address-keyed registry-of-record payload for a V2 build: identity
    /// straight off the registered entry (parity with the builder's return
    /// surface — token0/token1/address/variant — read from the SAME source).
    /// `None` when the address is unregistered or NOT a V2 pool (the
    /// single-flight pre-check contract).
    #[must_use]
    pub fn registered_v2_payload(
        &self,
        addr: &Address,
    ) -> Option<(u64, String, String, String, String)> {
        let core = self.state_arc();
        let state = core.read_at(LockSite::Orchestrator);
        let (pool_id, RegisteredPoolFamily::V2) = state.registered_pool_by_address(addr)? else {
            return None;
        };
        let ident = state.pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(pool_id)?;
        Some((
            pool_id,
            ident.token0.to_checksum(None),
            ident.token1.to_checksum(None),
            ident.address.to_checksum(None),
            ident.variant.as_str().to_string(),
        ))
    }

    /// The V3 twin of [`Self::registered_v2_payload`] — family string
    /// resolved from the registered `factory` exactly as the builder's
    /// return surface does.
    #[must_use]
    pub fn registered_v3_payload(
        &self,
        addr: &Address,
        chain_id: u64,
    ) -> Option<(u64, String, String, String, String)> {
        let core = self.state_arc();
        let state = core.read_at(LockSite::Orchestrator);
        let (pool_id, RegisteredPoolFamily::V3) = state.registered_pool_by_address(addr)? else {
            return None;
        };
        let ident = state.pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(pool_id)?;
        let family = degenbot_uniswap::deployments::resolve_dex_name(chain_id, ident.factory)
            .map_or_else(|| "uniswap-v3".to_string(), |d| d.as_str().to_string());
        Some((
            pool_id,
            ident.token0.to_checksum(None),
            ident.token1.to_checksum(None),
            ident.address.to_checksum(None),
            family,
        ))
    }

    /// The id-only registry-of-record payload for the Aerodrome / Balancer
    /// builds (their build adapters return just the pool id). `None` when
    /// the address is unregistered or the family differs from `want`.
    #[must_use]
    pub fn registered_family_pool_id(
        &self,
        addr: &Address,
        want: RegisteredPoolFamily,
    ) -> Option<u64> {
        let core = self.state_arc();
        let state = core.read_at(LockSite::Orchestrator);
        let (pool_id, family) = state.registered_pool_by_address(addr)?;
        (family == want).then_some(pool_id)
    }

    /// The V4 registry-of-record payload (`pool_manager`, `pool_id`)-keyed —
    /// the already-registered fast path. PRG-2: the keyed registration gate
    /// refuses an immutable-admission pool (dynamic fee /
    /// fee-exceeds-encoder-limit) BEFORE any registry read or RPC work on
    /// the registration path, with the exact typed refusal the live
    /// registration folds.
    ///
    /// # Errors
    /// [`RefusalKind::RegisterRefused`] (the V4 admission arms) when a
    /// recorded admission verdict refuses the pool.
    pub fn registered_v4_echo(
        &self,
        pm: Address,
        pid: &[u8; 32],
    ) -> Result<Option<V4RegisteredEcho>, RefusalKind> {
        if let Some(verdict) = self
            .state_arc()
            .read_at(LockSite::Orchestrator)
            .concentrated_liquidity()
            .admission_verdict(pm, pid)
        {
            let reason = match verdict {
                AdmissionVerdict::DynamicFee { fee } => RegisterRefusal::Admission {
                    failure: BuildFailure::DynamicFee,
                    hook_flags: None,
                    fee: Some(fee),
                },
                AdmissionVerdict::FeeExceedsEncoderLimit { fee } => RegisterRefusal::Admission {
                    failure: BuildFailure::HighFee,
                    hook_flags: None,
                    fee: Some(fee),
                },
            };
            return Err(RefusalKind::RegisterRefused {
                family: Some(RegisteredPoolFamily::V4),
                phase: RegistrationPhase::Register,
                reason,
            });
        }
        let Some(existing) = self
            .state_arc()
            .read_at(LockSite::Orchestrator)
            .try_registered(pm, pid)
        else {
            return Ok(None);
        };
        let key = existing.pool_key;
        Ok(Some((
            existing.pool_id,
            key.currency0.to_checksum(None),
            key.currency1.to_checksum(None),
            pm.to_checksum(None),
        )))
    }

    /// PRG-2: record one registration skip into the
    /// `degenbot.registration.skips` metric family (Rust meter), collapsing
    /// `reason` onto the closed label set ([`registration_skip_kind`]).
    pub fn record_registration_skip(&self, reason: &str) {
        crate::instruments::record(crate::instruments::CensusEvent::RegistrationSkip {
            reason: registration_skip_kind(reason),
        });
    }

    /// Assemble a V3 pool's tick map with the `Db → Chain` precedence
    /// (Decision 6 (B)) and normalize a hit into [`TickRow`] rows — the
    /// entry point the shell's `assemble_v3_tick_map` method drives with the
    /// retained `SnapshotDb` borrow (`&dyn TickMapDb`; no `BotState` guard
    /// is held across the read, and the GIL release stays at the shell).
    ///
    /// # Errors
    /// [`TickMapAssemblyError`] from the Db or Chain arm (Decision 8 (A) —
    /// loud error over silent degrade).
    pub fn assemble_v3_tick_map(
        db: Option<&dyn degenbot_db::snapshot::TickMapDb>,
        address: Address,
        tick: i32,
        tick_spacing: i32,
        block: u64,
        chain: Option<&dyn TickBootstrapRpc>,
    ) -> Result<Option<(Vec<TickRow>, PoolTickCoverage)>, TickMapAssemblyError> {
        let hit = degenbot_substrate::tick_assembly::assemble_v3_tick_map(
            db,
            address,
            tick,
            tick_spacing,
            block,
            chain,
        )?;
        Ok(hit.map(|(ticks, coverage)| (tick_rows(&ticks), coverage)))
    }

    /// Assemble a V4 pool's tick map — the V4 twin of
    /// [`Self::assemble_v3_tick_map`] (the Chain arm targets the
    /// `StateView` contract, NOT the `PoolManager`).
    ///
    /// # Errors
    /// [`TickMapAssemblyError`] as the V3 twin.
    #[expect(clippy::too_many_arguments)]
    pub fn assemble_v4_tick_map(
        db: Option<&dyn degenbot_db::snapshot::TickMapDb>,
        pool_manager: Address,
        state_view: Address,
        pool_id: [u8; 32],
        tick: i32,
        tick_spacing: i32,
        block: u64,
        chain: Option<&dyn TickBootstrapRpc>,
    ) -> Result<Option<(Vec<TickRow>, PoolTickCoverage)>, TickMapAssemblyError> {
        let hit = degenbot_substrate::tick_assembly::assemble_v4_tick_map(
            db,
            pool_manager,
            state_view,
            pool_id,
            tick,
            tick_spacing,
            block,
            chain,
        )?;
        Ok(hit.map(|(ticks, coverage)| (tick_rows(&ticks), coverage)))
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::bot_core::{V4PoolKey, V4_DYNAMIC_FEE_FLAG};
    use alloy::primitives::aliases::U112;

    /// The V2 door request the fixtures register through.
    fn v2_request(address: Address, token0: Address, factory: Address) -> PoolRegistration {
        PoolRegistration::V2(Box::new(RegisterV2PoolParams {
            address,
            token0,
            token1: Address::from([0x02u8; 20]),
            reserve0: U112::from(1000),
            reserve1: U112::from(2000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory,
            update_block: 10,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            ..Default::default()
        }))
    }

    /// The V3 door request the fixtures register through (the seeded variant
    /// carries `tick_data`; the empty map is the unseeded shape).
    fn v3_request(
        address: Address,
        tick_data: hashbrown::HashMap<i32, TickInfo>,
    ) -> PoolRegistration {
        PoolRegistration::V3(Box::new(RegisterV3PoolParams {
            address,
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            fee: 3_000,
            tick_spacing: 60,
            factory: Address::from([0x33u8; 20]),
            sqrt_price_x96: alloy::primitives::U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 100,
            coverage: PoolTickCoverage::Sparse,
            slot_layout: ConcentratedLiquiditySlotLayout::UniswapV3,
            ..Default::default()
        }))
    }

    /// Register a fixture V2 pool over `address` with `factory` (defaulting
    /// to a factory that is NOT in the shipped deployments JSON — the CREATE2
    /// verify is skipped, the manual/ad-hoc path).
    fn register_v2(bot: &Bot, address: Address, factory: Address) -> Result<u64, RefusalKind> {
        bot.register(v2_request(address, Address::from([0x01u8; 20]), factory))
    }

    /// A non-JSON factory (CREATE2 verify skipped).
    fn adhoc_factory() -> Address {
        Address::from([0x33u8; 20])
    }

    /// The facade `register_v2_pool` resolves the deployer/init-hash, assembles
    /// the params, and registers under ONE core write — the shell only parsed
    /// args. A non-JSON factory skips the CREATE2 verify and registers.
    #[test]
    fn register_v2_pool_through_the_facade_registers_and_journals() {
        let bot = Bot::new(1);
        let pool_id = register_v2(&bot, Address::from([0x11u8; 20]), adhoc_factory())
            .expect("non-JSON factory registers");
        assert!(bot.has_pool(pool_id));
        assert_eq!(bot.pool_count(), 1);
        // The V2 genesis delta is journaled (the family contract).
        assert_eq!(bot.v2_journal_len(pool_id), 1);
    }

    /// A shipped `(chain, factory)` row enforces the CREATE2 address: a
    /// declared address that does not match the recomputed one is refused
    /// with the typed `Create2` arm BEFORE any registry write.
    #[test]
    fn register_v2_pool_refuses_a_create2_mismatch() {
        let bot = Bot::new(1);
        let factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
            .parse::<Address>()
            .unwrap();
        let err =
            register_v2(&bot, Address::from([0x99u8; 20]), factory).expect_err("CREATE2 mismatch");
        assert!(
            matches!(err, RefusalKind::IdentityMismatch { .. }),
            "{err:?}"
        );
        assert_eq!(bot.pool_count(), 0, "nothing registered on refusal");
    }

    /// Get-or-register, hit arm: a same-address + same-family + same-identity
    /// re-registration answers the EXISTING pool id and creates no second pool
    /// (the register entry is now the GET half too).
    #[test]
    fn register_v2_pool_get_or_register_returns_the_existing_id() {
        let bot = Bot::new(1);
        let addr = Address::from([0x11u8; 20]);
        let first = register_v2(&bot, addr, adhoc_factory()).expect("first registration");
        let second =
            register_v2(&bot, addr, adhoc_factory()).expect("a same-identity re-registration");
        assert_eq!(first, second, "the existing handle answers");
        assert_eq!(bot.pool_count(), 1, "no second pool was created");
    }

    /// Get-or-register, wire-drift arm: a same-address + same-family entry with
    /// a DISAGREEING identity still refuses loudly with the typed
    /// `AlreadyRegistered`.
    #[test]
    fn register_v2_pool_same_address_different_identity_refuses_loudly() {
        let bot = Bot::new(1);
        let addr = Address::from([0x11u8; 20]);
        register_v2(&bot, addr, adhoc_factory()).expect("first registration");
        // Same address, DIFFERENT token0 — the wire-drift class.
        let err = bot
            .register(v2_request(
                addr,
                Address::from([0x09u8; 20]),
                adhoc_factory(),
            ))
            .expect_err("a same-family identity mismatch is a loud refusal");
        assert!(
            matches!(
                err,
                RefusalKind::RegisterRefused {
                    reason: crate::bot_core::RegisterRefusal::AlreadyRegistered { .. },
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(bot.pool_count(), 1, "the conflict created nothing new");
    }

    /// Get-or-register, family-guard arm: an address registered under a
    /// DIFFERENT family is not an answer — the collision stays the loud typed
    /// refusal.
    #[test]
    fn register_v3_pool_on_a_registered_v2_address_refuses_the_family_collision() {
        let bot = Bot::new(1);
        let addr = Address::from([0x11u8; 20]);
        register_v2(&bot, addr, adhoc_factory()).expect("first registration");
        let err = bot
            .register(v3_request(addr, hashbrown::HashMap::new()))
            .expect_err("a different-family address is not an answer");
        assert!(
            matches!(
                err,
                RefusalKind::RegisterRefused {
                    reason: crate::bot_core::RegisterRefusal::AlreadyRegistered { .. },
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// Get-or-register for the balance-vector families: a same-identity
    /// re-registration reuses the existing id instead of tripping the state
    /// insert's already-registered panic.
    #[test]
    fn register_curve_pool_get_or_register_reuses_the_existing_id() {
        let bot = Bot::new(1);
        let params = curve_params(Address::from([0x71u8; 20]));
        let first = bot
            .register(PoolRegistration::Curve(Box::new(params.clone())))
            .expect("curve registers");
        let second = bot
            .register(PoolRegistration::Curve(Box::new(params)))
            .expect("curve reuses");
        assert_eq!(first, second, "the existing Curve handle answers");
        assert_eq!(bot.pool_count(), 1, "no second Curve pool was created");
    }

    /// A minimal in-spec Curve params fixture (two coins, matching balances).
    fn curve_params(address: Address) -> RegisterCurvePoolParams {
        RegisterCurvePoolParams {
            address,
            tokens: vec![Address::from([0x01u8; 20]), Address::from([0x02u8; 20])],
            a_coefficient: 200,
            a_precision: 100,
            fee: 4_000_000,
            admin_fee: 0,
            rate_multipliers: vec![
                alloy::primitives::U256::from(1u8),
                alloy::primitives::U256::from(1u8),
            ],
            balances: vec![
                alloy::primitives::U256::from(1_000u64),
                alloy::primitives::U256::from(1_000u64),
            ],
            update_block: 5,
            swap_style: 0,
            lending_rate_style: 0,
            d_variant: 0,
            y_variant: 0,
            yd_variant: 0,
            base_pool: None,
            initial_a_coefficient: None,
            future_a_coefficient: None,
            initial_a_coefficient_time: None,
            future_a_coefficient_time: None,
            create_timestamp: None,
            fee_gamma: None,
            mid_fee: None,
            offpeg_fee_multiplier: None,
            out_fee: None,
            gamma: None,
            lp_token: None,
            use_lending: Vec::new(),
            precision_multipliers: Vec::new(),
            tokens_underlying: None,
            metapool_rate_style: 1,
            metapool_underlying_style: 1,
            data_provider: None,
        }
    }

    /// The facade `register_v3_pool` verifies, resolves deployer/init-hash,
    /// and registers with the seeded tick data inline (ADR-006 rolling-start
    /// race closure) — the shell only parsed args.
    #[test]
    fn register_v3_pool_through_the_facade_registers_seeded() {
        let bot = Bot::new(1);
        let mut tick_data = hashbrown::HashMap::new();
        tick_data.insert(
            0,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(1_000u64),
                liquidity_net: 10,
                block: 99,
            },
        );
        let pool_id = bot
            .register(v3_request(Address::from([0x22u8; 20]), tick_data))
            .expect("non-JSON factory registers");
        assert!(bot.has_pool(pool_id));
    }

    /// The registry-of-record payload shaping matches the registered entry:
    /// checksummed identity + family/variant strings, `None` off-family or
    /// unregistered (the pre-check contract the single-flight peers rely on).
    #[test]
    fn registered_v2_v3_payloads_shape_the_registered_identity() {
        let bot = Bot::new(1);
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_id =
            register_v2(&bot, v2_addr, adhoc_factory()).expect("test setup: V2 registration");
        let v3_addr = Address::from([0x22u8; 20]);
        let v3_id = bot
            .register(v3_request(v3_addr, hashbrown::HashMap::new()))
            .expect("test setup: V3 registration");

        let (pid, t0, t1, addr, variant) = bot
            .registered_v2_payload(&v2_addr)
            .expect("registered V2 answers its payload");
        assert_eq!(pid, v2_id);
        assert_eq!(
            t0.to_lowercase(),
            "0x0101010101010101010101010101010101010101"
        );
        assert_eq!(
            t1.to_lowercase(),
            "0x0202020202020202020202020202020202020202"
        );
        assert_eq!(addr.to_lowercase(), format!("{v2_addr}").to_lowercase());
        assert_eq!(variant, "uniswap-v2");

        // Wrong family: a V2 address answers None on the V3 read.
        assert!(bot.registered_v3_payload(&v2_addr, 1).is_none());
        assert!(bot.registered_v2_payload(&v3_addr).is_none());
        assert!(bot
            .registered_v2_payload(&Address::from([0xEE; 20]))
            .is_none());

        // The V3 family string falls back to "uniswap-v3" for a non-JSON
        // factory (the builder's return-surface convention).
        let (pid3, .., family) = bot
            .registered_v3_payload(&v3_addr, 1)
            .expect("registered V3 answers its payload");
        assert_eq!(pid3, v3_id);
        assert_eq!(family, "uniswap-v3");
    }

    /// `registered_family_pool_id` answers only the wanted family — the
    /// Aerodrome/Balancer build pre-check contract.
    #[test]
    fn registered_family_pool_id_checks_the_family() {
        let bot = Bot::new(8453);
        let addr = Address::from([0x44u8; 20]);
        let pool_id = bot
            .register(PoolRegistration::AerodromeV2(Box::new(
                RegisterAerodromeV2PoolParams {
                    address: addr,
                    token0: Address::from([0x01u8; 20]),
                    token1: Address::from([0x02u8; 20]),
                    factory: Address::from([0x33u8; 20]),
                    variant: degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile,
                    stable: false,
                    fee: (30, 10_000),
                    token0_decimals: 18,
                    token1_decimals: 18,
                    reserve0: U112::from(1000),
                    reserve1: U112::from(2000),
                    update_block: 10,
                },
            )))
            .expect("non-JSON factory registers (CREATE2 verify skipped)");
        assert_eq!(
            bot.registered_family_pool_id(&addr, RegisteredPoolFamily::AerodromeV2),
            Some(pool_id)
        );
        assert_eq!(
            bot.registered_family_pool_id(&addr, RegisteredPoolFamily::BalancerWeighted),
            None,
            "wrong family answers None"
        );
    }

    /// PRG-2: the keyed immutable admission verdict is consulted BEFORE the
    /// registry read on the already-registered fast path. A dynamic-fee V4
    /// pool is refused at registration (recording the verdict); the payload
    /// read then surfaces the SAME typed refusal pre-registry.
    #[test]
    fn registered_v4_payload_consults_the_gate_before_the_registry() {
        let bot = Bot::new(8453);
        let pm = Address::from([0x50u8; 20]);
        let pid = [0xABu8; 32];
        let dynamic_fee_params = v4_params(pm, pid, V4_DYNAMIC_FEE_FLAG, Address::ZERO);
        let err = bot
            .register(PoolRegistration::V4(Box::new(dynamic_fee_params)))
            .expect_err("dynamic fee is admission-refused");
        assert!(
            matches!(
                err,
                RefusalKind::RegisterRefused {
                    reason: RegisterRefusal::Admission {
                        failure: BuildFailure::DynamicFee,
                        ..
                    },
                    ..
                }
            ),
            "{err:?}"
        );

        // The gate verdict (recorded at refusal) is what the echo read
        // answers — before (and independent of) any registry entry.
        let gate_err = bot
            .registered_v4_echo(pm, &pid)
            .expect_err("the recorded verdict refuses the fast path too");
        assert!(matches!(
            gate_err,
            RefusalKind::RegisterRefused {
                reason: RegisterRefusal::Admission {
                    failure: BuildFailure::DynamicFee,
                    ..
                },
                ..
            }
        ));
    }

    /// The registered V4 echo shapes the identity from the core's
    /// immutable pool key — the ONE construction verb's GET arm answer.
    #[test]
    fn registered_v4_echo_shapes_the_registered_identity() {
        let bot = Bot::new(8453);
        let pm = Address::from([0x51u8; 20]);
        let pid = [0xCDu8; 32];
        let pool_id = bot
            .register(PoolRegistration::V4(Box::new(v4_params(
                pm,
                pid,
                3_000,
                Address::ZERO,
            ))))
            .expect("a static-fee V4 pool registers");
        let echo = bot
            .registered_v4_echo(pm, &pid)
            .expect("no verdict refusal")
            .expect("registered → Some");
        assert_eq!(echo.0, pool_id);
        assert_eq!(
            echo.1.to_lowercase(),
            "0x0101010101010101010101010101010101010101"
        );
        assert_eq!(
            echo.2.to_lowercase(),
            "0x0202020202020202020202020202020202020202"
        );
        assert_eq!(echo.3.to_lowercase(), format!("{pm}").to_lowercase());

        // An unregistered (manager, id) answers Ok(None) — the miss the
        // single-flight lead path builds on.
        assert!(bot
            .registered_v4_echo(pm, &[0u8; 32])
            .expect("no verdict for an unregistered id")
            .is_none());
    }

    /// The facade `register_token` / `register_v4_state_view` /
    /// `unregister_pool` are the same lock-inside-core writes the shell
    /// performed, with the V2/V3 address-keyed unregister contract.
    #[test]
    fn token_state_view_and_unregister_write_through_the_facade() {
        let bot = Bot::new(1);
        let token = Address::from([0x77u8; 20]);
        bot.register_token(token, "T".to_string(), "T".to_string(), 18, 1);
        assert!(bot.has_token(&token));

        let pm = Address::from([0x50u8; 20]);
        let sv = Address::from([0x60u8; 20]);
        bot.register_state_view(pm, sv);

        let v2_addr = Address::from([0x11u8; 20]);
        let pool_id =
            register_v2(&bot, v2_addr, adhoc_factory()).expect("test setup: V2 registration");
        assert!(
            bot.unregister_pool(PoolLookup::from_address(v2_addr)),
            "a registered pool unregisters"
        );
        assert!(!bot.has_pool(pool_id));
        assert!(
            !bot.unregister_pool(PoolLookup::from_address(v2_addr)),
            "an unregistered address is a silent no-op"
        );
    }

    /// The Curve/Balancer registration facades are the same lock-inside-core
    /// inserts (the shell assembles the params; the facade owns the write).
    #[test]
    fn curve_and_balancer_facades_register_under_one_core_write() {
        let bot = Bot::new(1);
        let curve_id = bot
            .register(PoolRegistration::Curve(Box::new(RegisterCurvePoolParams {
                address: Address::from([0x71u8; 20]),
                tokens: vec![Address::from([0x01u8; 20]), Address::from([0x02u8; 20])],
                a_coefficient: 200,
                a_precision: 100,
                fee: 4_000_000,
                admin_fee: 0,
                rate_multipliers: vec![
                    alloy::primitives::U256::from(1u8),
                    alloy::primitives::U256::from(1u8),
                ],
                balances: vec![
                    alloy::primitives::U256::from(1_000u64),
                    alloy::primitives::U256::from(1_000u64),
                ],
                update_block: 5,
                swap_style: 0,
                lending_rate_style: 0,
                d_variant: 0,
                y_variant: 0,
                yd_variant: 0,
                base_pool: None,
                initial_a_coefficient: None,
                future_a_coefficient: None,
                initial_a_coefficient_time: None,
                future_a_coefficient_time: None,
                create_timestamp: None,
                fee_gamma: None,
                mid_fee: None,
                offpeg_fee_multiplier: None,
                out_fee: None,
                gamma: None,
                lp_token: None,
                use_lending: Vec::new(),
                precision_multipliers: Vec::new(),
                tokens_underlying: None,
                metapool_rate_style: 1,
                metapool_underlying_style: 1,
                data_provider: None,
            })))
            .expect("curve registers");
        assert!(bot.has_pool(curve_id));

        let weighted_id = bot
            .register(PoolRegistration::BalancerWeighted(Box::new(
                RegisterBalancerWeightedPoolParams {
                    address: Address::from([0x72u8; 20]),
                    vault: Address::from([0xBAu8; 20]),
                    pool_id: [0x07u8; 32],
                    tokens: vec![Address::from([0x01u8; 20]), Address::from([0x02u8; 20])],
                    weights: vec![
                        alloy::primitives::U256::from(1u8),
                        alloy::primitives::U256::from(1u8),
                    ],
                    scaling_factors: vec![
                        alloy::primitives::U256::from(1u8),
                        alloy::primitives::U256::from(1u8),
                    ],
                    swap_fee: 0,
                    pow_version: 1,
                    balances: vec![
                        alloy::primitives::U256::from(1_000u64),
                        alloy::primitives::U256::from(1_000u64),
                    ],
                    update_block: 5,
                },
            )))
            .expect("weighted registers");
        assert!(bot.has_pool(weighted_id));

        let stable_id = bot
            .register(PoolRegistration::BalancerStable(Box::new(
                RegisterBalancerStablePoolParams {
                    address: Address::from([0x73u8; 20]),
                    vault: Address::from([0xBAu8; 20]),
                    pool_id: [0x08u8; 32],
                    tokens: vec![Address::from([0x01u8; 20]), Address::from([0x02u8; 20])],
                    amp: 100,
                    scaling_factors: vec![
                        alloy::primitives::U256::from(1u8),
                        alloy::primitives::U256::from(1u8),
                    ],
                    swap_fee: 0,
                    bpt_idx: None,
                    invariant_version: 1,
                    balances: vec![
                        alloy::primitives::U256::from(1_000u64),
                        alloy::primitives::U256::from(1_000u64),
                    ],
                    update_block: 5,
                    rate_provider: None,
                },
            )))
            .expect("stable registers");
        assert!(bot.has_pool(stable_id));
    }

    /// `resolve_v4_identity` requires an attached `ConstructionIo` — the
    /// typed `NoConstructionIo` arm the shell maps to its historical
    /// "<method>: no `ConstructionIo` attached" `RuntimeError` — the V4
    /// twin retired with the identity-verb fold (the door's io requirement
    /// is pinned at the seam's V4 flight test).
    /// The assemble entry points keep the Db→Chain precedence contract: with
    /// no Db handle and no Chain transport, a miss is `Ok(None)` (the
    /// cold-start path the shell's `None` return preserves).
    #[test]
    fn assemble_tick_maps_miss_without_db_or_chain() {
        let v3 = Bot::assemble_v3_tick_map(None, Address::from([0x22u8; 20]), 0, 10, 0, None)
            .expect("a miss is not an error");
        assert!(v3.is_none(), "cold-start (no Db, no Chain) → miss");

        let v4 = Bot::assemble_v4_tick_map(
            None,
            Address::from([0xBBu8; 20]),
            Address::from([0x7Fu8; 20]),
            [0xCCu8; 32],
            0,
            10,
            0,
            None,
        )
        .expect("a miss is not an error");
        assert!(v4.is_none());
    }

    /// The row-normalization half of the shell's `build_tick_rows_py`:
    /// `TickInfo` narrows to the `(tick, gross: u128, net: i128, block: u64)`
    /// dict rows in the map's own iteration order.
    #[test]
    fn tick_rows_normalize_tickinfo() {
        let mut ticks = hashbrown::HashMap::new();
        ticks.insert(
            10,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5u64),
                liquidity_net: -3,
                block: 7,
            },
        );
        ticks.insert(
            -20,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::MAX,
                liquidity_net: 0,
                block: 0,
            },
        );
        let rows = super::tick_rows(&ticks);
        assert_eq!(rows.len(), 2);
        for (tick, gross, net, block) in rows {
            if tick == 10 {
                assert_eq!((gross, net, block), (5u128, -3i128, 7u64));
            } else {
                assert_eq!(tick, -20, "unexpected tick {tick}");
                assert_eq!(gross, alloy::primitives::U128::MAX.to::<u128>());
                assert_eq!((net, block), (0i128, 0u64));
            }
        }
    }

    /// The skip-label collapse onto the closed cardinality set (the Python
    /// `SkipGate` memo's Rust twin) — every historical reason class maps to
    /// its label, and unknown reasons fall to `other`.
    #[test]
    fn registration_skip_kind_collapses_onto_the_closed_label_set() {
        for (reason, label) in [
            ("v4-hook-rejected", "v4-admission"),
            ("v4-dynamic-fee-rejected", "v4-admission"),
            ("v4-high-fee-rejected", "v4-admission"),
            ("path-cap", "path-cap"),
            ("dup", "dup"),
            ("direction-mismatch", "candidate-invalid"),
            ("v4-no-hash", "candidate-invalid"),
            ("unknown-pool-type", "candidate-invalid"),
            ("engine-reject", "engine-reject"),
            ("register-fail-already", "register-fail"),
            ("build-v2: rpc down", "pool-build-error"),
            ("build-v3: timeout", "pool-build-error"),
            ("build-v4: decode", "pool-build-error"),
            ("something-new", "other"),
        ] {
            assert_eq!(super::registration_skip_kind(reason), label, "{reason}");
        }
    }

    /// The intake rendering of an unparseable address.
    #[expect(clippy::panic)] // the malformed-input invariant is by construction; the test helper asserts it loudly
    fn malformed_detail(err: &RefusalKind) -> String {
        match err {
            RefusalKind::MalformedInput { detail, .. } => detail.clone(),
            other => panic!("an unparseable entry is malformed input: {other:?}"),
        }
    }

    /// The string-list address parse keeps the shell's historical message
    /// vocabulary byte-identically (`Invalid address '<input>': <source>`).
    #[test]
    fn parse_address_list_keeps_the_historical_message_vocabulary() {
        let parsed =
            super::parse_address_list(&["0x0101010101010101010101010101010101010101".to_string()])
                .expect("a valid list parses");
        assert_eq!(parsed, vec![Address::from([0x01u8; 20])]);

        let err = super::parse_address_list(&["zz".to_string()]).expect_err("invalid entry");
        let message = malformed_detail(&err);
        assert!(
            message.starts_with("Invalid address 'zz': "),
            "historical vocabulary preserved: {message}"
        );
        let single = super::parse_address_str("zz").expect_err("invalid single");
        assert_eq!(malformed_detail(&single), message);
    }

    /// The PRG-3 stance gate: the typed refusal carries the historical
    /// message the Python driver matches on.
    #[test]
    fn the_fleet_stance_gate_refusal_message_is_stable() {
        let err = RegistrationIntakeNotHosted;
        assert_eq!(
            err.to_string(),
            "registration intake is not fleet-hosted (fleet.stance != fleet)"
        );
    }

    /// A V4 registration fixture: pool key over the fixture tokens with the
    /// given fee and hook.
    fn v4_params(pm: Address, pid: [u8; 32], fee: u32, hook: Address) -> RegisterV4PoolParams {
        RegisterV4PoolParams {
            pool_manager: pm,
            pool_id: pid,
            pool_key: V4PoolKey {
                currency0: Address::from([0x01u8; 20]),
                currency1: Address::from([0x02u8; 20]),
                fee,
                tick_spacing: 60,
                hooks: hook,
            },
            hook_flags: crate::bot_core::pool_builder::builder::derive_hook_flags(hook),
            protocol_fee: 0,
            sqrt_price_x96: alloy::primitives::U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: hashbrown::HashMap::new(),
            update_block: 100,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            known_bitmap_words: Vec::new(),
        }
    }
}
