//! The registration cluster on the `Bot` facade (ergo ND7GW7 — cycle 3 of
//! the PyBot shell deepening). Moved from the PyO3 shell
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
use std::sync::Arc;

use alloy::primitives::aliases::U112;
use alloy::primitives::{Address, U256};
use degenbot_pools::tick_fetch::{TickBootstrapRpc, TickWordFetcher};
use degenbot_substrate::registration_gate::AdmissionVerdict;
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::tick_assembly::TickMapAssemblyError;
use degenbot_uniswap::deployments::AddressMismatch;
use degenbot_uniswap::dex_identity::DexVariant;

use super::bot::Bot;
use crate::bot_core::pool_builder::builder::{self, PoolBuilderError};
use crate::bot_core::{
    ClSlotLayout, PoolTickCoverage, RegisterAerodromeV2PoolParams,
    RegisterBalancerStablePoolParams, RegisterBalancerWeightedPoolParams, RegisterCurvePoolParams,
    RegisterV2PoolError, RegisterV2PoolParams, RegisterV3PoolError, RegisterV3PoolParams,
    RegisterV4PoolError, RegisterV4PoolParams, RegisteredPoolFamily, TickInfo,
};

/// Error from [`Bot::register_v2_pool`]: the CREATE2 address verification
/// (`Create2`) is distinguishable from the registration refusal (`Register`)
/// so the PyO3 shell preserves its two historical surfaces byte-identically
/// (the bare-address-mismatch `ValueError` and the `PoolRegistrationError`
/// hierarchy map).
#[derive(Debug)]
pub enum V2RegistrationError {
    /// The recomputed CREATE2 address differs from the declared address (a
    /// shipped `(chain, factory)` row only — otherwise the check is skipped).
    Create2(AddressMismatch),
    /// `BotState::register_v2_pool` refused (already registered / spec).
    Register(RegisterV2PoolError),
}

/// Error from [`Bot::register_v3_pool`] — the V3 twin of
/// [`V2RegistrationError`] (the V3 salt includes the fee).
#[derive(Debug)]
pub enum V3RegistrationError {
    /// The recomputed CREATE2 address differs from the declared address.
    Create2(AddressMismatch),
    /// `BotState::register_v3_pool` refused (already registered / spec).
    Register(RegisterV3PoolError),
}

/// Error from [`Bot::register_aerodrome_pool`]: only the EIP-1167
/// deployer/implementation verification can refuse — the registration insert
/// itself is infallible.
#[derive(Debug)]
pub enum AerodromeRegistrationError {
    /// The recomputed EIP-1167 address differs from the declared address.
    Create2(AddressMismatch),
}

/// Error from [`Bot::resolve_v4_identity`].
#[derive(Debug)]
pub enum ResolveV4IdentityError {
    /// No `ConstructionIo` was attached to the bot (the shell's
    /// `"<method>: no ConstructionIo attached (requires an alloy provider)"`
    /// `RuntimeError` — the shell owns the method-prefixed message).
    NoConstructionIo,
    /// The DB two-step / identity resolution failed (a builder RPC, decode,
    /// DB, or `MissingIdentity` failure).
    Builder(PoolBuilderError),
}

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

/// A list entry was not a parseable address. `Display` carries the
/// historical shell message (`Invalid address '<input>': <source>`)
/// byte-identically.
#[derive(Debug)]
pub enum ParseAddressError {
    /// The input string was rejected by the `Address` parser.
    InvalidAddress {
        /// The rejected input (verbatim, for the message).
        input: String,
        /// The underlying `Address` parse failure (the parser's own error
        /// type, boxed so the vocabulary does not pin a hex-crate version).
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl fmt::Display for ParseAddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAddress { input, source } => {
                write!(f, "Invalid address '{input}': {source}")
            }
        }
    }
}

impl std::error::Error for ParseAddressError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidAddress { source, .. } => Some(source.as_ref()),
        }
    }
}

/// One normalized tick-map row: `(tick, liquidity_gross: u128,
/// liquidity_net: i128, block: u64)` — the dict-entry shape the shell's
/// `build_tick_rows_py` turns into a Python `{tick: (gross, net, block)}`.
pub type TickRow = (i32, u128, i128, u64);

