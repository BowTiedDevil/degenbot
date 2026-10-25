//! The build cluster on the `Bot` facade. Moved from the PyO3 shell
//! (`degenbot-python/src/bot/mod.rs`): the shell keeps only arg parsing +
//! `PyErr` mapping + the single-flight claim, while the construction-io
//! fetch, the builder run on the shared runtime, the identity shaping, the
//! `BotState` registration write, and the `degenbot.pool.register`
//! telemetry span live here so the standalone-Rust path gets the same
//! builds.
//!
//! Invariants preserved from the shell:
//! - Incident 2026-08-20 #2: the caller (`PyBot`) releases the GIL across
//!   the WHOLE build+register scope — each entry point here blocks on the
//!   shared runtime and takes its `BotState` guard under the caller's
//!   `py.detach`, never under a held GIL.
//! - PRG-1: the registry-of-record pre-check answers an already-registered
//!   address BEFORE the io fetch (the pre-check lives on the shell's flight
//!   seam per ADR-006 D4; these methods are only reached for fresh builds).
//! - The historical Python error surfaces stay at the shell: the methods
//!   return typed errors carrying exactly the facts the shell's messages
//!   interpolate (the method prefix stays at the shell, which owns the
//!   pymethod name).

use std::sync::Arc;

use alloy::primitives::{Address, I256, U256};
use degenbot_db::snapshot::TickMapDb;
use degenbot_pools::sim::tick_fetch::TickWordFetcher;
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::swap_simulation::{SwapOutcome, SwapRead, SwapRequest};
use degenbot_substrate::PoolRegistration;

use super::bot::Bot;
use super::construction_io::ConstructionIo;
use super::pool_builder::builder;
use super::pool_builder::construction_door::PoolConstruction;
use super::pool_builder::parity;
use super::pool_builder::route::{self, ConstructedPool};
use super::registration_ledger::{RefusalKind, RegisteredPoolFamily, RegistrationPhase};
use super::{ConcentratedLiquiditySlotLayout, PoolTickCoverage};

/// The `calculate_tokens_out` refusal (the legacy Python surface: a
/// `ValueError` for the overflow/on-chain-revert and unknown-pool classes,
/// `0` for the unrecovered sparse-map misses).
#[derive(Debug)]
pub enum CalcTokensOutError {
    /// The request overflowed a `uint256` intermediate (on-chain
    /// `getAmountOut` `SafeMath` revert — cdbc03bb).
    Overflow,
    /// Non-computable arithmetic/invariant (same Python `ValueError`
    /// message as [`CalcTokensOutError::Overflow`]).
    NotComputable,
    /// The `pool_id` is not registered.
    UnknownPool { pool_id: u64 },
    /// The registered family has no exact-input path for this operation.
    UnsupportedFamily { pool_id: u64, family: &'static str },
}

/// The `calculate_tokens_in` refusal: the legacy contract flattens every
/// non-computed outcome to `0` EXCEPT the typed family gap.
#[derive(Debug)]
pub enum CalcTokensInError {
    /// The request overflowed a `uint256` intermediate.
    Overflow,
    /// The registered family has no exact-output path (a typed gap, not the
    /// legacy silent-0 contract).
    UnsupportedFamily { pool_id: u64, family: &'static str },
}

/// The build-and-register return payload: `(pool_id, identity)` with the
/// core-computed identity `(token0, token1, address, variant)` — the
/// builder's return surface (checksummed hex strings).
pub type PoolBuildPayload = (u64, (String, String, String, String));

impl Bot {
    /// The attached `ConstructionIo`, or the typed no-io refusal at the
    /// named family (the shell formats the pymethod-prefixed message).
    fn construction_io_required(
        &self,
        family: Option<RegisteredPoolFamily>,
    ) -> Result<Arc<ConstructionIo>, RefusalKind> {
        self.construction_io_arc()
            .ok_or_else(|| RefusalKind::missing_construction_io(family, RegistrationPhase::Build))
    }

