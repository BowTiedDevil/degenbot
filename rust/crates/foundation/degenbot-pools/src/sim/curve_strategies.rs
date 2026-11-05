//! Curve pool strategy + variant resolution.
//!
//! The address->strategy dispatch, as pure-Rust static maps producing the typed
//! discriminants that [`crate::state::curve_state::RegisterCurvePoolParams`]
//! stores as bytes. These maps are the live authority: a pool receiving these
//! discriminants at construction does not re-resolve its own address. Frozen at
//! construction time; the Rust calc path (the swap engine) later dispatches on
//! these values. No I/O, no `pyo3` — a standalone `cargo add degenbot` consumer
//! resolves strategies directly.
//!
//! ## Discriminant values (single source of truth)
//!
//! Every axis below is a [`degenbot_math::curve`] enum whose explicit
//! discriminant IS the byte the pool identity stores (the Python `auto()` order):
//! [`DVariant`], [`YVariant`], [`YDVariant`], [`SwapStyle`],
//! [`LendingRateStyle`], [`MetapoolRateStyle`], [`MetapoolUnderlyingStyle`]. The
//! enum declarations are the only place the numbers live; an axis a table entry
//! does not name keeps its `Standard`/`None` default, which is the same
//! discriminant as an unmapped address.
//!
//! ## Provenance
//!
//! Preserves the Python `PROVENANCE WARNING`: this transliterates the address-
//! based dispatch without re-verifying each address against on-chain source,
//! and an unlisted address falls through to the `STANDARD`/`NONE` defaults.

use alloy::primitives::{address, Address};
use degenbot_math::curve::{
    DVariant, LendingRateStyle, MetapoolRateStyle, MetapoolUnderlyingStyle, SwapStyle, YDVariant,
    YVariant,
};

/// The strategy discriminants one pool's identity stores, one typed field per
/// axis. A field a table entry does not name stays at its default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CurveStrategyDiscriminants {
    /// Which D-step pair the stableswap `get_D` walks.
    pub d_variant: DVariant,
    /// Which Y-step the stableswap `get_y` walks.
    pub y_variant: YVariant,
    /// Which Y_D-step the stableswap `get_y_d` walks.
    pub yd_variant: YDVariant,
    /// Which `get_dy` computation path the pool uses.
    pub swap_style: SwapStyle,
    /// Which stored-rate path supplies a lending token's rate.
    pub lending_rate_style: LendingRateStyle,
    /// How a metapool builds its `get_dy` rate tuple.
    pub metapool_rate_style: MetapoolRateStyle,
    /// How a metapool walks `get_dy_underlying`.
    pub metapool_underlying_style: MetapoolUnderlyingStyle,
}

// ---------------------------------------------------------------------------
// Variant groups: address membership selects the D/Y/Y_D variant discriminant.
// ---------------------------------------------------------------------------

/// D-variant group 0 → `DVariant::VARIANT_ALPHA` (2).
const D_GROUP_0: [Address; 6] = [
    address!("0x06364f10B501e868329afBc005b3492902d6C763"),
    address!("0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F"),
    address!("0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C"),
    address!("0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714"),
    address!("0x93054188d876f558f4a66B2EF1d97d16eDf0895B"),
    address!("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"),
];

/// D-variant group 1 → `DVariant::VARIANT_ALPHA_DP_ALPHA` (3).
const D_GROUP_1: [Address; 4] = [
    address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51"),
    address!("0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27"),
    address!("0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56"),
    address!("0xA5407eAE9Ba41422680e2e00537571bcC53efBfD"),
];

/// D-variant group 2 → `DVariant::VARIANT_BETA_DP` (5).
const D_GROUP_2: [Address; 17] = [
    address!("0x0AD66FeC8dB84F8A3365ADA04aB23ce607ac6E24"),
    address!("0x1c899dED01954d0959E034b62a728e7fEbE593b0"),
    address!("0x3F1B0278A9ee595635B61817630cC19DE792f506"),
    address!("0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a"),
    address!("0x447Ddd4960d9fdBF6af9a790560d0AF76795CB08"),
    address!("0x453D92C7d4263201C69aACfaf589Ed14202d83a4"),
    address!("0x663aC72a1c3E1C4186CD3dCb184f216291F4878C"),
    address!("0x6A274dE3e2462c7614702474D64d376729831dCa"),
    address!("0x7C0d189E1FecB124487226dCbA3748bD758F98E4"),
    address!("0x875DF0bA24ccD867f8217593ee27253280772A97"),
    address!("0x99f5aCc8EC2Da2BC0771c32814EFF52b712de1E5"),
    address!("0x9D0464996170c6B9e75eED71c68B99dDEDf279e8"),
    address!("0xB37D6c07482Bc11cd28a1f11f1a6ad7b66Dec933"),
    address!("0xB657B895B265C38c53FFF00166cF7F6A3C70587d"),
    address!("0xD6Ac1CB9019137a896343Da59dDE6d097F710538"),
    address!("0xE95E4c2dAC312F31Dc605533D5A4d0aF42579308"),
    address!("0xf7b55C3732aD8b2c2dA7c24f30A69f55c54FB717"),
];

