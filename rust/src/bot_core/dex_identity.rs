//! DEX identity presets — the standalone Rust-core registry (ADR-005 slice 6).
//!
//! A `DexIdentity` is a frozen value object holding the per-DEX per-chain
//! deployment identity: factory, deployer, CREATE2 init hash, default fee
//! params, ABI struct shape for reserve decoding, and a `DexVariant` tag.
//! `pub const` presets encode each supported DEX+variant's CANONICAL-CHAIN
//! deployment (Uniswap/Sushi → Ethereum mainnet; Pancake → Ethereum mainnet
//! V2-style; Swapbased → Base; Camelot/Aerodrome → Arbitrum).
//!
//! ## Why this lives here (ADR-005 "standalone constraint")
//!
//! A standalone Rust consumer constructing a Sushiswap V2 pool must be able to
//! look up its factory/deployer/init-hash/fees **without a Python import**. If
//! these presets lived in `degenbot.dex_presets` (Python), the standalone
//! claim breaks. So `DexIdentity` + presets are pure Rust (no `pyo3`); the
//! Python seam ([`crate::bot_core::py_dex_identity`]) is a thin read-only view.
//!
//! ## Why `DexIdentity` is NOT a `V2PoolState` field
//!
//! A pool's swap-math inputs (reserves) don't need the identity to apply Sync
//! events or solve swaps — that's invariant math. The identity is needed only
//! at *encoding* (factory → calldata recipient) and *registration* (which
//! preset constructed this). So it's a construction/encoding parameter, not
//! state (ADR-005: "identity is a construction/encoding parameter, not state").
//!
//! ## Fee convention (slice-5 lesson, encoded here)
//!
//! `fee_tokenN: (u64, u64)` = `(gamma_numer, fee_denom)` where `gamma_numer`
//! is the **retained post-fee fraction** numerator (e.g. `997` for a 0.3% fee,
//! `fee_denom=1000`) — NOT the fee numerator. This matches `V2PoolState`'s
//! `fee_tokenN` field doc + the `IntHopState` convention + `register_v2_pool`'s
//! parameter meaning. Slice 5 fixed a latent bug where the fee numerator (3)
//! was registered instead of the retained fraction (997); these presets bake
//! the correct convention in by construction (e.g. `PancakeSwap`'s
//! `Fraction(25, 10000)` FEE → preset `(9975, 10_000)`).

use alloy::primitives::{address, b256, Address, B256};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// The DEX + variant discriminator for a `DexIdentity`.
///
/// Stable kebab-case string form (via [`DexVariant::as_str`]) is the canonical
/// lookup key for the Python seam (`dex_identity(variant="camelot-v2-stable")`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DexVariant {
    /// Uniswap V2 (canonical: Ethereum mainnet, chain 1).
    UniswapV2,
    /// `SushiSwap` V2 (canonical: Ethereum mainnet, chain 1).
    SushiswapV2,
    /// `PancakeSwap` V2 (canonical: Ethereum mainnet V2-style, chain 1).
    PancakeswapV2,
    /// `SwapBased` V2 (canonical: Base, chain 8453).
    SwapbasedV2,
    /// Camelot V2 volatile mode (canonical: Arbitrum, chain 42161).
    CamelotV2Volatile,
    /// Camelot V2 stable mode (canonical: Arbitrum, chain 42161).
    /// Fee is fetched per-pool on-chain (`token0FeePercent` / `token1FeePercent`
    /// over `FEE_DENOMINATOR`); the preset's fee params are a documented
    /// typical default, not a contract constant.
    CamelotV2Stable,
    /// Aerodrome V2 volatile mode (canonical: Base, chain 8453; Aerodrome also
    /// deploys on Arbitrum). Fees are fetched per-pool on-chain
    /// (`volatileFee` / `stableFee` over `FEE_DENOMINATOR`).
    AerodromeV2Volatile,
    /// Aerodrome V2 stable mode.
    AerodromeV2Stable,
}