    /// Build + register a V2 pool: the core `builder::build_v2` choreography
    /// (immutable data, reserves, DEX resolution incl. the Camelot branch,
    /// CREATE2 verify, deployer/init-hash, narrow reserves) on the attached
    /// `ConstructionIo`, then the family write door's registration
    /// (`BotState::register_pool`, ADR-078 D3) under ONE core write guard.
    /// Returns `(pool_id, identity)` with the core-computed
    /// identity `(token0, token1, address, variant)` — the builder's return
    /// surface, so a facade-free registration driver consumes it directly.
    ///
    /// # Errors
    /// [`RefusalKind::MissingConstructionIo`] when no construction I/O is
    /// attached, [`RefusalKind::BuildRefused`] on a builder failure,
    /// [`RefusalKind::RegisterRefused`] when the registry refuses.
    pub fn build_and_register_v2(
        &self,
        address: Address,
        block: Option<u64>,
    ) -> Result<PoolBuildPayload, RefusalKind> {
        let io = self.construction_io_required(Some(RegisteredPoolFamily::V2))?;
        let params = degenbot_core::runtime::get_runtime()
            .block_on(builder::build_v2(self.chain_id(), address, &io, block))
            .map_err(|err| {
                RefusalKind::from_builder_error(
                    &err,
                    Some(RegisteredPoolFamily::V2),
                    RegistrationPhase::Build,
                )
            })?;
        let identity = (
            params.token0.to_checksum(None),
            params.token1.to_checksum(None),
            params.address.to_checksum(None),
            params.variant.as_str().to_string(),
        );
        // The minted identity rides the parity guard; capture it before
        // `params` moves into the door's request.
        let minted = parity::AddressIdentity {
            token0: params.token0,
            token1: params.token1,
            address: params.address,
        };
        let pool_id = RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::V2(Box::new(params))),
        )?;
        // Core-side identity-parity guard (at the mint site): the echo minted
        // from `params` must equal the identity the registry of record now
        // holds for `pool_id`. A divergence is a core/driver seam bug — refused
        // here rather than trusted across the FFI seam.
        if let Some(registered) = parity::registered_address_identity(self, pool_id) {
            parity::verify_address_identity(minted, registered).map_err(|divergence| {
                RefusalKind::from_identity_divergence(Some(RegisteredPoolFamily::V2), &divergence)
            })?;
        }

        // Telemetry: one Jaeger node per pool construction+registration
        // (zero-duration span carrying the full identity).
        let _reg = tracing::info_span!(
            "degenbot.pool.register",
            pool.version = "v2",
            pool.address = %identity.2,
            token0 = %identity.0,
            token1 = %identity.1,
            dex = %identity.3,
            pool.id = pool_id,
        )
        .entered();
        Ok((pool_id, identity))
    }

