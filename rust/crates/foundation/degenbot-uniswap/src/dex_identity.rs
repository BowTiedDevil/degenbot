//! DEX identity presets — the standalone Rust-core registry.
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
//! Python seam (`py_dex_identity` in the root `degenbot_rs` cdylib) is a thin
//! read-only view.
//!
//! ## Why `DexIdentity` is NOT a `V2PoolState` field
//!
//! A pool's swap-math inputs (reserves) don't need the identity to apply Sync
//! events or solve swaps — that's invariant math. The identity is needed only
//! at *encoding* (factory → calldata recipient) and *registration* (which
//! preset constructed this). So it's a construction/encoding parameter, not
//! state (ADR-005: "identity is a construction/encoding parameter, not state").
//!
//! ## Fee convention
//!
//! `fee_tokenN: FeeSpec` declares either a **retained post-fee fraction**
//! (`gamma_numer / denominator`, e.g. `997/1000` for a 0.3% fee) — NOT the fee
//! numerator — or, for a deployment whose fee is per-pool on-chain, the
//! "read the pool live" MARKER carrying only the read's denominator. This
//! matches `V2PoolState`'s `fee_tokenN` field doc + the `IntHopState`
//! convention + `register_v2_pool`'s parameter meaning. The fee numerator (3)
//! must never be registered instead of the retained fraction (997); these
//! presets bake the correct convention in by construction (e.g.
//! `PancakeSwap`'s `Fraction(25, 10000)` FEE → preset `9975/10_000`).

use alloy::primitives::{address, b256, Address, B256};
use degenbot_core::fee::{fee_spec_marker, fee_spec_retained, FeeSpec};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// An unrecognized label for a closed variant set: the rejected input plus the
/// known labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownVariant {
    /// The rejected input, verbatim.
    pub raw: String,
    /// The known labels, in declaration order.
    pub known: Vec<&'static str>,
}

impl UnknownVariant {
    pub(crate) fn new(raw: &str, known: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            raw: raw.to_owned(),
            known: known.into_iter().collect(),
        }
    }
}

impl std::fmt::Display for UnknownVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown variant {:?}: expected one of {}",
            self.raw,
            self.known.join(", ")
        )
    }
}

impl std::error::Error for UnknownVariant {}

/// The DEX + variant discriminator for a `DexIdentity`.
///
/// Stable kebab-case string form (via [`DexVariant::as_str`]) is the canonical
/// lookup key for the Python seam (`dex_identity(variant="camelot-v2-stable")`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DexVariant {
    /// Uniswap V2 (canonical: Ethereum mainnet, chain 1).
    #[default]
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

/// A resolved DEX name (`Uniswap` vs `SushiSwap` vs `PancakeSwap` …) for a
/// pool, independent of its protocol family sub-variant.
///
/// This is the deployment-level discriminator the structural
/// [`crate::Pool`] `Identity` surfaces alongside the family sub-variant. It is
/// resolved per-`(chain, factory)` from `deployments.json` by
/// [`crate::deployments::resolve_dex_name`] (single source, Rust-owned), or
/// derived from the V2 `DexVariant` where no deployment row exists. It is
/// deliberately a small closed set so the identity stays `Copy`/`Eq`; `None`
/// means "unknown deployment → generic variant", never an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DexName {
    Uniswap,
    SushiSwap,
    PancakeSwap,
    Camelot,
    Aerodrome,
    SwapBased,
    Balancer,
}

impl DexName {
    /// Canonical kebab-case string (e.g. `"sushiswap"`) for display / the
    /// Python FFI surface.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uniswap => "uniswap",
            Self::SushiSwap => "sushiswap",
            Self::PancakeSwap => "pancakeswap",
            Self::Camelot => "camelot",
            Self::Aerodrome => "aerodrome",
            Self::SwapBased => "swapbased",
            Self::Balancer => "balancer",
        }
    }
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

impl TryFrom<&str> for DexVariant {
    type Error = UnknownVariant;

    /// Parse from the canonical kebab-case form (the inverse of [`as_str`]).
    /// Case-insensitive.
    ///
    /// # Errors
    ///
    /// [`UnknownVariant`] for a label outside the closed set, naming the raw
    /// input and the known labels: a variant string that names no preset is
    /// caller drift, never an absent lookup.
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s.to_ascii_lowercase().as_str() {
            "uniswap-v2" => Ok(Self::UniswapV2),
            "sushiswap-v2" => Ok(Self::SushiswapV2),
            "pancakeswap-v2" => Ok(Self::PancakeswapV2),
            "swapbased-v2" => Ok(Self::SwapbasedV2),
            "camelot-v2-volatile" => Ok(Self::CamelotV2Volatile),
            "camelot-v2-stable" => Ok(Self::CamelotV2Stable),
            "aerodrome-v2-volatile" => Ok(Self::AerodromeV2Volatile),
            "aerodrome-v2-stable" => Ok(Self::AerodromeV2Stable),
            _ => Err(UnknownVariant::new(
                s,
                Self::ALL.iter().map(|variant| variant.as_str()),
            )),
        }
    }
}