/// D-variant group 3 → `DVariant::VARIANT_DP_ALPHA` (4).
const D_GROUP_3: [Address; 3] = [
    address!("0xDC24316b9AE028F1497c275EB9192a3Ea0f67022"),
    address!("0xDeBF20617708857ebe4F679508E7b7863a8A8EeE"),
    address!("0xEB16Ae0052ed37f479f7fe63849198Df1765a733"),
];

/// D-variant group 4 → `DVariant::VARIANT_GAMMA_DP` (6).
const D_GROUP_4: [Address; 10] = [
    address!("0x1062FD8eD633c1f080754c19317cb3912810B5e5"),
    address!("0x1C5F80b6B68A9E1Ef25926EeE00b5255791b996B"),
    address!("0x26f3f26F46cBeE59d1F8860865e13Aa39e36A8c0"),
    address!("0x2d600BbBcC3F1B6Cb9910A70BaB59eC9d5F81B9A"),
    address!("0x320B564Fb9CF36933eC507a846ce230008631fd3"),
    address!("0x3b21C2868B6028CfB38Ff86127eF22E68d16d53B"),
    address!("0x69ACcb968B19a53790f43e57558F5E443A91aF22"),
    address!("0x971add32Ea87f10bD192671630be3BE8A11b8623"),
    address!("0xCA0253A98D16e9C1e3614caFDA19318EE69772D0"),
    address!("0xfBB481A443382416357fA81F16dB5A725DC6ceC8"),
];

/// Y-variant group 0 → `YVariant::VARIANT_0` (2). Subset of `Y_GROUP_1`.
const Y_GROUP_0: [Address; 5] = [
    address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51"),
    address!("0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C"),
    address!("0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27"),
    address!("0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56"),
    address!("0xA5407eAE9Ba41422680e2e00537571bcC53efBfD"),
];

/// Y-variant group 1 → `YVariant::VARIANT_1` (3).
const Y_GROUP_1: [Address; 10] = [
    address!("0x06364f10B501e868329afBc005b3492902d6C763"),
    address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51"),
    address!("0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F"),
    address!("0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C"),
    address!("0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27"),
    address!("0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714"),
    address!("0x93054188d876f558f4a66B2EF1d97d16eDf0895B"),
    address!("0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56"),
    address!("0xA5407eAE9Ba41422680e2e00537571bcC53efBfD"),
    address!("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"),
];

/// Y_D-variant group 0 → `YDVariant::VARIANT_0` (2).
const YD_GROUP_0: [Address; 2] = [
    address!("0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2"),
    address!("0xf253f83AcA21aAbD2A20553AE0BF7F65C755A07F"),
];

/// Resolve the D-step variant for a pool address.
#[must_use]
pub fn resolve_d_variant(addr: Address) -> DVariant {
    if D_GROUP_0.contains(&addr) {
        DVariant::VariantAlpha
    } else if D_GROUP_1.contains(&addr) {
        DVariant::VariantAlphaDpAlpha
    } else if D_GROUP_2.contains(&addr) {
        DVariant::VariantBetaDp
    } else if D_GROUP_3.contains(&addr) {
        DVariant::VariantDpAlpha
    } else if D_GROUP_4.contains(&addr) {
        DVariant::VariantGammaDp
    } else {
        DVariant::Standard
    }
}

/// Resolve the Y-step variant for a pool address. `Y_GROUP_0` is a subset of
/// `Y_GROUP_1`, so `group0` wins over `group1`.
#[must_use]
pub fn resolve_y_variant(addr: Address) -> YVariant {
    if Y_GROUP_0.contains(&addr) {
        YVariant::Variant0
    } else if Y_GROUP_1.contains(&addr) {
        YVariant::Variant1
    } else {
        YVariant::Standard
    }
}

/// Resolve the Y_D-step variant for a pool address.
#[must_use]
pub fn resolve_yd_variant(addr: Address) -> YDVariant {
    if YD_GROUP_0.contains(&addr) {
        YDVariant::Variant0
    } else {
        YDVariant::Standard
    }
}

// ---------------------------------------------------------------------------
// Address -> strategy table. Each group is the audited set of pools sharing one
// combination of the four non-variant axes; a group names ONLY the axes it
// changes, so an entry cannot silently land in the wrong tuple position.
// ---------------------------------------------------------------------------

/// The four non-variant strategy axes for one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Strategies {
    swap: SwapStyle,
    lending: LendingRateStyle,
    metapool_rate: MetapoolRateStyle,
    metapool_underlying: MetapoolUnderlyingStyle,
}