impl DexVariant {
    /// Canonical kebab-case string form (the Python-seam lookup key).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UniswapV2 => "uniswap-v2",
            Self::SushiswapV2 => "sushiswap-v2",
            Self::PancakeswapV2 => "pancakeswap-v2",
            Self::SwapbasedV2 => "swapbased-v2",
            Self::CamelotV2Volatile => "camelot-v2-volatile",
            Self::CamelotV2Stable => "camelot-v2-stable",
            Self::AerodromeV2Volatile => "aerodrome-v2-volatile",
            Self::AerodromeV2Stable => "aerodrome-v2-stable",
        }
    }

    /// Parse from the canonical kebab-case form (the inverse of [`as_str`]).
    /// Case-insensitive.
    ///
    /// # Errors
    /// Returns `None` for an unrecognized string (the Python seam maps this to
    /// a `PanicException`-free `None` return).
    #[must_use]
    pub fn from_kebab(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "uniswap-v2" => Some(Self::UniswapV2),
            "sushiswap-v2" => Some(Self::SushiswapV2),
            "pancakeswap-v2" => Some(Self::PancakeswapV2),
            "swapbased-v2" => Some(Self::SwapbasedV2),
            "camelot-v2-volatile" => Some(Self::CamelotV2Volatile),
            "camelot-v2-stable" => Some(Self::CamelotV2Stable),
            "aerodrome-v2-volatile" => Some(Self::AerodromeV2Volatile),
            "aerodrome-v2-stable" => Some(Self::AerodromeV2Stable),
            _ => None,
        }
    }

    /// All variants in declaration order (for exhaustive iteration in tests).
    pub const ALL: [Self; 8] = [
        Self::UniswapV2,
        Self::SushiswapV2,
        Self::PancakeswapV2,
        Self::SwapbasedV2,
        Self::CamelotV2Volatile,
        Self::CamelotV2Stable,
        Self::AerodromeV2Volatile,
        Self::AerodromeV2Stable,
    ];
}

/// The ABI struct-type shape a DEX uses for its `Sync`-event reserve decoding.
///
/// Mirrors the Python `RESERVES_STRUCT_TYPES` `ClassVar`
/// (`src/degenbot/uniswap/v2_pool_calc.py` default; `pancakeswap/pools.py`
/// override). Decoupled from `DexVariant` because a future DEX could share a
/// variant's identity but use a different reserve encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReservesAbi {
    /// `(uint112, uint112)` — `UniswapV2` default (Sushi, Swapbased, Camelot,
    /// Aerodrome all inherit this).
    Standard,
    /// `(uint112, uint112, uint32)` — `PancakeSwap`'s extended reserve struct
    /// (adds a `blockTimestampLast` field).
    PancakeswapStyle,
}

impl ReservesAbi {
    /// The Solidity struct types for `eth_abi`-style decoding.
    #[must_use]
    pub const fn as_types(self) -> &'static [&'static str] {
        match self {
            Self::Standard => &["uint112", "uint112"],
            Self::PancakeswapStyle => &["uint112", "uint112", "uint32"],
        }
    }
}

// ---------------------------------------------------------------------------
// DexIdentity
// ---------------------------------------------------------------------------

/// Frozen value object: deployment identity for a V2-style DEX pool.
///
/// Holds the per-DEX per-chain deployment data needed to construct a pool
/// (factory, deployer, init hash, default fee params, reserve ABI shape) + a
/// `DexVariant` tag. Lighter than a `V2PoolState` — no reserves, no journal
/// (identity is construction-time data, not per-block state).
///
/// `fee_tokenN.0` is the RETAINED post-fee fraction numerator (`gamma_numer`
/// in `IntHopState` / `register_v2_pool`); `fee_tokenN.1` is the denominator.
/// See the module-level fee-convention comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DexIdentity {
    /// The factory contract address (CREATE2 deployer-or-redirect target).
    pub factory: Address,
    /// The CREATE2 deployer address. Equals `factory` for most DEXes (which use
    /// the factory itself as the deployer); `PancakeSwap` V3 uses a separate
    /// deployer — V2 presets here use `factory` for all (verified: every V2
    /// `v2_deployments` entry has `deployer=None` → "use factory").
    pub deployer: Address,
    /// The CREATE2 init code hash (Solidity `keccak256(type.creationCode)`).
    pub init_hash: B256,
    /// `token0→token1` fee parameters: `(gamma_numer, fee_denom)` — the
    /// retained post-fee fraction. `(997, 1000)` for a 0.3% fee.
    pub fee_token0: (u64, u64),
    /// `token1→token0` fee parameters: `(gamma_numer, fee_denom)`.
    pub fee_token1: (u64, u64),
    /// The ABI struct-type shape for `Sync`-event reserve decoders.
    pub reserves_abi: ReservesAbi,
    /// The DEX+variant discriminator.
    pub variant: DexVariant,
}