/// The ABI struct-type shape a DEX uses for its `Sync`-event reserve decoding.
///
/// The reserves-struct type per DEX (defaulted, with per-DEX overrides).
/// Decoupled from `DexVariant` because a future DEX could share a
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
/// `fee_tokenN` is a [`FeeSpec`]: either a declared RETAINED ratio (the share
/// the swapper keeps — `gamma_numer / denominator`, the `IntHopState` /
/// `register_v2_pool` convention) or the per-pool "read the fee live" marker,
/// which carries only the on-chain read's denominator and is never a ratio.
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
    /// `token0→token1` fee declaration: a retained ratio (`997/1000` for a
    /// 0.3% fee) or the per-pool marker for a deployment whose fee is read
    /// on-chain.
    pub fee_token0: FeeSpec,
    /// `token1→token0` fee declaration. See [`Self::fee_token0`].
    pub fee_token1: FeeSpec,
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

/// Look up the `DexIdentity` preset for a variant label (kebab-case).
///
/// # Errors
///
/// [`UnknownVariant`] for a label outside the closed set, naming the raw
/// label and the known labels — a Python caller that cannot name a preset is
/// surfaced its error, never handed a silently-absent lookup.
pub fn preset(variant: &str) -> Result<DexIdentity, UnknownVariant> {
    let v = DexVariant::try_from(variant)?;
    Ok(preset_for_variant(v))
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
    fee_token0: fee_spec_retained(997, 1000),
    fee_token1: fee_spec_retained(997, 1000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::UniswapV2,
};

/// `SushiSwap` factory (Ethereum mainnet, chain 1).
pub const SUSHISWAP_V2: DexIdentity = DexIdentity {
    factory: address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
    deployer: address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
    init_hash: b256!("e18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"),
    fee_token0: fee_spec_retained(997, 1000),
    fee_token1: fee_spec_retained(997, 1000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::SushiswapV2,
};

/// `PancakeSwap` factory (Ethereum mainnet V2-style, chain 1).
pub const PANCAKESWAP_V2: DexIdentity = DexIdentity {
    factory: address!("1097053Fd2ea711dad45caCcc45EfF7548fCB362"),
    deployer: address!("1097053Fd2ea711dad45caCcc45EfF7548fCB362"),
    init_hash: b256!("57224589c67f3f30a6b0d7a1b54cf3153ab84563bc609ef41dfb34f8b2974d2d"),
    fee_token0: fee_spec_retained(9975, 10_000),
    fee_token1: fee_spec_retained(9975, 10_000),
    reserves_abi: ReservesAbi::PancakeswapStyle,
    variant: DexVariant::PancakeswapV2,
};

/// `SwapBased` factory (Base, chain 8453).
pub const SWAPBASED_V2: DexIdentity = DexIdentity {
    factory: address!("04C9f118d21e8B767D2e50C946f0cC9F6C367300"),
    deployer: address!("04C9f118d21e8B767D2e50C946f0cC9F6C367300"),
    init_hash: b256!("b64118b4e99d4a4163453838112a1695032df46c09f7f09064d4777d2767f8ea"),
    fee_token0: fee_spec_retained(997, 1000),
    fee_token1: fee_spec_retained(997, 1000),
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
    fee_token0: fee_spec_retained(99_700, 100_000),
    fee_token1: fee_spec_retained(99_700, 100_000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::CamelotV2Volatile,
};

/// Camelot V2 stable. Fee is per-pool on-chain; the preset declares NO fee —
/// it carries the per-pool MARKER, whose only payload is the denominator
/// Camelot's `FEE_DENOMINATOR()` read reports.
///
/// The marker is never a quotable ratio: `build_v2` reads the pool's
/// `token0FeePercent()`/`token1FeePercent()` over its own `FEE_DENOMINATOR()`
/// and registers those as `FeeRatio`s, and a marker has no `FeeRatio` to hand
/// the registration door — so it cannot reach a pool's state.
pub const CAMELOT_V2_STABLE: DexIdentity = DexIdentity {
    factory: address!("6EcCab422D763aC031210895C81787E87B43A652"),
    deployer: address!("6EcCab422D763aC031210895C81787E87B43A652"),
    init_hash: b256!("a856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1"),
    fee_token0: fee_spec_marker(100_000),
    fee_token1: fee_spec_marker(100_000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::CamelotV2Stable,
};

/// Aerodrome V2 volatile (canonical Base, chain 8453). Fee is per-pool
/// on-chain (`getFee` over `FEE_DENOMINATOR=10000`), so the preset declares NO
/// fee: it carries the per-pool marker with that denominator, and the
/// Aerodrome door reads the live fee. Init hash is empty in the Python source
/// (different address generation — documented here as zero with a
/// `factory`-only budget; encoding/address-derivation for Aerodrome is carried
/// by a different path).
pub const AERODROME_V2_VOLATILE: DexIdentity = DexIdentity {
    factory: address!("420DD381b31aEf6683db6B902084cB0FFECe40Da"),
    deployer: address!("420DD381b31aEf6683db6B902084cB0FFECe40Da"),
    init_hash: B256::ZERO,
    fee_token0: fee_spec_marker(10_000),
    fee_token1: fee_spec_marker(10_000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::AerodromeV2Volatile,
};

/// Aerodrome V2 stable. Init hash is empty in the Python source (different
/// address generation); the fee is per-pool on-chain, so the preset declares
/// none — the marker carries Aerodrome's `FEE_DENOMINATOR` (`10_000`).
pub const AERODROME_V2_STABLE: DexIdentity = DexIdentity {
    factory: address!("420DD381b31aEf6683db6B902084cB0FFECe40Da"),
    deployer: address!("420DD381b31aEf6683db6B902084cB0FFECe40Da"),
    init_hash: B256::ZERO,
    fee_token0: fee_spec_marker(10_000),
    fee_token1: fee_spec_marker(10_000),
    reserves_abi: ReservesAbi::Standard,
    variant: DexVariant::AerodromeV2Stable,
};

#[cfg(test)]
#[expect(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    // ---- DexVariant round-trip --------------------------------------------

    #[test]
    fn variant_str_round_trips_for_all_variants() {
        for v in DexVariant::ALL {
            let s = v.as_str();
            assert_eq!(DexVariant::try_from(s), Ok(v), "round-trip failed for {s}");
        }
    }

    #[test]
    fn variant_from_str_is_case_insensitive() {
        assert_eq!(
            DexVariant::try_from("UNISWAP-V2"),
            Ok(DexVariant::UniswapV2)
        );
        assert_eq!(
            DexVariant::try_from("Camelot-V2-Stable"),
            Ok(DexVariant::CamelotV2Stable)
        );
    }

    #[test]
    fn variant_from_unknown_label_names_the_raw_value_and_known_set() {
        for label in ["curve", "", "uniswap"] {
            let err = DexVariant::try_from(label).unwrap_err();
            assert_eq!(err.raw, label);
            let message = err.to_string();
            assert!(
                message.contains(&format!("{label:?}")),
                "raw value {label:?} missing from: {message}"
            );
            assert!(
                message.contains("uniswap-v2") && message.contains("aerodrome-v2-stable"),
                "known set missing from: {message}"
            );
        }
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
    fn preset_for_unknown_string_names_the_label() {
        let err = preset("nonexistent-dex").unwrap_err();
        assert_eq!(err.raw, "nonexistent-dex");
        assert_eq!(err.known.len(), DexVariant::ALL.len());
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
        // gamma_numer is the kept amount, NOT the fee.
        assert_eq!(ident.fee_token0.as_pair(), (997, 1000));
        assert_eq!(ident.fee_token1.as_pair(), (997, 1000));
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
        assert_eq!(ident.fee_token0.as_pair(), (997, 1000)); // 3/1000 fee
        assert_eq!(ident.fee_token1.as_pair(), (997, 1000));
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
        assert_eq!(ident.fee_token0.as_pair(), (9975, 10_000));
        assert_eq!(ident.fee_token1.as_pair(), (9975, 10_000));
        // Pancake's RESERVES_STRUCT_TYPES override = 3-tuple.
        assert_eq!(ident.reserves_abi, ReservesAbi::PancakeswapStyle);
        assert_eq!(
            ident.reserves_abi.as_types(),
            &["uint112", "uint112", "uint32"]
        );
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
        assert_eq!(ident.fee_token0.as_pair(), (997, 1000));
        assert_eq!(ident.fee_token1.as_pair(), (997, 1000));
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
        assert_eq!(ident.fee_token0.as_pair(), (99_700, 100_000));
        assert_eq!(ident.fee_token1.as_pair(), (99_700, 100_000));
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
        assert_eq!(ident.fee_token0.denominator(), 10_000);
        assert_eq!(ident.fee_token1.denominator(), 10_000);
        // The volatile fee is read per-pool (`getFee`), so the preset declares
        // no ratio at all: the marker is not a fee a pool could register with.
        assert_eq!(ident.fee_token0.ratio(), None);
        assert_eq!(ident.fee_token1.ratio(), None);
        assert_eq!(ident.variant, DexVariant::AerodromeV2Volatile);
    }

    #[test]
    fn aerodrome_v2_stable_shares_deployment_with_volatile() {
        let stable = AERODROME_V2_STABLE;
        let volatile = AERODROME_V2_VOLATILE;
        assert_eq!(stable.factory, volatile.factory);
        assert_eq!(stable.deployer, volatile.deployer);
        assert_eq!(stable.fee_token0.denominator(), 10_000);
        assert_eq!(stable.fee_token1.denominator(), 10_000);
        assert_eq!(stable.fee_token0.ratio(), None);
        assert_eq!(stable.fee_token1.ratio(), None);
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