impl Strategies {
    /// Every axis at its default — what an unmapped address resolves to.
    const DEFAULT: Self = Self {
        swap: SwapStyle::Standard,
        lending: LendingRateStyle::None,
        metapool_rate: MetapoolRateStyle::Standard,
        metapool_underlying: MetapoolUnderlyingStyle::Standard,
    };

    /// Only the swap style moves off its default.
    const fn swap(swap: SwapStyle) -> Self {
        Self {
            swap,
            ..Self::DEFAULT
        }
    }

    /// The swap style plus the lending-rate source.
    const fn swap_and_lending(swap: SwapStyle, lending: LendingRateStyle) -> Self {
        Self {
            swap,
            lending,
            ..Self::DEFAULT
        }
    }

    /// The two metapool axes (the swap/lending axes stay at their defaults).
    const fn metapool(
        metapool_rate: MetapoolRateStyle,
        metapool_underlying: MetapoolUnderlyingStyle,
    ) -> Self {
        Self {
            metapool_rate,
            metapool_underlying,
            ..Self::DEFAULT
        }
    }
}

/// Metapool rate style: PRECISION + virtual price for both the rate tuple and
/// the underlying path.
const METAPOOL_PRECISION_VP_UNDERLYING_PRECISION: [Address; 2] = [
    address!("0xC61557C5d177bd7DC889A3b621eEC333e168f68A"),
    address!("0x4606326b4Db89373F5377C316d3b0F6e55Bc6A20"),
];

/// Metapool rate style: redemption price + virtual price for the rate tuple,
/// the redemption-price underlying path.
const METAPOOL_REDEMPTION_VP_UNDERLYING_REDEMPTION: [Address; 1] =
    [address!("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d")];

/// Live balances minus admin balances.
const LIVE_ADMIN: [Address; 5] = [
    address!("0x4e0915C88bC70750D68C481540F081fEFaF22273"),
    address!("0x1005F7406f32a61BD760CfA14aCCd2737913d546"),
    address!("0x6A274dE3e2462c7614702474D64d376729831dCa"),
    address!("0xb9446c4Ef5EBE66268dA6700D26f96273DE3d571"),
    address!("0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a"),
];

/// Crypto (Newton) pools.
const CRYPTO: [Address; 1] = [address!("0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5")];

/// Rate-adjusted fee style.
const RATE_ADJUSTED: [Address; 4] = [
    address!("0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F"),
    address!("0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714"),
    address!("0x93054188d876f558f4a66B2EF1d97d16eDf0895B"),
    address!("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"),
];

/// Standard rate style (audited list; every axis at its default).
const STANDARD_RATE: [Address; 31] = [
    address!("0x0Ce6a5fF5217e38315f87032CF90686C96627CAA"),
    address!("0x19b080FE1ffA0553469D20Ca36219F17Fcf03859"),
    address!("0x1C5F80b6B68A9E1Ef25926EeE00b5255791b996B"),
    address!("0x1F6bb2a7a2A84d08bb821B89E38cA651175aeDd4"),
    address!("0x21B45B2c1C53fDFe378Ed1955E8Cc29aE8cE0132"),
    address!("0x3CFAa1596777CAD9f5004F9a0c443d912E262243"),
    address!("0x3F1B0278A9ee595635B61817630cC19DE792f506"),
    address!("0x4424b4A37ba0088D8a718b8fc2aB7952C7e695F5"),
    address!("0x602a9Abb10582768Fd8a9f13aD6316Ac2A5A2e2B"),
    address!("0x8461A004b50d321CB22B7d034969cE6803911899"),
    address!("0x857110B5f8eFD66CC3762abb935315630AC770B5"),
    address!("0x8818a9bb44Fbf33502bE7c15c500d0C783B73067"),
    address!("0x9c2C8910F113181783c249d8F6Aa41b51Cde0f0c"),
    address!("0xa1F8A6807c402E4A15ef4EBa36528A3FED24E577"),
    address!("0xaE34574AC03A15cd58A92DC79De7B1A0800F1CE3"),
    address!("0xAf25fFe6bA5A8a29665adCfA6D30C5Ae56CA0Cd3"),
    address!("0xBa3436Fd341F2C8A928452Db3C5A3670d1d5Cc73"),
    address!("0xbB2dC673E1091abCA3eaDB622b18f6D4634b2CD9"),
    address!("0xc5424B857f758E906013F3555Dad202e4bdB4567"),
    address!("0xc8a7C1c4B748970F57cA59326BcD49F5c9dc43E3"),
    address!("0xcbD5cC53C5b846671C6434Ab301AD4d210c21184"),
    address!("0xD6Ac1CB9019137a896343Da59dDE6d097F710538"),
    address!("0xD7C10449A6D134A9ed37e2922F8474EAc6E5c100"),
    address!("0xDC24316b9AE028F1497c275EB9192a3Ea0f67022"),
    address!("0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2"),
    address!("0xe7A3b38c39F97E977723bd1239C3470702568e7B"),
    address!("0xf083FBa98dED0f9C970e5a418500bad08D8b9732"),
    address!("0xF178C0b5Bb7e7aBF4e12A4838C7b7c5bA2C623c0"),
    address!("0xf253f83AcA21aAbD2A20553AE0BF7F65C755A07F"),
    address!("0xfC8c34a3B3CFE1F1Dd6DBCCEC4BC5d3103b80FF0"),
    address!("0xFD5dB7463a3aB53fD211b4af195c5BCCC1A03890"),
];