impl DexIdentity {
    /// Convenience: the kebab-case variant name (for logs / Python round-trip).
    #[must_use]
    pub const fn variant_str(self) -> &'static str {
        self.variant.as_str()
    }
}

/// Look up the `DexIdentity` preset for a variant (`from_kebab` → preset).
///
/// Returns `None` for an unrecognized string. The Python seam wraps this as a
/// `dex_identity(variant: str) -> Optional[PyDexIdentity]` callable so slice 7
/// can resolve presets by string from a Python builder.
#[must_use]
pub fn preset(variant: &str) -> Option<DexIdentity> {
    let v = DexVariant::from_kebab(variant)?;
    Some(preset_for_variant(v))
}

/// Look up the preset for a typed `DexVariant`.
#[must_use]
pub const fn preset_for_variant(variant: DexVariant) -> DexIdentity {
    match variant {
        DexVariant::UniswapV2 => UNISWAP_V2,
        DexVariant::SushiswapV2 => SUSHISWAP_V2,
        DexVariant::PancakeswapV2 => PANCAKESWAP_V2,
        DexVariant::SwapbasedV2 => SWAPBASED_V2,
        DexVariant::CamelotV2Volatile => CAMELOT_V2_VOLATILE,
        DexVariant::CamelotV2Stable => CAMELOT_V2_STABLE,
        DexVariant::AerodromeV2Volatile => AERODROME_V2_VOLATILE,
        DexVariant::AerodromeV2Stable => AERODROME_V2_STABLE,
    }
}

// ---------------------------------------------------------------------------
// Presets — TDD RED placeholders (B256::ZERO / Address::ZERO / (0,0)).
//
// The test module below encodes the VERIFIED Python ClassVar values cross-
// checked from `src/degenbot/`. Filling in the real values is the green step.
// ---------------------------------------------------------------------------