/// The already-registered V4 fast-path payload: `(pool_id, coverage,
/// currency0, currency1, pool_manager, fee, tick_spacing, hook_flags,
/// pool_id_hex, protocol_fee, lp_fee)` — the builder's return surface, read
/// from the registry of record (identity from the core's immutable pool
/// key, protocol fee from the state machine, coverage as recorded).
#[expect(clippy::type_complexity)]
pub type V4RegisteredPayload = (
    u64,
    String,
    String,
    String,
    String,
    u32,
    i32,
    u16,
    String,
    u32,
    u32,
);

/// Parse one address string with the shell's historical message vocabulary.
///
/// # Errors
/// [`ParseAddressError::InvalidAddress`] when `s` is not a parseable address.
pub fn parse_address_str(s: &str) -> Result<Address, ParseAddressError> {
    s.parse::<Address>()
        .map_err(|source| ParseAddressError::InvalidAddress {
            input: s.to_string(),
            source: Box::new(source),
        })
}

/// Parse a list of address strings (the shell extracts the `Vec<String>`
/// from the Python list, keeping its per-item "token address must be a str"
/// surface and per-item error precedence).
///
/// # Errors
/// [`ParseAddressError::InvalidAddress`] for the first unparseable entry.
pub fn parse_address_list(items: &[String]) -> Result<Vec<Address>, ParseAddressError> {
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
    // === Registration cluster (moved from the PyO3 shell, ergo ND7GW7): ===
    // === the shell keeps only arg parsing + PyErr mapping; the CREATE2    ===
    // === verification, deployer/init-hash resolution, params assembly,   ===
    // === and the lock discipline live here so the standalone-Rust path   ===
    // === gets the same registrations.                                    ===

    /// Register a V2 pool by contract address. Runs the JSON-sourced CREATE2
    /// verification (Fork A) — skipped when `(chain, factory)` is not in the
    /// shipped deployments JSON, preserving the manual/ad-hoc path — then
    /// resolves the deployer + init hash onto the identity and registers
    /// under ONE core write guard.
    ///
    /// # Errors
    /// [`V2RegistrationError::Create2`] on a verified CREATE2 mismatch,
    /// [`V2RegistrationError::Register`] when the registry refuses (already
    /// registered / spec violation).
    #[expect(clippy::too_many_arguments)]
    pub fn register_v2_pool(
        &self,
        address: Address,
        token0: Address,
        token1: Address,
        reserve0: U112,
        reserve1: U112,
        fee_token0: (u64, u64),
        fee_token1: (u64, u64),
        factory: Address,
        update_block: u64,
        variant: DexVariant,
        stable_swap: bool,
        fee_denominator: Option<u64>,
    ) -> Result<u64, V2RegistrationError> {
        degenbot_uniswap::deployments::verify_v2_pool_address(
            self.chain_id(),
            factory,
            address,
            token0,
            token1,
        )
        .map_err(V2RegistrationError::Create2)?;
        let deployer = degenbot_uniswap::deployments::resolve_deployer(self.chain_id(), factory);
        let init_hash =
            degenbot_uniswap::deployments::resolve_v2_init_hash(self.chain_id(), factory);
        let params = RegisterV2PoolParams {
            address,
            token0,
            token1,
            reserve0,
            reserve1,
            fee_token0,
            fee_token1,
            factory,
            deployer,
            init_hash,
            update_block,
            variant,
            stable_swap,
            fee_denominator,
        };
        self.state_arc()
            .write_at(LockSite::Core)
            .register_v2_pool(&params)
            .map_err(V2RegistrationError::Register)
    }

    /// Register a V3 pool by contract address, with the seeded tick data
    /// inline (ADR-006 rolling-start race closure: the pool is never visible
    /// to the pump in an unseeded state). An explicit `Some` slot layout
    /// wins; `None` defers to the deployment table (the PancakeSwap factory
    /// check), else the canonical Uniswap layout.
    ///
    /// # Errors
    /// [`V3RegistrationError::Create2`] on a verified CREATE2 mismatch,
    /// [`V3RegistrationError::Register`] when the registry refuses.
    #[expect(clippy::too_many_arguments)]
    pub fn register_v3_pool(
        &self,
        address: Address,
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: i32,
        factory: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        tick_data: hashbrown::HashMap<i32, TickInfo>,
        update_block: u64,
        coverage: PoolTickCoverage,
        fetcher: Option<Arc<dyn TickWordFetcher>>,
        tick_data_block: Option<u64>,
        slot_layout: Option<ClSlotLayout>,
    ) -> Result<u64, V3RegistrationError> {
        let slot_layout = slot_layout.unwrap_or_else(|| {
            if degenbot_uniswap::deployments::is_pancakeswap_v3_factory(self.chain_id(), factory) {
                ClSlotLayout::PancakeV3
            } else {
                ClSlotLayout::UniswapV3
            }
        });
        degenbot_uniswap::deployments::verify_v3_pool_address(
            self.chain_id(),
            factory,
            address,
            token0,
            token1,
            fee,
        )
        .map_err(V3RegistrationError::Create2)?;
        let deployer = degenbot_uniswap::deployments::resolve_deployer(self.chain_id(), factory);
        let init_hash =
            degenbot_uniswap::deployments::resolve_v3_init_hash(self.chain_id(), factory);
        let params = RegisterV3PoolParams {
            address,
            token0,
            token1,
            fee,
            tick_spacing,
            factory,
            deployer,
            init_hash,
            sqrt_price_x96,
            liquidity,
            tick,
            tick_data,
            update_block,
            tick_data_block,
            coverage,
            fetcher,
            slot_layout,
        };
        self.state_arc()
            .write_at(LockSite::Core)
            .register_v3_pool(&params)
            .map_err(V3RegistrationError::Register)
    }

    /// Register a V4 pool by `(pool_manager, pool_id)` — the params (with
    /// the FULL hook address riding the pool key and the derived 16-bit
    /// mask) are assembled by the caller; the admission floor (dynamic fee /
    /// fee-exceeds-encoder-limit, recording the PRG-2 keyed verdicts) and
    /// the seeded-inline tick data discipline live in
    /// `BotState::register_v4_pool`, driven here under ONE core write guard.
    ///
    /// # Errors
    /// [`RegisterV4PoolError`] (hooked/dynamic-fee/high-fee admission,
    /// already registered, spec violation).
    pub fn register_v4_pool(
        &self,
        params: &RegisterV4PoolParams,
    ) -> Result<u64, RegisterV4PoolError> {
        self.state_arc()
            .write_at(LockSite::Core)
            .register_v4_pool(params)
    }

    /// Register a Curve `StableSwap` pool — the params are assembled by the
    /// caller; the insert runs under ONE core write guard.
    pub fn register_curve_pool(&self, params: &RegisterCurvePoolParams) -> u64 {
        self.state_arc()
            .write_at(LockSite::Core)
            .register_curve_pool(params)
    }

    /// Register a Balancer V2 weighted pool — see [`Self::register_curve_pool`].
    pub fn register_balancer_weighted_pool(
        &self,
        params: &RegisterBalancerWeightedPoolParams,
    ) -> u64 {
        self.state_arc()
            .write_at(LockSite::Core)
            .register_balancer_weighted_pool(params)
    }

    /// Register a Balancer V2 stable pool — see [`Self::register_curve_pool`].
    pub fn register_balancer_stable_pool(&self, params: &RegisterBalancerStablePoolParams) -> u64 {
        self.state_arc()
            .write_at(LockSite::Core)
            .register_balancer_stable_pool(params)
    }

    /// Register an Aerodrome V2 pool. Runs the JSON-sourced EIP-1167
    /// deployer/implementation verification (Fork A follow-on; skipped for
    /// non-JSON `(chain, factory)` rows), then registers under ONE core
    /// write guard.
    ///
    /// # Errors
    /// [`AerodromeRegistrationError::Create2`] on a verified mismatch.
    #[expect(clippy::too_many_arguments)]
    pub fn register_aerodrome_pool(
        &self,
        address: Address,
        token0: Address,
        token1: Address,
        factory: Address,
        variant: DexVariant,
        stable: bool,
        fee_numer: u64,
        fee_denom: u64,
        token0_decimals: u8,
        token1_decimals: u8,
        reserve0: U112,
        reserve1: U112,
        update_block: u64,
    ) -> Result<u64, AerodromeRegistrationError> {
        degenbot_uniswap::deployments::verify_aerodrome_v2_pool_address(
            self.chain_id(),
            factory,
            address,
            token0,
            token1,
            stable,
        )
        .map_err(AerodromeRegistrationError::Create2)?;
        let params = RegisterAerodromeV2PoolParams {
            address,
            token0,
            token1,
            factory,
            variant,
            stable,
            fee: (fee_numer, fee_denom),
            token0_decimals,
            token1_decimals,
            reserve0,
            reserve1,
            update_block,
        };
        Ok(self
            .state_arc()
            .write_at(LockSite::Core)
            .register_aerodrome_pool(&params))
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
            .write_at(LockSite::Core)
            .register_token(address, name, symbol, decimals, chain_id);
    }

    /// Seed the Rust-owned V4 `StateView` registry (ADR-005 / Option 2).
    pub fn register_v4_state_view(&self, pool_manager: Address, state_view: Address) {
        self.state_arc()
            .write_at(LockSite::Core)
            .register_v4_state_view(pool_manager, state_view);
    }

    /// Unregister a V2/V3 pool by its contract address (ADR-007 U3): drops
    /// the entry, journal, index entry, and buffered V3 events. `false` when
    /// the address was never registered (silent no-op). V4 tuple-key
    /// unregister is engine-side and never routed here.
    #[must_use]
    pub fn unregister_pool(&self, address: Address) -> bool {
        self.state_arc()
            .write_at(LockSite::Core)
            .unregister_pool(address, None)
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
        let state = core.read_at(LockSite::Core);
        let (pool_id, RegisteredPoolFamily::V2) = state.registered_pool_by_address(addr)? else {
            return None;
        };
        let ident = state.get_v2_identity(pool_id)?;
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
        let state = core.read_at(LockSite::Core);
        let (pool_id, RegisteredPoolFamily::V3) = state.registered_pool_by_address(addr)? else {
            return None;
        };
        let ident = state.get_v3_identity(pool_id)?;
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
        let state = core.read_at(LockSite::Core);
        let (pool_id, family) = state.registered_pool_by_address(addr)?;
        (family == want).then_some(pool_id)
    }

    /// The V4 registry-of-record payload (`pool_manager`, `pool_id`)-keyed —
    /// the already-registered fast path. PRG-2: the keyed registration gate
    /// refuses an immutable-admission pool (dynamic fee /
    /// fee-exceeds-encoder-limit) BEFORE any registry read or RPC work on
    /// the registration path, with the exact typed error the live
    /// registration refusal raised.
    ///
    /// # Errors
    /// [`RegisterV4PoolError::DynamicFee`] / `FeeExceedsEncoderLimit` when a
    /// recorded admission verdict refuses the pool.
    #[expect(clippy::type_complexity)]
    pub fn registered_v4_payload(
        &self,
        pm: Address,
        pid: &[u8; 32],
    ) -> Result<Option<V4RegisteredPayload>, RegisterV4PoolError> {
        if let Some(verdict) = self
            .state_arc()
            .read_at(LockSite::Core)
            .admission_verdict(pm, pid)
        {
            let core_err = match verdict {
                AdmissionVerdict::DynamicFee { fee } => RegisterV4PoolError::DynamicFee { fee },
                AdmissionVerdict::FeeExceedsEncoderLimit { fee } => {
                    RegisterV4PoolError::FeeExceedsEncoderLimit { fee }
                }
            };
            return Err(core_err);
        }
        let Some(existing) = self
            .state_arc()
            .read_at(LockSite::Core)
            .try_registered_v4(pm, pid)
        else {
            return Ok(None);
        };
        let coverage_str = match existing.coverage {
            PoolTickCoverage::Tracked => "tracked",
            PoolTickCoverage::Sparse => "sparse",
        };
        let key = existing.pool_key;
        // `lp_fee` = the static pool-key fee: dynamic-fee pools are
        // admission-rejected and never registered, so they never reach this
        // branch.
        Ok(Some((
            existing.pool_id,
            coverage_str.to_string(),
            key.currency0.to_checksum(None),
            key.currency1.to_checksum(None),
            pm.to_checksum(None),
            key.fee,
            key.tick_spacing,
            // Derived mask — the driver's parity check compares it against
            // the resolve_v4_identity mask, both derived from the same hook
            // address.
            crate::bot_core::pool_builder::builder::derive_hook_flags(key.hooks),
            format!("0x{}", alloy::hex::encode(pid)),
            existing.protocol_fee,
            key.fee,
        )))
    }

    /// PRG-2: record one registration skip into the
    /// `degenbot.registration.skips` metric family (Rust meter), collapsing
    /// `reason` onto the closed label set ([`registration_skip_kind`]).
    pub fn record_registration_skip(&self, reason: &str) {
        if let Some(p) = crate::instruments::pipeline() {
            p.count_registration_skip(registration_skip_kind(reason));
        }
    }

    /// Resolve the V4 identity (currency0/1, fee, tick_spacing, hook,
    /// state_view): the DB two-step (manager → V4 row → per-FK tokens) on
    /// the attached `ConstructionIo` first, else the caller-supplied
    /// overrides. Runs the core builder's async resolution on the shared
    /// runtime (the shell detaches the GIL around this call).
    ///
    /// # Errors
    /// [`ResolveV4IdentityError::NoConstructionIo`] when no construction
    /// I/O is attached, [`ResolveV4IdentityError::Builder`] on a builder
    /// failure (a typed `MissingIdentity` when neither source is complete).
    pub fn resolve_v4_identity(
        &self,
        chain_id: u64,
        pool_manager: Address,
        pool_id: [u8; 32],
        overrides: &builder::V4PoolBuildOverrides,
    ) -> Result<builder::V4PoolBuildIdentity, ResolveV4IdentityError> {
        let io = self
            .construction_io_arc()
            .ok_or(ResolveV4IdentityError::NoConstructionIo)?;
        degenbot_core::runtime::get_runtime()
            .block_on(builder::resolve_v4_identity(
                chain_id,
                pool_manager,
                pool_id,
                overrides,
                &io,
            ))
            .map_err(ResolveV4IdentityError::Builder)
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
    #![expect(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
    use super::*;
    use crate::bot_core::{V4PoolKey, V4_DYNAMIC_FEE_FLAG};
    use alloy::primitives::aliases::U112;

    /// Register a fixture V2 pool over `address` with `factory` (defaulting
    /// to a factory that is NOT in the shipped deployments JSON — the CREATE2
    /// verify is skipped, the manual/ad-hoc path).
    fn register_v2(
        bot: &Bot,
        address: Address,
        factory: Address,
    ) -> Result<u64, V2RegistrationError> {
        bot.register_v2_pool(
            address,
            Address::from([0x01u8; 20]),
            Address::from([0x02u8; 20]),
            U112::from(1000),
            U112::from(2000),
            (997, 1000),
            (997, 1000),
            factory,
            10,
            degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            false,
            None,
        )
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
        assert!(matches!(err, V2RegistrationError::Create2(_)), "{err:?}");
        assert_eq!(bot.pool_count(), 0, "nothing registered on refusal");
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
            .register_v3_pool(
                Address::from([0x22u8; 20]),
                Address::from([0x01u8; 20]),
                Address::from([0x02u8; 20]),
                3_000,
                60,
                Address::from([0x33u8; 20]),
                alloy::primitives::U256::from(1u128) << 96,
                1_000_000,
                0,
                tick_data,
                100,
                PoolTickCoverage::Sparse,
                None,
                None,
                None,
            )
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
            .register_v3_pool(
                v3_addr,
                Address::from([0x01u8; 20]),
                Address::from([0x02u8; 20]),
                3_000,
                60,
                Address::from([0x33u8; 20]),
                alloy::primitives::U256::from(1u128) << 96,
                1_000_000,
                0,
                hashbrown::HashMap::new(),
                100,
                PoolTickCoverage::Sparse,
                None,
                None,
                None,
            )
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
            .register_aerodrome_pool(
                addr,
                Address::from([0x01u8; 20]),
                Address::from([0x02u8; 20]),
                Address::from([0x33u8; 20]),
                degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile,
                false,
                30,
                10_000,
                18,
                18,
                U112::from(1000),
                U112::from(2000),
                10,
            )
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
            .register_v4_pool(&dynamic_fee_params)
            .expect_err("dynamic fee is admission-refused");
        assert!(
            matches!(err, RegisterV4PoolError::DynamicFee { .. }),
            "{err:?}"
        );

        // The gate verdict (recorded at refusal) is what the payload read
        // answers — before (and independent of) any registry entry.
        let gate_err = bot
            .registered_v4_payload(pm, &pid)
            .expect_err("the recorded verdict refuses the fast path too");
        assert!(matches!(gate_err, RegisterV4PoolError::DynamicFee { .. }));
    }

    /// The registered V4 fast path shapes the identity from the core's
    /// immutable pool key, the protocol fee from the state machine, and the
    /// coverage as recorded — with the hook-flag mask derived from the SAME
    /// hook address the resolve path derives it from.
    #[test]
    fn registered_v4_payload_shapes_the_registered_identity() {
        let bot = Bot::new(8453);
        let pm = Address::from([0x51u8; 20]);
        let pid = [0xCDu8; 32];
        let pool_id = bot
            .register_v4_pool(&v4_params(pm, pid, 3_000, Address::ZERO))
            .expect("a static-fee V4 pool registers");
        let payload = bot
            .registered_v4_payload(pm, &pid)
            .expect("no verdict refusal")
            .expect("registered → Some");
        assert_eq!(payload.0, pool_id);
        assert_eq!(payload.1, "sparse", "coverage as registered");
        assert_eq!(
            payload.2.to_lowercase(),
            "0x0101010101010101010101010101010101010101"
        );
        assert_eq!(
            payload.3.to_lowercase(),
            "0x0202020202020202020202020202020202020202"
        );
        assert_eq!(payload.4.to_lowercase(), format!("{pm}").to_lowercase());
        assert_eq!(payload.5, 3_000);
        assert_eq!(payload.6, 60);
        assert_eq!(payload.7, 0, "no hook → zero mask");
        assert_eq!(payload.8, format!("0x{}", alloy::hex::encode(pid)));
        assert_eq!(payload.9, 0, "protocol fee as registered");
        assert_eq!(payload.10, 3_000, "lp_fee = the static pool-key fee");

        // An unregistered (manager, id) answers Ok(None) — the miss the
        // single-flight lead path builds on.
        assert!(bot
            .registered_v4_payload(pm, &[0u8; 32])
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
        bot.register_v4_state_view(pm, sv);

        let v2_addr = Address::from([0x11u8; 20]);
        let pool_id =
            register_v2(&bot, v2_addr, adhoc_factory()).expect("test setup: V2 registration");
        assert!(
            bot.unregister_pool(v2_addr),
            "a registered pool unregisters"
        );
        assert!(!bot.has_pool(pool_id));
        assert!(
            !bot.unregister_pool(v2_addr),
            "an unregistered address is a silent no-op"
        );
    }

    /// The Curve/Balancer registration facades are the same lock-inside-core
    /// inserts (the shell assembles the params; the facade owns the write).
    #[test]
    fn curve_and_balancer_facades_register_under_one_core_write() {
        let bot = Bot::new(1);
        let curve_id = bot.register_curve_pool(&RegisterCurvePoolParams {
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
        });
        assert!(bot.has_pool(curve_id));

        let weighted_id =
            bot.register_balancer_weighted_pool(&RegisterBalancerWeightedPoolParams {
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
            });
        assert!(bot.has_pool(weighted_id));

        let stable_id = bot.register_balancer_stable_pool(&RegisterBalancerStablePoolParams {
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
        });
        assert!(bot.has_pool(stable_id));
    }

    /// `resolve_v4_identity` requires an attached `ConstructionIo` — the
    /// typed `NoConstructionIo` arm the shell maps to its historical
    /// "<method>: no ConstructionIo attached" `RuntimeError`.
    #[test]
    fn resolve_v4_identity_refuses_without_construction_io() {
        let bot = Bot::new(1);
        let err = bot
            .resolve_v4_identity(
                1,
                Address::from([0x50u8; 20]),
                [0u8; 32],
                &Default::default(),
            )
            .expect_err("no ConstructionIo attached");
        assert!(
            matches!(err, ResolveV4IdentityError::NoConstructionIo),
            "{err:?}"
        );
    }

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
            match tick {
                10 => {
                    assert_eq!((gross, net, block), (5u128, -3i128, 7u64));
                }
                -20 => {
                    assert_eq!(gross, alloy::primitives::U128::MAX.to::<u128>());
                    assert_eq!((net, block), (0i128, 0u64));
                }
                other => panic!("unexpected tick {other}"),
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

    /// The string-list address parse keeps the shell's historical message
    /// vocabulary byte-identically (`Invalid address '<input>': <source>`).
    #[test]
    fn parse_address_list_keeps_the_historical_message_vocabulary() {
        let parsed =
            super::parse_address_list(&["0x0101010101010101010101010101010101010101".to_string()])
                .expect("a valid list parses");
        assert_eq!(parsed, vec![Address::from([0x01u8; 20])]);

        let err = super::parse_address_list(&["zz".to_string()]).expect_err("invalid entry");
        let message = err.to_string();
        assert!(
            message.starts_with("Invalid address 'zz': "),
            "historical vocabulary preserved: {message}"
        );
        let single = super::parse_address_str("zz").expect_err("invalid single");
        assert_eq!(single.to_string(), message);
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
        }
    }
}