/// Raw balance style (no rate conversion).
const RAW_BALANCE: [Address; 10] = [
    address!("0x04c90C198b2eFF55716079bc06d7CCc4aa4d7512"),
    address!("0x320B564Fb9CF36933eC507a846ce230008631fd3"),
    address!("0x48fF31bBbD8Ab553Ebe7cBD84e1eA3dBa8f54957"),
    address!("0x55A8a39bc9694714E2874c1ce77aa1E599461E18"),
    address!("0x875DF0bA24ccD867f8217593ee27253280772A97"),
    address!("0x9D0464996170c6B9e75eED71c68B99dDEDf279e8"),
    address!("0xBaaa1F5DbA42C3389bDbc2c9D2dE134F5cD0Dc89"),
    address!("0xDa5B670CcD418a187a3066674A8002Adc9356Ad1"),
    address!("0xf03bD3cfE85f00bF5819AC20f0870cE8a8d1F0D8"),
    address!("0xFB9a265b5a1f52d97838Ec7274A0b1442efAcC87"),
];

/// Live admin balances with oracle-supplied lending rates.
const LIVE_ADMIN_ORACLE: [Address; 2] = [
    address!("0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492"),
    address!("0xBfAb6FA95E0091ed66058ad493189D2cB29385E6"),
];

/// Rate-adjusted (no `- 1`) over cToken lending rates.
const RATE_ADJUSTED_NO_ONE_CTOKEN: [Address; 2] = [
    address!("0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C"),
    address!("0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56"),
];

/// Rate-adjusted (no `- 1`) fee style.
const RATE_ADJUSTED_NO_ONE: [Address; 1] = [address!("0xA5407eAE9Ba41422680e2e00537571bcC53efBfD")];

/// cToken+yToken combined accrual lending rates.
const CYTOKEN: [Address; 1] = [address!("0x2dded6Da1BF5DBdF597C45fcFaa3194e53EcfeAF")];

/// Rate-adjusted over yToken price-per-share lending rates.
const RATE_ADJUSTED_YTOKEN: [Address; 1] = [address!("0x06364f10B501e868329afBc005b3492902d6C763")];

/// Rate-adjusted (no `- 1`) over yToken lending rates.
const RATE_ADJUSTED_NO_ONE_YTOKEN: [Address; 2] = [
    address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51"),
    address!("0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27"),
];

/// No `- 1` fee style over ankrETH ratio lending rates.
const NO_ONE_FEE_RATE_AETH: [Address; 1] = [address!("0xA96A65c051bF88B4095Ee1f2451C2A9d43F53Ae2")];

/// No `- 1` fee style over rETH oracle lending rates.
const NO_ONE_FEE_RATE_RETH: [Address; 1] = [address!("0xF9440930043eb3997fc70e1339dBb11F341de7A8")];

/// Live admin balances with the dynamic offpeg fee.
const LIVE_ADMIN_DYNAMIC: [Address; 1] = [address!("0xEB16Ae0052ed37f479f7fe63849198Df1765a733")];

/// Live admin balances, precision multipliers, dynamic offpeg fee.
const LIVE_ADMIN_DYNAMIC_PRECISION: [Address; 1] =
    [address!("0xDeBF20617708857ebe4F679508E7b7863a8A8EeE")];