    /// Build + register an Aerodrome V2 pool — the Aerodrome twin of
    /// [`Self::build_and_register_v2`] (the `stable()` + `getFee()` +
    /// reserves + CREATE2 choreography, registered into
    /// `PoolEntry::AerodromeV2` through the family write door).
    ///
    /// # Errors
    /// [`RefusalKind::MissingConstructionIo`] / [`RefusalKind::BuildRefused`]
    /// / [`RefusalKind::RegisterRefused`] (the duplicate-address / spec
    /// refusal — previously an `assert!` panic that killed the process).
    pub fn build_and_register_aerodrome_v2(
        &self,
        address: Address,
        block: Option<u64>,
    ) -> Result<u64, RefusalKind> {
        let io = self.construction_io_required(Some(RegisteredPoolFamily::AerodromeV2))?;
        let params = degenbot_core::runtime::get_runtime()
            .block_on(builder::build_aerodrome_v2(
                self.chain_id(),
                address,
                &io,
                block,
            ))
            .map_err(|err| {
                RefusalKind::from_builder_error(
                    &err,
                    Some(RegisteredPoolFamily::AerodromeV2),
                    RegistrationPhase::Build,
                )
            })?;
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::AerodromeV2(Box::new(params))),
        )
    }

    /// Build + register a Balancer V2 **weighted** pool (the `getPoolId` +
    /// Vault `getPoolTokens` + `getSwapFeePercentage` +
    /// `getNormalizedWeights` + bytecode `PowVersion` detect + `decimals()`
    /// scaling-factor choreography, registered into
    /// `PoolEntry::BalancerWeighted`).
    ///
    /// # Errors
    /// [`RefusalKind::MissingConstructionIo`] / [`RefusalKind::BuildRefused`].
    pub fn build_and_register_balancer_weighted(
        &self,
        vault: Address,
        address: Address,
        block: Option<u64>,
    ) -> Result<u64, RefusalKind> {
        let io = self.construction_io_required(None)?;
        let params = degenbot_core::runtime::get_runtime()
            .block_on(builder::build_balancer_weighted(vault, address, &io, block))
            .map_err(|err| RefusalKind::from_builder_error(&err, None, RegistrationPhase::Build))?;
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::BalancerWeighted(Box::new(params))),
        )
    }

    /// Build + register a Balancer V2 **stable** pool (the `getPoolId` +
    /// Vault `getPoolTokens` + `getSwapFeePercentage` +
    /// `getAmplificationParameter` + BPT-detect + rate-provider/rate +
    /// scaling-factor + `invariant_version` resolution choreography,
    /// registered into `PoolEntry::BalancerStable`). `invariant_version`
    /// overrides the shape-derived invariant version (BPT among the Vault
    /// tokens → V1, else V2).
    ///
    /// # Errors
    /// [`RefusalKind::MissingConstructionIo`] / [`RefusalKind::BuildRefused`].
    pub fn build_and_register_balancer_stable(
        &self,
        vault: Address,
        address: Address,
        block: Option<u64>,
        invariant_version: Option<u8>,
    ) -> Result<u64, RefusalKind> {
        let io = self.construction_io_required(None)?;
        let params = degenbot_core::runtime::get_runtime()
            .block_on(builder::build_balancer_stable(
                vault,
                address,
                &io,
                block,
                invariant_version,
            ))
            .map_err(|err| RefusalKind::from_builder_error(&err, None, RegistrationPhase::Build))?;
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::BalancerStable(Box::new(params))),
        )
    }

    /// Build + register a Curve `StableSwap` pool (the full detection
    /// choreography — coins + balances, `A`/`fee`/`admin_fee`, A-ramping,
    /// lending, crypto params, `lp_token`, metapool base + underlying coins,
    /// ERC20 decimals — registered with a Rust-native
    /// `RpcCurveDataProvider`).
    ///
    /// # Errors
    /// [`RefusalKind::MissingConstructionIo`] / [`RefusalKind::BuildRefused`].
    pub fn build_and_register_curve_pool(
        &self,
        address: Address,
        registry_addresses: &[Address],
        block: Option<u64>,
    ) -> Result<u64, RefusalKind> {
        let io = self.construction_io_required(None)?;
        let params = degenbot_core::runtime::get_runtime()
            .block_on(builder::build_curve_pool(
                address,
                registry_addresses,
                &io,
                block,
            ))
            .map_err(|err| RefusalKind::from_builder_error(&err, None, RegistrationPhase::Build))?;
        RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::Curve(Box::new(params))),
        )
    }

    /// Build + register an ERC-20 token: `builder::build_erc20_metadata`
    /// resolves `name`/`symbol`/`decimals` (DB row → on-chain batched read →
    /// alternate-prototype fallback → UNKNOWN sentinels, with a DB
    /// write-back), then the token registers into `BotState.tokens` exactly
    /// like [`Self::register_token`]. Returns `(name, symbol, decimals)`;
    /// the handle construction stays at the shell.
    ///
    /// # Errors
    /// [`RefusalKind::MissingConstructionIo`] / [`RefusalKind::BuildRefused`].
    pub fn build_and_register_erc20_token(
        &self,
        chain_id: u64,
        address: Address,
        block: Option<u64>,
    ) -> Result<(String, String, u8), RefusalKind> {
        let io = self.construction_io_required(None)?;
        #[expect(clippy::cast_possible_wrap)] // ids are small; core signature is i64
        let (name, symbol, decimals) = degenbot_core::runtime::get_runtime()
            .block_on(builder::build_erc20_metadata(
                &io,
                chain_id as i64,
                address,
                block,
            ))
            .map_err(|err| RefusalKind::from_builder_error(&err, None, RegistrationPhase::Build))?;
        self.state_arc()
            .write_at(LockSite::Orchestrator)
            .register_token(address, name.clone(), symbol.clone(), decimals, chain_id);
        Ok((name, symbol, decimals))
    }

    /// Build + register a V3 pool through the ONE core construction door
    /// ([`Bot::construct_pool`](super::pool_builder::construction_door) —
    /// get-or-register + route order + build + register over the route
    /// interior), then shape the identity echo via
    /// [`Self::finalize_v3_construct`].
    ///
    /// The construction io is a parameter (the shell fetched it via
    /// [`Self::require_construction_io`] — its error message carries the
    /// pymethod prefix); the sparse-backfill fetcher, DB handle, and slot
    /// layout ride the door's [`PoolConstruction::V3`] variant.
    ///
    /// # Errors
    /// [`RefusalKind::BuildRefused`] (or [`RefusalKind::UnsupportedFamily`])
    /// from the construction route, [`RefusalKind::UnreadableIdentity`] on
    /// the race-answer arm.
    #[expect(clippy::too_many_arguments)]
    pub fn build_and_register_v3(
        &self,
        io: &ConstructionIo,
        plan: &route::ConstructionRoute,
        db: Option<&dyn TickMapDb>,
        fetcher: Option<Arc<dyn TickWordFetcher>>,
        slot_layout: Option<ConcentratedLiquiditySlotLayout>,
        address: Address,
        block: Option<u64>,
    ) -> Result<PoolBuildPayload, RefusalKind> {
        let constructed = degenbot_core::runtime::get_runtime()
            .block_on(self.construct_pool(
                plan,
                PoolConstruction::V3 {
                    address,
                    db,
                    fetcher,
                    slot_layout,
                },
                io,
                block,
            ))
            .into_result()
            .map_err(|refusal| {
                RefusalKind::from_construction_refusal(&refusal, Some(RegisteredPoolFamily::V3))
            })?;
        self.finalize_v3_construct(constructed, address)
    }

    /// Shape the V3 identity echo from a constructed pool: a fresh build
    /// resolves the family from the builder-verified `factory` via
    /// `resolve_dex_name` (kebab-case, e.g. "uniswap"), falling back to the
    /// generic "uniswap-v3" for an unknown deployment; a registry-GET race
    /// answer (`built == None`) echoes the already-registered payload from
    /// the registry of record. Emits the `degenbot.pool.register` telemetry
    /// span (one Jaeger node per V3 registration).
    ///
    /// # Errors
    /// [`RefusalKind::UnreadableIdentity`] when the race answer's registry
    /// entry carried no readable V3 identity.
    pub fn finalize_v3_construct(
        &self,
        constructed: ConstructedPool,
        address: Address,
    ) -> Result<PoolBuildPayload, RefusalKind> {
        let (pool_id, identity) = if let Some(built) = constructed.built {
            let family = degenbot_uniswap::deployments::resolve_dex_name(
                self.chain_id(),
                built.factory.unwrap_or_default(),
            )
            .map_or_else(|| "uniswap-v3".to_string(), |d| d.as_str().to_string());
            (
                constructed.pool_id,
                (
                    built.token0.to_checksum(None),
                    built.token1.to_checksum(None),
                    built.address.to_checksum(None),
                    family,
                ),
            )
        } else {
            let payload = self
                .registered_v3_payload(&address, self.chain_id())
                .ok_or_else(|| {
                    RefusalKind::unreadable_identity(
                        Some(RegisteredPoolFamily::V3),
                        RegistrationPhase::Register,
                        address,
                        format!("registry GET answered {address} with no readable V3 identity"),
                    )
                })?;
            (payload.0, (payload.1, payload.2, payload.3, payload.4))
        };
        let _reg = tracing::info_span!(
            "degenbot.pool.register",
            pool.version = "v3",
            pool.address = %identity.2,
            token0 = %identity.0,
            token1 = %identity.1,
            dex = %identity.3,
            pool.id = pool_id,
        )
        .entered();
        Ok((pool_id, identity))
    }

    /// Build + register a V4 pool: the core `builder::build_v4` (LIVE
    /// scalars off `state_view` + the Chain/Db tick-map arm) with the
    /// caller-supplied identity, then the family write door's registration
    /// under ONE core write guard. Returns `(pool_id, coverage,
    /// protocol_fee, lp_fee)`; the identity echo is derived from the
    /// caller-supplied
    /// `identity` (the shell keeps shaping its own parsed values).
    ///
    /// # Errors
    /// [`RefusalKind::MissingConstructionIo`], [`RefusalKind::BuildRefused`],
    /// [`RefusalKind::RegisterRefused`].
    pub fn build_and_register_v4(
        &self,
        identity: builder::V4PoolBuildIdentity,
        db: Option<&dyn TickMapDb>,
        block: Option<u64>,
        fetcher: Option<Arc<dyn TickWordFetcher>>,
    ) -> Result<(u64, String, u32, u32), RefusalKind> {
        let io = self.construction_io_required(Some(RegisteredPoolFamily::V4))?;
        // Capture the span identity before `identity` moves into build_v4.
        let span_manager = identity.pool_manager;
        let span_pool_id = format!("0x{}", alloy::hex::encode(identity.pool_id));
        let span_fee = identity.fee;
        let span_tick_spacing = identity.tick_spacing;
        // The minted construction identity (the immutable pool-key fields).
        let minted_identity = parity::V4Identity {
            currency0: identity.currency0,
            currency1: identity.currency1,
            fee: identity.fee,
            tick_spacing: identity.tick_spacing,
        };
        let result = degenbot_core::runtime::get_runtime()
            .block_on(builder::build_v4(identity, db, &io, block))
            .map_err(|err| {
                RefusalKind::from_builder_error(
                    &err,
                    Some(RegisteredPoolFamily::V4),
                    RegistrationPhase::Build,
                )
            })?;
        let mut params = result.params;
        // lp_fee + protocol_fee come from the SAME head-stamped slot0 read
        // inside build_v4 (no second fetch_v4_slot0_liquidity each pool).
        let lp_fee = result.lp_fee;
        let protocol_fee = params.protocol_fee;
        // Sparse-map parity (the V4 twin of the V3 build): the builder leaves
        // the backfill fetcher `None`; the driver's Python-wrapped fetcher
        // rides in here.
        params.fetcher = fetcher;
        let coverage = match params.coverage {
            PoolTickCoverage::Tracked => "tracked",
            PoolTickCoverage::Sparse => "sparse",
        };
        let registered = RefusalKind::from_registration_verdict(
            self.state_arc()
                .write_at(LockSite::Orchestrator)
                .register_pool(PoolRegistration::V4(Box::new(params))),
        )?;
        // Core-side identity-parity guard (at the mint site): the minted
        // pool-key identity must equal the registered identity's key. A
        // divergence is a core/driver seam bug — refused here rather than
        // trusted across the FFI seam.
        if let Some(registered_identity) = self
            .state_arc()
            .read_at(LockSite::Orchestrator)
            .pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(registered)
            .map(|i| parity::V4Identity {
                currency0: i.pool_key.currency0,
                currency1: i.pool_key.currency1,
                fee: i.pool_key.fee,
                tick_spacing: i.pool_key.tick_spacing,
            })
        {
            parity::verify_v4_identity(minted_identity, registered_identity).map_err(
                |divergence| {
                    RefusalKind::from_identity_divergence(
                        Some(RegisteredPoolFamily::V4),
                        &divergence,
                    )
                },
            )?;
        }

        // Telemetry: one Jaeger node per V4 registration (pool.manager is
        // the PoolManager; pool.id is the 32-byte id).
        let _reg = tracing::info_span!(
            "degenbot.pool.register",
            pool.version = "v4",
            pool.manager = %span_manager,
            pool.id = %span_pool_id,
            fee = span_fee,
            tick_spacing = span_tick_spacing,
        )
        .entered();
        Ok((registered, coverage.to_string(), protocol_fee, lp_fee))
    }

    /// The attached `ConstructionIo` or the typed no-io refusal — for the
    /// families whose build entry takes the io as a parameter (the V3 route),
    /// so the shell's error message keeps its pymethod prefix.
    ///
    /// # Errors
    /// [`RefusalKind::MissingConstructionIo`] when nothing is attached.
    pub fn require_construction_io(&self) -> Result<Arc<ConstructionIo>, RefusalKind> {
        self.construction_io_required(None)
    }

    /// Simulate an exact-input swap (`getAmountOut` semantics — the request
    /// amount is negated per the sign convention) against current registered
    /// pool state under ONE core write guard (the sparse-miss recovery fills
    /// words). Legacy numeric contract preserved: unrecovered sparse-map
    /// misses flatten to `0`; the overflow and unknown-pool classes surface
    /// as typed errors.
    ///
    /// # Errors
    /// [`CalcTokensOutError::Overflow`] on a `uint256` intermediate overflow,
    /// [`CalcTokensOutError::NotComputable`] (same Python message as the
    /// overflow — on-chain revert class), [`CalcTokensOutError::UnknownPool`],
    /// [`CalcTokensOutError::UnsupportedFamily`].
    pub fn calculate_tokens_out(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_in: U256,
    ) -> Result<U256, CalcTokensOutError> {
        let request = SwapRequest {
            zero_for_one,
            amount_specified: -I256::try_from(amount_in)
                .map_err(|_| CalcTokensOutError::Overflow)?,
            sqrt_price_limit: None,
        };
        let result = self
            .state_arc()
            .write_at(LockSite::Orchestrator)
            .swap_simulation(0, pool_id, request);
        // cdbc03bb: surface `NotComputable` (uint256 overflow = on-chain
        // revert) as a typed error; keep unrecovered sparse-map misses mapped
        // to 0 so callers that haven't opted into the fetch-retry path keep
        // the legacy no-raise-on-miss contract.
        match result {
            SwapRead::Computed(outcome) => Ok(outcome.delivered_unsigned()),
            SwapRead::FetchFailed { .. } | SwapRead::FetchExhausted { .. } => Ok(U256::ZERO),
            SwapRead::NotComputable => Err(CalcTokensOutError::NotComputable),
            SwapRead::UnknownPool { pool_id } => Err(CalcTokensOutError::UnknownPool { pool_id }),
            SwapRead::UnsupportedFamily { pool_id, family } => {
                Err(CalcTokensOutError::UnsupportedFamily { pool_id, family })
            }
        }
    }

    /// Simulate an exact-output swap (ADR-037: positive user-perspective
    /// request; the required input rides back as the consumed magnitude)
    /// under ONE core write guard. Legacy numeric contract preserved at this
    /// seam: any non-computed outcome flattens to `0` EXCEPT the typed family
    /// gap.
    ///
    /// # Errors
    /// [`CalcTokensInError::Overflow`],
    /// [`CalcTokensInError::UnsupportedFamily`].
    pub fn calculate_tokens_in(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: U256,
    ) -> Result<U256, CalcTokensInError> {
        let request = SwapRequest {
            zero_for_one,
            amount_specified: I256::try_from(amount_out)
                .map_err(|_| CalcTokensInError::Overflow)?,
            sqrt_price_limit: None,
        };
        let read = self
            .state_arc()
            .write_at(LockSite::Orchestrator)
            .swap_simulation(0, pool_id, request);
        match read {
            SwapRead::Computed(outcome) => Ok(match &outcome {
                SwapOutcome::V2(o) => (-o.consumed).into_raw(),
                SwapOutcome::V3(o) | SwapOutcome::V4(o) => (-o.consumed).into_raw(),
            }),
            // A registered family with no exact-output path is a typed gap,
            // not the legacy silent-0 contract.
            SwapRead::UnsupportedFamily { pool_id, family } => {
                Err(CalcTokensInError::UnsupportedFamily { pool_id, family })
            }
            _ => Ok(U256::ZERO),
        }
    }

    /// The raw stableswap `get_dy` for a Curve pool — the read guard is
    /// taken here (`LockSite::Orchestrator`); the math + provider resolution are
    /// `BotState::get_dy`'s (their error type passes through, and the
    /// shell's `Debug`-format surface is unchanged).
    ///
    /// # Errors
    /// [`CurveInputsError`](super::CurveInputsError) as the state method.
    pub fn curve_get_dy(
        &self,
        pool_id: u64,
        i: usize,
        j: usize,
        dx: U256,
        block_number: u64,
        override_balances: Option<&[U256]>,
    ) -> Result<U256, super::CurveInputsError> {
        self.state_arc().read_at(LockSite::Orchestrator).get_dy(
            pool_id,
            i,
            j,
            dx,
            block_number,
            override_balances,
        )
    }

    /// Apply a V2 `Sync` event (reserves + journaling) under ONE core write
    /// guard.
    pub fn update_v2_pool(
        &self,
        address: Address,
        reserve0: alloy::primitives::aliases::U112,
        reserve1: alloy::primitives::aliases::U112,
        block_number: u64,
    ) {
        let _ = self
            .state_arc()
            .write_at(LockSite::Orchestrator)
            .apply_sync_by_address(address, reserve0, reserve1, block_number);
    }

    /// Apply a V3 `Swap` event (scalars + reorg journal priors; no per-tick
    /// priors — the shell entry never carried any) under ONE core write
    /// guard. No-op if the pool is not registered.
    pub fn update_pool(
        &self,
        address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
    ) {
        self.state_arc()
            .write_at(LockSite::Orchestrator)
            .update_pool(
                address,
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
                vec![],
            );
    }
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::super::{RegisterCurvePoolParams, RegisterV2PoolParams, RegisterV3PoolParams};
    use super::*;

    /// A non-JSON factory (CREATE2 verify skipped).
    fn adhoc_factory() -> Address {
        Address::from([0x33u8; 20])
    }

    /// The V3 fixture geometry the registration fixtures share.
    fn v3_fixture_params() -> RegisterV3PoolParams {
        RegisterV3PoolParams {
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            fee: 3_000,
            tick_spacing: 60,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: hashbrown::HashMap::new(),
            update_block: 100,
            coverage: PoolTickCoverage::Sparse,
            slot_layout:
                degenbot_pools::state::v3_state::ConcentratedLiquiditySlotLayout::UniswapV3,
            ..Default::default()
        }
    }

    /// Register a fixture V2 pool (the facade registration; the CREATE2
    /// verify is skipped for the non-JSON factory).
    fn register_v2_fixture(bot: &Bot, address: Address) -> u64 {
        bot.register(PoolRegistration::V2(Box::new(RegisterV2PoolParams {
            address,
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            reserve0: U256::from(1000).to::<alloy::primitives::aliases::U112>(),
            reserve1: U256::from(2000).to::<alloy::primitives::aliases::U112>(),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: adhoc_factory(),
            update_block: 10,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            ..Default::default()
        })))
        .expect("test setup: V2 registration")
    }

    /// A fresh build with no `ConstructionIo` attached is refused with the
    /// typed no-io arm BEFORE any registry write (the shell maps this to the
    /// pymethod-prefixed `RuntimeError`).
    #[test]
    fn build_entries_require_construction_io() {
        let bot = Bot::new(1);
        let addr = Address::from([0x11u8; 20]);
        assert!(matches!(
            bot.build_and_register_v2(addr, None),
            Err(RefusalKind::MissingConstructionIo { .. })
        ));
        assert!(matches!(
            bot.build_and_register_aerodrome_v2(addr, None),
            Err(RefusalKind::MissingConstructionIo { .. })
        ));
        assert!(matches!(
            bot.build_and_register_balancer_weighted(addr, addr, None),
            Err(RefusalKind::MissingConstructionIo { .. })
        ));
        assert!(matches!(
            bot.build_and_register_balancer_stable(addr, addr, None, None),
            Err(RefusalKind::MissingConstructionIo { .. })
        ));
        assert!(matches!(
            bot.build_and_register_curve_pool(addr, &[], None),
            Err(RefusalKind::MissingConstructionIo { .. })
        ));
        assert!(matches!(
            bot.build_and_register_erc20_token(1, addr, None),
            Err(RefusalKind::MissingConstructionIo { .. })
        ));
        assert!(matches!(
            bot.build_and_register_v4(
                builder::V4PoolBuildIdentity {
                    pool_manager: addr,
                    state_view: addr,
                    pool_id: [0xAB; 32],
                    currency0: addr,
                    currency1: addr,
                    fee: 3_000,
                    tick_spacing: 60,
                    hook_address: Address::ZERO,
                },
                None,
                None,
                None,
            ),
            Err(RefusalKind::MissingConstructionIo { .. })
        ));
        assert_eq!(bot.pool_count(), 0, "nothing registered on refusal");
    }

    /// `calculate_tokens_out` through the facade matches the on-chain
    /// `getAmountOut` arithmetic against the registered reserves, and the
    /// typed refusals carry the legacy classes (unknown pool; the uint256
    /// overflow the on-chain `SafeMath` revert models).
    #[test]
    fn calculate_tokens_out_through_the_facade() {
        let bot = Bot::new(1);
        let addr = Address::from([0x11u8; 20]);
        let pool_id = register_v2_fixture(&bot, addr);

        // 1000 in against (1000, 2000) reserves with the 997/1000 fee:
        // floor(1000*997*2000 / (1000*1000 + 1000*997)) = 998.
        let out = bot
            .calculate_tokens_out(pool_id, true, U256::from(1000u64))
            .expect("exact-input computes");
        assert_eq!(out, U256::from(998u64));

        // Unknown pool: the typed refusal (the shell's `ValueError`).
        assert!(matches!(
            bot.calculate_tokens_out(9_999, true, U256::from(1u64)),
            Err(CalcTokensOutError::UnknownPool { pool_id: 9_999 })
        ));

        // A `uint256`-overflowing intermediate is the on-chain revert class.
        assert!(matches!(
            bot.calculate_tokens_out(pool_id, true, U256::MAX),
            Err(CalcTokensOutError::Overflow)
        ));
    }

    /// `calculate_tokens_in` through the facade: the required input feeds
    /// `calculate_tokens_out` back to exactly the requested output; the
    /// legacy silent-0 contract holds for unknown pools; the overflow class
    /// is the typed refusal.
    #[test]
    fn calculate_tokens_in_through_the_facade() {
        let bot = Bot::new(1);
        let addr = Address::from([0x11u8; 20]);
        let pool_id = register_v2_fixture(&bot, addr);

        let required = bot
            .calculate_tokens_in(pool_id, true, U256::from(998u64))
            .expect("exact-output computes");
        let delivered = bot
            .calculate_tokens_out(pool_id, true, required)
            .expect("round-trip computes");
        assert_eq!(delivered, U256::from(998u64), "exact-output inverts");

        // Unknown pool flattens to 0 (the legacy no-raise contract).
        assert_eq!(
            bot.calculate_tokens_in(9_999, true, U256::from(1u64))
                .expect("unknown pool flattens"),
            U256::ZERO
        );

        // Overflowing request: the typed overflow.
        assert!(matches!(
            bot.calculate_tokens_in(pool_id, true, U256::MAX),
            Err(CalcTokensInError::Overflow)
        ));
    }

    /// `get_dy` through the facade delegates to the state method under
    /// the core read guard, passing the typed error through unchanged (the
    /// shell's `Debug`-format surface).
    #[test]
    fn curve_get_dy_through_the_facade() {
        let bot = Bot::new(1);
        let params = RegisterCurvePoolParams {
            address: Address::from([0x44u8; 20]),
            tokens: vec![Address::from([0x01u8; 20]), Address::from([0x02u8; 20])],
            a_coefficient: 100,
            a_precision: 100,
            fee: 0,
            admin_fee: 0,
            rate_multipliers: vec![U256::from(1_000_000_000_000_000_000u128); 2],
            balances: vec![U256::from(1_000_000_000_000_000_000_000_000u128); 2],
            update_block: 10,
            // STANDARD (=1) discriminants: the dy calculator's vocabularies
            // (SwapStyle / LENDING_NONE / D / Y / Yd) are 1-based.
            swap_style: 1,
            lending_rate_style: 1,
            d_variant: 1,
            y_variant: 1,
            yd_variant: 1,
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
        };
        let pool_id = bot
            .register(PoolRegistration::Curve(Box::new(params)))
            .expect("curve registers");

        let dy = bot
            .curve_get_dy(
                pool_id,
                0,
                1,
                U256::from(1_000_000_000_000_000_000u128),
                10,
                None,
            )
            .expect("stableswap get_dy computes");
        assert!(dy > U256::ZERO, "positive dy");
        assert!(
            dy <= U256::from(1_000_000_000_000_000_000u128),
            "dy never exceeds dx at parity"
        );

        // Unknown pool: the typed error passes through (UnknownPool arm).
        let err = bot
            .curve_get_dy(
                9_999,
                0,
                1,
                U256::from(1_000_000_000_000_000_000u128),
                10,
                None,
            )
            .expect_err("unknown pool refused");
        assert!(
            matches!(err, super::super::CurveInputsError::UnknownPool(_)),
            "{err:?}"
        );
    }

    /// The V3 identity echo: a fresh build resolves the family from the
    /// built factory (generic "uniswap-v3" off the JSON deployments), a
    /// registry-GET race answer echoes the registered payload, and an
    /// unregistered race answer is the typed no-readable-identity refusal.
    #[test]
    fn finalize_v3_construct_shapes_the_identity_echo() {
        let bot = Bot::new(1);
        let addr = Address::from([0x22u8; 20]);

        // Fresh-build arm (no RPC involved — the identity is shaped
        // core-side from the built identity).
        let built = route::BuiltIdentity {
            address: addr,
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            factory: Some(adhoc_factory()),
            v4_pool_id: None,
        };
        let (pool_id, identity) = bot
            .finalize_v3_construct(
                ConstructedPool {
                    pool_id: 7,
                    built: Some(built),
                },
                addr,
            )
            .expect("fresh build shapes its identity");
        assert_eq!(pool_id, 7);
        assert_eq!(identity.3, "uniswap-v3");

        // Race-answer arm: `built == None` echoes the registered payload.
        let registered = bot
            .register(PoolRegistration::V3(Box::new(RegisterV3PoolParams {
                address: addr,
                factory: adhoc_factory(),
                ..v3_fixture_params()
            })))
            .expect("test setup: V3 registration");
        let (pool_id, identity) = bot
            .finalize_v3_construct(
                ConstructedPool {
                    pool_id: 0,
                    built: None,
                },
                addr,
            )
            .expect("registered race answer echoes its payload");
        assert_eq!(pool_id, registered);
        assert_eq!(identity.3, "uniswap-v3");

        // Unregistered race answer: the typed refusal (the shell's
        // method-prefixed `RuntimeError`).
        let err = bot
            .finalize_v3_construct(
                ConstructedPool {
                    pool_id: 0,
                    built: None,
                },
                Address::from([0xEE; 20]),
            )
            .expect_err("unregistered race answer refuses");
        assert!(matches!(err, RefusalKind::UnreadableIdentity { .. }));
    }

    /// `update_v2_pool` lands the Sync reserves under the core write guard
    /// (journaling the priors), visible to the next swap simulation.
    #[test]
    fn update_v2_pool_through_the_facade() {
        let bot = Bot::new(1);
        let addr = Address::from([0x11u8; 20]);
        let pool_id = register_v2_fixture(&bot, addr);

        // Before: 100 in against (1000, 2000) → 181.
        let before = bot
            .calculate_tokens_out(pool_id, true, U256::from(100u64))
            .expect("pre-update computes");
        assert_eq!(before, U256::from(181u64));

        bot.update_v2_pool(
            addr,
            U256::from(500).to::<alloy::primitives::aliases::U112>(),
            U256::from(600).to::<alloy::primitives::aliases::U112>(),
            20,
        );

        // After: 100 in against (500, 600) → 99.
        let after = bot
            .calculate_tokens_out(pool_id, true, U256::from(100u64))
            .expect("post-update computes");
        assert_eq!(after, U256::from(99u64));
    }

    /// `update_v3_pool` journals the scalar priors under the core write
    /// guard (the reorg-restore discipline).
    #[test]
    fn update_v3_pool_through_the_facade() {
        let bot = Bot::new(1);
        let addr = Address::from([0x22u8; 20]);
        let pool_id = bot
            .register(PoolRegistration::V3(Box::new(RegisterV3PoolParams {
                address: addr,
                factory: adhoc_factory(),
                ..v3_fixture_params()
            })))
            .expect("test setup: V3 registration");
        assert_eq!(bot.v3_journal_len(pool_id), 0);
        bot.update_pool(addr, U256::from(2u128) << 96, 2_000_000, -1, 101);
        assert_eq!(
            bot.v3_journal_len(pool_id),
            1,
            "the scalar priors journal (reorg-restore discipline)"
        );
    }
}