/// Uniswap factory (Ethereum mainnet, chain 1).
/// Source: `src/degenbot/uniswap/__init__.py` `_register_uniswap_deployments`
/// `v2_deployments[chain_id=1]` + `UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH`.
pub const UNISWAP_V2: DexIdentity = DexIdentity {
    factory: address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
    deployer: address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
    init_hash: b256!("96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"),
    fee_token0: (997, 1000),
    fee_token1: (997, 1000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::UniswapV2,
};

/// `SushiSwap` factory (Ethereum mainnet, chain 1).
pub const SUSHISWAP_V2: DexIdentity = DexIdentity {
    factory: address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
    deployer: address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
    init_hash: b256!("e18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"),
    fee_token0: (997, 1000),
    fee_token1: (997, 1000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::SushiswapV2,
};

/// `PancakeSwap` factory (Ethereum mainnet V2-style, chain 1).
pub const PANCAKESWAP_V2: DexIdentity = DexIdentity {
    factory: address!("1097053Fd2ea711dad45caCcc45EfF7548fCB362"),
    deployer: address!("1097053Fd2ea711dad45caCcc45EfF7548fCB362"),
    init_hash: b256!("57224589c67f3f30a6b0d7a1b54cf3153ab84563bc609ef41dfb34f8b2974d2d"),
    fee_token0: (9975, 10_000),
    fee_token1: (9975, 10_000),
    reserves_abi: ReservesAbi::PancakeswapStyle,
    variant: DexVariant::PancakeswapV2,
};

/// `SwapBased` factory (Base, chain 8453).
pub const SWAPBASED_V2: DexIdentity = DexIdentity {
    factory: address!("04C9f118d21e8B767D2e50C946f0cC9F6C367300"),
    deployer: address!("04C9f118d21e8B767D2e50C946f0cC9F6C367300"),
    init_hash: b256!("b64118b4e99d4a4163453838112a1695032df46c09f7f09064d4777d2767f8ea"),
    fee_token0: (997, 1000),
    fee_token1: (997, 1000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::SwapbasedV2,
};

/// Camelot V2 volatile (Arbitrum, chain 42161). Fee is a typical volatile
/// default (on-chain `token0FeePercent=300` over `FEE_DENOMINATOR=100000` →
/// 0.3%; gamma 99700/100000).
pub const CAMELOT_V2_VOLATILE: DexIdentity = DexIdentity {
    factory: address!("6EcCab422D763aC031210895C81787E87B43A652"),
    deployer: address!("6EcCab422D763aC031210895C81787E87B43A652"),
    init_hash: b256!("a856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1"),
    fee_token0: (99_700, 100_000),
    fee_token1: (99_700, 100_000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::CamelotV2Volatile,
};

/// Camelot V2 stable. Fee is per-pool on-chain; preset carries no
/// compile-time fee default (gamma 0 signals "fetched per-pool"); the
/// denominator mirrors Camelot's `FEE_DENOMINATOR` for encoding contexts.
pub const CAMELOT_V2_STABLE: DexIdentity = DexIdentity {
    factory: address!("6EcCab422D763aC031210895C81787E87B43A652"),
    deployer: address!("6EcCab422D763aC031210895C81787E87B43A652"),
    init_hash: b256!("a856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1"),
    fee_token0: (0, 100_000),
    fee_token1: (0, 100_000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::CamelotV2Stable,
};

/// Aerodrome V2 volatile (canonical Base, chain 8453). Fee is per-pool
/// on-chain (`volatileFee` over `FEE_DENOMINATOR=10000`); preset carries a
/// typical 0.3% default (gamma 9970/10000). Init hash is empty in the Python
/// source (different address generation — documented here as zero with a
/// `factory`-only budget; encoding/address-derivation for Aerodrome is carried
/// by a different path).
pub const AERODROME_V2_VOLATILE: DexIdentity = DexIdentity {
    factory: address!("420DD381b31aEf6683db6B902084cB0FFECe40Da"),
    deployer: address!("420DD381b31aEf6683db6B902084cB0FFECe40Da"),
    init_hash: B256::ZERO,
    fee_token0: (9970, 10_000),
    fee_token1: (9970, 10_000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::AerodromeV2Volatile,
};

/// Aerodrome V2 stable. Init hash is empty in the Python source (different
/// address generation); fee is per-pool on-chain (gamma 0 signals "fetched
/// per-pool"; denominator 10000 mirrors Aerodrome's `FEE_DENOMINATOR`).
pub const AERODROME_V2_STABLE: DexIdentity = DexIdentity {
    factory: address!("420DD381b31aEf6683db6B902084cB0FFECe40Da"),
    deployer: address!("420DD381b31aEf6683db6B902084cB0FFECe40Da"),
    init_hash: B256::ZERO,
    fee_token0: (0, 10_000),
    fee_token1: (0, 10_000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::AerodromeV2Stable,
};

#[cfg(test)]
mod tests {
    use super::*;

    // ---- DexVariant round-trip --------------------------------------------

    #[test]
    fn variant_str_round_trips_for_all_variants() {
        for v in DexVariant::ALL {
            let s = v.as_str();
            assert_eq!(DexVariant::from_kebab(s), Some(v), "round-trip failed for {s}");
        }
    }

    #[test]
    fn variant_from_str_is_case_insensitive() {
        assert_eq!(DexVariant::from_kebab("UNISWAP-V2"), Some(DexVariant::UniswapV2));
        assert_eq!(
            DexVariant::from_kebab("Camelot-V2-Stable"),
            Some(DexVariant::CamelotV2Stable)
        );
    }

    #[test]
    fn variant_from_str_rejects_unknown() {
        assert_eq!(DexVariant::from_kebab("curve"), None);
        assert_eq!(DexVariant::from_kebab(""), None);
        assert_eq!(DexVariant::from_kebab("uniswap"), None); // missing -v2 suffix
    }

    // ---- Preset lookup helpers --------------------------------------------

    #[test]
    fn preset_string_lookup_round_trips() {
        for v in DexVariant::ALL {
            let ident = preset(v.as_str()).expect("preset must exist for every variant");
            assert_eq!(ident.variant, v);
            assert_eq!(ident.variant_str(), v.as_str());
        }
    }

    #[test]
    fn preset_for_unknown_string_is_none() {
        assert!(preset("nonexistent-dex").is_none());
    }

    // ---- UNISWAP_V2 — cross-checked against Python source -----------------
    // Factory + init_hash: `src/degenbot/uniswap/__init__.py` v2_deployments[chain_id=1].
    // Init hash matches `UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH`.
    // Fee: `UniswapV2PoolCalc.FEE = Fraction(3, 1000)` → gamma 997/1000.
    #[test]
    fn uniswap_v2_preset_matches_python_classvars() {
        let ident = UNISWAP_V2;
        assert_eq!(
            ident.factory,
            address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
        );
        // V2 deployments all use deployer=None → "use factory".
        assert_eq!(ident.deployer, ident.factory);
        assert_eq!(
            ident.init_hash,
            b256!("96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f")
        );
        // Fraction(3, 1000) fee → RETAINED fraction (1000-3, 1000) = (997, 1000).
        // Slice-5 convention: gamma_numer is the kept amount, NOT the fee.
        assert_eq!(ident.fee_token0, (997, 1000));
        assert_eq!(ident.fee_token1, (997, 1000));
        assert_eq!(ident.reserves_abi, ReservesAbi::Standard);
        assert_eq!(ident.variant, DexVariant::UniswapV2);
    }

    // ---- SUSHISWAP_V2 — chain 1 factory + Sushi's own init hash -----------
    #[test]
    fn sushiswap_v2_preset_matches_python_source() {
        let ident = SUSHISWAP_V2;
        assert_eq!(
            ident.factory,
            address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac")
        );
        assert_eq!(ident.deployer, ident.factory);
        assert_eq!(
            ident.init_hash,
            b256!("e18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303")
        );
        assert_eq!(ident.fee_token0, (997, 1000)); // 3/1000 fee
        assert_eq!(ident.fee_token1, (997, 1000));
        assert_eq!(ident.reserves_abi, ReservesAbi::Standard);
    }

    // ---- PANCAKESWAP_V2 — chain 1 factory + Pancake's init hash + FEE --
    // Python: `PancakeswapV2Pool.FEE = Fraction(25, 10000)` + `RESERVES_STRUCT_TYPES
    // = ("uint112", "uint112", "uint32")`. gamma = (10000-25, 10000) = (9975, 10_000).
    #[test]
    fn pancakeswap_v2_preset_matches_python_classvars() {
        let ident = PANCAKESWAP_V2;
        assert_eq!(
            ident.factory,
            address!("1097053Fd2ea711dad45caCcc45EfF7548fCB362")
        );
        assert_eq!(ident.deployer, ident.factory);
        assert_eq!(
            ident.init_hash,
            b256!("57224589c67f3f30a6b0d7a1b54cf3153ab84563bc609ef41dfb34f8b2974d2d")
        );
        // Fraction(25, 10000) fee → RETAINED fraction (9975, 10_000).
        assert_eq!(ident.fee_token0, (9975, 10_000));
        assert_eq!(ident.fee_token1, (9975, 10_000));
        // Pancake's RESERVES_STRUCT_TYPES override = 3-tuple.
        assert_eq!(ident.reserves_abi, ReservesAbi::PancakeswapStyle);
        assert_eq!(ident.reserves_abi.as_types(), &["uint112", "uint112", "uint32"]);
    }

    // ---- SWAPBASED_V2 — Base chain 8453 ----------------------------------
    #[test]
    fn swapbased_v2_preset_matches_python_source() {
        let ident = SWAPBASED_V2;
        assert_eq!(
            ident.factory,
            address!("04C9f118d21e8B767D2e50C946f0cC9F6C367300")
        );
        assert_eq!(ident.deployer, ident.factory);
        assert_eq!(
            ident.init_hash,
            b256!("b64118b4e99d4a4163453838112a1695032df46c09f7f09064d4777d2767f8ea")
        );
        // Swapbased inherits UniswapV2Pool.FEE = Fraction(3, 1000).
        assert_eq!(ident.fee_token0, (997, 1000));
        assert_eq!(ident.fee_token1, (997, 1000));
        assert_eq!(ident.reserves_abi, ReservesAbi::Standard);
    }

    // ---- CAMELOT_V2_VOLATILE — Arbitrum 42161 + CAMELOT init hash --
    // Python: `CamelotLiquidityPool.CAMELOT_ARBITRUM_POOL_INIT_HASH` + factory
    // from `camelot/__init__.py` `_factory_address`. On-chain `token0FeePercent=300`
    // over `FEE_DENOMINATOR=100000` → 0.3% → gamma (100000-300, 100000) = (99_700, 100_000).
    #[test]
    fn camelot_v2_volatile_preset_matches_python_source() {
        let ident = CAMELOT_V2_VOLATILE;
        assert_eq!(
            ident.factory,
            address!("6EcCab422D763aC031210895C81787E87B43A652")
        );
        assert_eq!(ident.deployer, ident.factory);
        assert_eq!(
            ident.init_hash,
            b256!("a856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1")
        );
        assert_eq!(ident.fee_token0, (99_700, 100_000));
        assert_eq!(ident.fee_token1, (99_700, 100_000));
        assert_eq!(ident.reserves_abi, ReservesAbi::Standard);
    }

    // ---- CAMELOT_V2_STABLE — same deployment, stable-strategy variant ----
    #[test]
    fn camelot_v2_stable_shares_deployment_identity() {
        let stable = CAMELOT_V2_STABLE;
        let volatile = CAMELOT_V2_VOLATILE;
        // Same canonical-chain deployment (factory/deployer/init_hash).
        assert_eq!(stable.factory, volatile.factory);
        assert_eq!(stable.deployer, volatile.deployer);
        assert_eq!(stable.init_hash, volatile.init_hash);
        assert_eq!(stable.variant, DexVariant::CamelotV2Stable);
    }

    // ---- AERODROME_V2_* — Base 8453 factory + FEE_DENOMINATOR --
    // Python: `aerodrome/__init__.py` `_v2_factory_address` + `FEE_DENOMINATOR`
    // (acceptance criterion: the denominator component must equal 10000). The
    // volatile fee default (gamma) is the typical 0.3% (30/10000 → 9970/10000).
    #[test]
    fn aerodrome_v2_volatile_preset_matches_python_source() {
        let ident = AERODROME_V2_VOLATILE;
        assert_eq!(
            ident.factory,
            address!("420DD381b31aEf6683db6B902084cB0FFECe40Da")
        );
        assert_eq!(ident.deployer, ident.factory);
        // Acceptance criterion (aerodrome/pools.py FEE_DENOMINATOR=10000).
        assert_eq!(ident.fee_token0.1, 10_000);
        assert_eq!(ident.fee_token1.1, 10_000);
        assert_eq!(ident.variant, DexVariant::AerodromeV2Volatile);
    }

    #[test]
    fn aerodrome_v2_stable_shares_deployment_with_volatile() {
        let stable = AERODROME_V2_STABLE;
        let volatile = AERODROME_V2_VOLATILE;
        assert_eq!(stable.factory, volatile.factory);
        assert_eq!(stable.deployer, volatile.deployer);
        assert_eq!(stable.fee_token0.1, 10_000);
        assert_eq!(stable.fee_token1.1, 10_000);
        assert_eq!(stable.variant, DexVariant::AerodromeV2Stable);
    }

    // ---- ReservesAbi shape enumeration ------------------------------------
    #[test]
    fn reserves_abi_as_types_returns_canonical_shapes() {
        assert_eq!(ReservesAbi::Standard.as_types(), &["uint112", "uint112"]);
        assert_eq!(
            ReservesAbi::PancakeswapStyle.as_types(),
            &["uint112", "uint112", "uint32"]
        );
    }
}