/// Look up the address-specific (non-variant) strategy axes.
///
/// `None` for an unmapped address, which resolves to every axis' default.
fn mapped_strategies(addr: Address) -> Option<Strategies> {
    if METAPOOL_PRECISION_VP_UNDERLYING_PRECISION.contains(&addr) {
        Some(Strategies::metapool(
            MetapoolRateStyle::PrecisionVp,
            MetapoolUnderlyingStyle::PrecisionVp,
        ))
    } else if METAPOOL_REDEMPTION_VP_UNDERLYING_REDEMPTION.contains(&addr) {
        Some(Strategies::metapool(
            MetapoolRateStyle::RedemptionVp,
            MetapoolUnderlyingStyle::Redemption,
        ))
    } else if LIVE_ADMIN.contains(&addr) {
        Some(Strategies::swap(SwapStyle::LiveAdmin))
    } else if CRYPTO.contains(&addr) {
        Some(Strategies::swap(SwapStyle::Crypto))
    } else if RATE_ADJUSTED.contains(&addr) {
        Some(Strategies::swap(SwapStyle::RateAdjusted))
    } else if STANDARD_RATE.contains(&addr) {
        Some(Strategies::DEFAULT)
    } else if RAW_BALANCE.contains(&addr) {
        Some(Strategies::swap(SwapStyle::RawBalance))
    } else if LIVE_ADMIN_ORACLE.contains(&addr) {
        Some(Strategies::swap_and_lending(
            SwapStyle::LiveAdminOracle,
            LendingRateStyle::Oracle,
        ))
    } else if RATE_ADJUSTED_NO_ONE_CTOKEN.contains(&addr) {
        Some(Strategies::swap_and_lending(
            SwapStyle::RateAdjustedNoOne,
            LendingRateStyle::Ctoken,
        ))
    } else if RATE_ADJUSTED_NO_ONE.contains(&addr) {
        Some(Strategies::swap(SwapStyle::RateAdjustedNoOne))
    } else if CYTOKEN.contains(&addr) {
        Some(Strategies::swap_and_lending(
            SwapStyle::CyToken,
            LendingRateStyle::CyToken,
        ))
    } else if RATE_ADJUSTED_YTOKEN.contains(&addr) {
        Some(Strategies::swap_and_lending(
            SwapStyle::RateAdjusted,
            LendingRateStyle::Ytoken,
        ))
    } else if RATE_ADJUSTED_NO_ONE_YTOKEN.contains(&addr) {
        Some(Strategies::swap_and_lending(
            SwapStyle::RateAdjustedNoOne,
            LendingRateStyle::Ytoken,
        ))
    } else if NO_ONE_FEE_RATE_AETH.contains(&addr) {
        Some(Strategies::swap_and_lending(
            SwapStyle::NoOneFeeRate,
            LendingRateStyle::Aeth,
        ))
    } else if NO_ONE_FEE_RATE_RETH.contains(&addr) {
        Some(Strategies::swap_and_lending(
            SwapStyle::NoOneFeeRate,
            LendingRateStyle::Reth,
        ))
    } else if LIVE_ADMIN_DYNAMIC.contains(&addr) {
        Some(Strategies::swap(SwapStyle::LiveAdminDynamic))
    } else if LIVE_ADMIN_DYNAMIC_PRECISION.contains(&addr) {
        Some(Strategies::swap(SwapStyle::LiveAdminDynamicPrecision))
    } else {
        None
    }
}

/// Resolve the complete strategy-discriminant set for a pool address.
///
/// Combines the variant-group resolution (d/y/yd) with the address-specific
/// table. Unlisted pools fall through to the `Standard`/`None` defaults.
#[must_use]
pub fn resolve_curve_strategy_discriminants(addr: Address) -> CurveStrategyDiscriminants {
    let mapped = mapped_strategies(addr).unwrap_or(Strategies::DEFAULT);
    CurveStrategyDiscriminants {
        d_variant: resolve_d_variant(addr),
        y_variant: resolve_y_variant(addr),
        yd_variant: resolve_yd_variant(addr),
        swap_style: mapped.swap,
        lending_rate_style: mapped.lending,
        metapool_rate_style: mapped.metapool_rate,
        metapool_underlying_style: mapped.metapool_underlying,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d_variant_groups_resolve_to_their_variant() {
        // One address from each group + the STANDARD default.
        assert_eq!(
            resolve_d_variant(address!("0x06364f10B501e868329afBc005b3492902d6C763")),
            DVariant::VariantAlpha
        );
        assert_eq!(
            resolve_d_variant(address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51")),
            DVariant::VariantAlphaDpAlpha
        );
        assert_eq!(
            resolve_d_variant(address!("0x0AD66FeC8dB84F8A3365ADA04aB23ce607ac6E24")),
            DVariant::VariantBetaDp
        );
        assert_eq!(
            resolve_d_variant(address!("0xDC24316b9AE028F1497c275EB9192a3Ea0f67022")),
            DVariant::VariantDpAlpha
        );
        assert_eq!(
            resolve_d_variant(address!("0x1062FD8eD633c1f080754c19317cb3912810B5e5")),
            DVariant::VariantGammaDp
        );
        assert_eq!(
            resolve_d_variant(address!("0x2222222222222222222222222222222222222222")),
            DVariant::Standard
        );
    }

    #[test]
    fn y_variant_group0_wins_over_group1() {
        // In both groups (group0 is a subset of group1) -> VARIANT_0.
        assert_eq!(
            resolve_y_variant(address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51")),
            YVariant::Variant0
        );
        // group1 only.
        assert_eq!(
            resolve_y_variant(address!("0x06364f10B501e868329afBc005b3492902d6C763")),
            YVariant::Variant1
        );
        // neither.
        assert_eq!(
            resolve_y_variant(address!("0x3F1B0278A9ee595635B61817630cC19DE792f506")),
            YVariant::Standard
        );
    }

    #[test]
    fn yd_variant_resolves() {
        assert_eq!(
            resolve_yd_variant(address!("0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2")),
            YDVariant::Variant0
        );
        assert_eq!(
            resolve_yd_variant(address!("0x3F1B0278A9ee595635B61817630cC19DE792f506")),
            YDVariant::Standard
        );
    }

    #[test]
    fn unmapped_address_is_all_defaults() {
        let unknown = address!("0x1111111111111111111111111111111111111111");
        assert_eq!(
            resolve_curve_strategy_discriminants(unknown),
            CurveStrategyDiscriminants::default()
        );
    }

    #[test]
    fn mapped_address_merges_variant_groups_and_strategies() {
        // 0xbEbc4478 (tripool): D-group0 + Y-group1 + RATE_ADJUSTED.
        let s = resolve_curve_strategy_discriminants(address!(
            "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"
        ));
        assert_eq!(s.d_variant, DVariant::VariantAlpha);
        assert_eq!(s.y_variant, YVariant::Variant1);
        assert_eq!(s.yd_variant, YDVariant::Standard);
        assert_eq!(s.swap_style, SwapStyle::RateAdjusted);
        assert_eq!(s.lending_rate_style, LendingRateStyle::None);
    }

    #[test]
    fn metapool_style_overrides_defaults() {
        let s = resolve_curve_strategy_discriminants(address!(
            "0x618788357D0EBd8A37e763ADab3bc575D54c2C7d"
        ));
        assert_eq!(s.metapool_rate_style, MetapoolRateStyle::RedemptionVp);
        assert_eq!(
            s.metapool_underlying_style,
            MetapoolUnderlyingStyle::Redemption
        );
        assert_eq!(s.swap_style, SwapStyle::Standard);
    }

    #[test]
    fn crypto_and_leverage_pools() {
        let crypto = resolve_curve_strategy_discriminants(address!(
            "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5"
        ));
        assert_eq!(crypto.swap_style, SwapStyle::Crypto);

        let ct = resolve_curve_strategy_discriminants(address!(
            "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C"
        ));
        assert_eq!(ct.swap_style, SwapStyle::RateAdjustedNoOne);
        assert_eq!(ct.lending_rate_style, LendingRateStyle::Ctoken);
    }

    /// One row's four address-specific axes, in the order the fixture pins them
    /// (`swap_style`, `lending_rate_style`, `metapool_rate_style`,
    /// `metapool_underlying_style`). Typed, not bytes: the axes a resolved pool
    /// carries ARE these enums, and the byte spelling is the enums' own
    /// discriminants (`style_discriminants_round_trip_through_the_stored_byte`
    /// in `degenbot-math` pins the encode/decode round trip).
    type Axes = (
        SwapStyle,
        LendingRateStyle,
        MetapoolRateStyle,
        MetapoolUnderlyingStyle,
    );

    /// An unlisted address' axes — every axis' default, which is what the
    /// resolver falls through to.
    const DEFAULT_AXES: Axes = (
        SwapStyle::Standard,
        LendingRateStyle::None,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );

    // One const per strategy group the address tables name, so a fixture row
    // states WHICH group the pool belongs to rather than restating four enum
    // members per address.
    const METAPOOL_PRECISION_VP: Axes = (
        SwapStyle::Standard,
        LendingRateStyle::None,
        MetapoolRateStyle::PrecisionVp,
        MetapoolUnderlyingStyle::PrecisionVp,
    );
    const METAPOOL_REDEMPTION_VP: Axes = (
        SwapStyle::Standard,
        LendingRateStyle::None,
        MetapoolRateStyle::RedemptionVp,
        MetapoolUnderlyingStyle::Redemption,
    );
    const LIVE_ADMIN: Axes = (
        SwapStyle::LiveAdmin,
        LendingRateStyle::None,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const CRYPTO: Axes = (
        SwapStyle::Crypto,
        LendingRateStyle::None,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const RATE_ADJUSTED: Axes = (
        SwapStyle::RateAdjusted,
        LendingRateStyle::None,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const RAW_BALANCE: Axes = (
        SwapStyle::RawBalance,
        LendingRateStyle::None,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const LIVE_ADMIN_ORACLE: Axes = (
        SwapStyle::LiveAdminOracle,
        LendingRateStyle::Oracle,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const RATE_ADJUSTED_NO_ONE_CTOKEN: Axes = (
        SwapStyle::RateAdjustedNoOne,
        LendingRateStyle::Ctoken,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const RATE_ADJUSTED_NO_ONE: Axes = (
        SwapStyle::RateAdjustedNoOne,
        LendingRateStyle::None,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const CYTOKEN: Axes = (
        SwapStyle::CyToken,
        LendingRateStyle::CyToken,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const RATE_ADJUSTED_YTOKEN: Axes = (
        SwapStyle::RateAdjusted,
        LendingRateStyle::Ytoken,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const RATE_ADJUSTED_NO_ONE_YTOKEN: Axes = (
        SwapStyle::RateAdjustedNoOne,
        LendingRateStyle::Ytoken,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const NO_ONE_FEE_RATE_AETH: Axes = (
        SwapStyle::NoOneFeeRate,
        LendingRateStyle::Aeth,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const NO_ONE_FEE_RATE_RETH: Axes = (
        SwapStyle::NoOneFeeRate,
        LendingRateStyle::Reth,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const LIVE_ADMIN_DYNAMIC: Axes = (
        SwapStyle::LiveAdminDynamic,
        LendingRateStyle::None,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );
    const LIVE_ADMIN_DYNAMIC_PRECISION: Axes = (
        SwapStyle::LiveAdminDynamicPrecision,
        LendingRateStyle::None,
        MetapoolRateStyle::Standard,
        MetapoolUnderlyingStyle::Standard,
    );

    /// The axes a mapped address registers with, by address: the stored
    /// discriminants are deployed pool behavior, so a pool filed under the wrong
    /// strategy group must fail here rather than re-register differently.
    // One row per address reads as the table it pins; a vertical rustfmt pass
    // would bury the pairing in 270 lines of punctuation.
    #[rustfmt::skip]
    const REGISTERED_DISCRIMINANTS: &[(Address, Axes)] = &[
        (address!("0xC61557C5d177bd7DC889A3b621eEC333e168f68A"), METAPOOL_PRECISION_VP),
        (address!("0x4606326b4Db89373F5377C316d3b0F6e55Bc6A20"), METAPOOL_PRECISION_VP),
        (address!("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d"), METAPOOL_REDEMPTION_VP),
        (address!("0x4e0915C88bC70750D68C481540F081fEFaF22273"), LIVE_ADMIN),
        (address!("0x1005F7406f32a61BD760CfA14aCCd2737913d546"), LIVE_ADMIN),
        (address!("0x6A274dE3e2462c7614702474D64d376729831dCa"), LIVE_ADMIN),
        (address!("0xb9446c4Ef5EBE66268dA6700D26f96273DE3d571"), LIVE_ADMIN),
        (address!("0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a"), LIVE_ADMIN),
        (address!("0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5"), CRYPTO),
        (address!("0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F"), RATE_ADJUSTED),
        (address!("0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714"), RATE_ADJUSTED),
        (address!("0x93054188d876f558f4a66B2EF1d97d16eDf0895B"), RATE_ADJUSTED),
        (address!("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"), RATE_ADJUSTED),
        (address!("0x0Ce6a5fF5217e38315f87032CF90686C96627CAA"), DEFAULT_AXES),
        (address!("0x19b080FE1ffA0553469D20Ca36219F17Fcf03859"), DEFAULT_AXES),
        (address!("0x1C5F80b6B68A9E1Ef25926EeE00b5255791b996B"), DEFAULT_AXES),
        (address!("0x1F6bb2a7a2A84d08bb821B89E38cA651175aeDd4"), DEFAULT_AXES),
        (address!("0x21B45B2c1C53fDFe378Ed1955E8Cc29aE8cE0132"), DEFAULT_AXES),
        (address!("0x3CFAa1596777CAD9f5004F9a0c443d912E262243"), DEFAULT_AXES),
        (address!("0x3F1B0278A9ee595635B61817630cC19DE792f506"), DEFAULT_AXES),
        (address!("0x4424b4A37ba0088D8a718b8fc2aB7952C7e695F5"), DEFAULT_AXES),
        (address!("0x602a9Abb10582768Fd8a9f13aD6316Ac2A5A2e2B"), DEFAULT_AXES),
        (address!("0x8461A004b50d321CB22B7d034969cE6803911899"), DEFAULT_AXES),
        (address!("0x857110B5f8eFD66CC3762abb935315630AC770B5"), DEFAULT_AXES),
        (address!("0x8818a9bb44Fbf33502bE7c15c500d0C783B73067"), DEFAULT_AXES),
        (address!("0x9c2C8910F113181783c249d8F6Aa41b51Cde0f0c"), DEFAULT_AXES),
        (address!("0xa1F8A6807c402E4A15ef4EBa36528A3FED24E577"), DEFAULT_AXES),
        (address!("0xaE34574AC03A15cd58A92DC79De7B1A0800F1CE3"), DEFAULT_AXES),
        (address!("0xAf25fFe6bA5A8a29665adCfA6D30C5Ae56CA0Cd3"), DEFAULT_AXES),
        (address!("0xBa3436Fd341F2C8A928452Db3C5A3670d1d5Cc73"), DEFAULT_AXES),
        (address!("0xbB2dC673E1091abCA3eaDB622b18f6D4634b2CD9"), DEFAULT_AXES),
        (address!("0xc5424B857f758E906013F3555Dad202e4bdB4567"), DEFAULT_AXES),
        (address!("0xc8a7C1c4B748970F57cA59326BcD49F5c9dc43E3"), DEFAULT_AXES),
        (address!("0xcbD5cC53C5b846671C6434Ab301AD4d210c21184"), DEFAULT_AXES),
        (address!("0xD6Ac1CB9019137a896343Da59dDE6d097F710538"), DEFAULT_AXES),
        (address!("0xD7C10449A6D134A9ed37e2922F8474EAc6E5c100"), DEFAULT_AXES),
        (address!("0xDC24316b9AE028F1497c275EB9192a3Ea0f67022"), DEFAULT_AXES),
        (address!("0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2"), DEFAULT_AXES),
        (address!("0xe7A3b38c39F97E977723bd1239C3470702568e7B"), DEFAULT_AXES),
        (address!("0xf083FBa98dED0f9C970e5a418500bad08D8b9732"), DEFAULT_AXES),
        (address!("0xF178C0b5Bb7e7aBF4e12A4838C7b7c5bA2C623c0"), DEFAULT_AXES),
        (address!("0xf253f83AcA21aAbD2A20553AE0BF7F65C755A07F"), DEFAULT_AXES),
        (address!("0xfC8c34a3B3CFE1F1Dd6DBCCEC4BC5d3103b80FF0"), DEFAULT_AXES),
        (address!("0xFD5dB7463a3aB53fD211b4af195c5BCCC1A03890"), DEFAULT_AXES),
        (address!("0x04c90C198b2eFF55716079bc06d7CCc4aa4d7512"), RAW_BALANCE),
        (address!("0x320B564Fb9CF36933eC507a846ce230008631fd3"), RAW_BALANCE),
        (address!("0x48fF31bBbD8Ab553Ebe7cBD84e1eA3dBa8f54957"), RAW_BALANCE),
        (address!("0x55A8a39bc9694714E2874c1ce77aa1E599461E18"), RAW_BALANCE),
        (address!("0x875DF0bA24ccD867f8217593ee27253280772A97"), RAW_BALANCE),
        (address!("0x9D0464996170c6B9e75eED71c68B99dDEDf279e8"), RAW_BALANCE),
        (address!("0xBaaa1F5DbA42C3389bDbc2c9D2dE134F5cD0Dc89"), RAW_BALANCE),
        (address!("0xDa5B670CcD418a187a3066674A8002Adc9356Ad1"), RAW_BALANCE),
        (address!("0xf03bD3cfE85f00bF5819AC20f0870cE8a8d1F0D8"), RAW_BALANCE),
        (address!("0xFB9a265b5a1f52d97838Ec7274A0b1442efAcC87"), RAW_BALANCE),
        (address!("0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492"), LIVE_ADMIN_ORACLE),
        (address!("0xBfAb6FA95E0091ed66058ad493189D2cB29385E6"), LIVE_ADMIN_ORACLE),
        (address!("0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C"), RATE_ADJUSTED_NO_ONE_CTOKEN),
        (address!("0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56"), RATE_ADJUSTED_NO_ONE_CTOKEN),
        (address!("0xA5407eAE9Ba41422680e2e00537571bcC53efBfD"), RATE_ADJUSTED_NO_ONE),
        (address!("0x2dded6Da1BF5DBdF597C45fcFaa3194e53EcfeAF"), CYTOKEN),
        (address!("0x06364f10B501e868329afBc005b3492902d6C763"), RATE_ADJUSTED_YTOKEN),
        (address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51"), RATE_ADJUSTED_NO_ONE_YTOKEN),
        (address!("0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27"), RATE_ADJUSTED_NO_ONE_YTOKEN),
        (address!("0xA96A65c051bF88B4095Ee1f2451C2A9d43F53Ae2"), NO_ONE_FEE_RATE_AETH),
        (address!("0xF9440930043eb3997fc70e1339dBb11F341de7A8"), NO_ONE_FEE_RATE_RETH),
        (address!("0xEB16Ae0052ed37f479f7fe63849198Df1765a733"), LIVE_ADMIN_DYNAMIC),
        (address!("0xDeBF20617708857ebe4F679508E7b7863a8A8EeE"), LIVE_ADMIN_DYNAMIC_PRECISION),
    ];

    #[test]
    fn every_mapped_address_keeps_its_registered_discriminants() {
        for (addr, want) in REGISTERED_DISCRIMINANTS {
            let s = resolve_curve_strategy_discriminants(*addr);
            let got = (
                s.swap_style,
                s.lending_rate_style,
                s.metapool_rate_style,
                s.metapool_underlying_style,
            );
            assert_eq!(got, *want, "{addr}");
        }
    }
}
