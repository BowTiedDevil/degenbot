//! Curve pool strategy + variant resolution (task `JVZO6T`, epic `TV72EG`).
//!
//! Ports the Python `curve/_pool_strategies.py` + `curve/_variant_groups.py`
//! address→strategy dispatch into pure-Rust static maps producing the raw
//! `u8` discriminants that [`crate::curve_state::RegisterCurvePoolParams`]
//! stores. Frozen at construction time; the Rust calc path (the swap engine)
//! later dispatches on these values. No I/O, no `pyo3` — a standalone `cargo
//! add degenbot` consumer resolves strategies directly.
//!
//! ## Discriminant values (single source of truth, spike `CE7QVQ` §2)
//!
//! The Python enums are `auto()`-based (1, 2, 3, … in declaration order) and
//! the builder maps `.value` into the `u8` the Rust identity stores. A
//! strategy fields not present in an entry resolves to its default:
//! `STANDARD`/`NONE` = `1`.
//!
//! ```text
//! DVariant:            STANDARD=1, VARIANT_ALPHA=2, VARIANT_ALPHA_DP_ALPHA=3,
//!                      VARIANT_DP_ALPHA=4, VARIANT_BETA_DP=5, VARIANT_GAMMA_DP=6
//! YVariant:            STANDARD=1, VARIANT_0=2, VARIANT_1=3
//! YDVariant:           STANDARD=1, VARIANT_0=2
//! SwapStyle:           STANDARD=1, RATE_ADJUSTED=2, RAW_BALANCE=3, CRYPTO=4,
//!                      LIVE_ADMIN=5, LIVE_ADMIN_DYNAMIC=6, LIVE_ADMIN_DYNAMIC_PRECISION=7,
//!                      LIVE_ADMIN_ORACLE=8, NO_ONE_FEE_RATE=9, CYTOKEN=10,
//!                      RATE_ADJUSTED_NO_ONE=11
//! LendingRateStyle:    NONE=1, CTOKEN=2, YTOKEN=3, CYTOKEN=4, AETH=5, RETH=6, ORACLE=7
//! MetapoolRateStyle:   STANDARD=1, PRECISION_VP=2, REDEMPTION_VP=3
//! MetapoolUnderlyingStyle: STANDARD=1, REDEMPTION=2, PRECISION_VP=3
//! ```
//!
//! ## Provenance
//!
//! Preserves the Python `PROVENANCE WARNING`: this transliterates the address-
//! based dispatch without re-verifying each address against on-chain source,
//! and an unlisted address falls through to the `STANDARD`/`NONE` defaults.

use alloy::primitives::{address, Address};

/// The four non-variant strategy discriminants for one pool entry (swap style,
/// lending rate style, metapool rate style, metapool underlying style). A
/// field not present in the mapped entry stays at its `1` default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveStrategyDiscriminants {
    /// `DVariant` discriminant (`1` = STANDARD).
    pub d_variant: u8,
    /// `YVariant` discriminant (`1` = STANDARD).
    pub y_variant: u8,
    /// `YDVariant` discriminant (`1` = STANDARD).
    pub yd_variant: u8,
    /// `SwapStyle` discriminant (`1` = STANDARD).
    pub swap_style: u8,
    /// `LendingRateStyle` discriminant (`1` = NONE).
    pub lending_rate_style: u8,
    /// `MetapoolRateStyle` discriminant (`1` = STANDARD).
    pub metapool_rate_style: u8,
    /// `MetapoolUnderlyingStyle` discriminant (`1` = STANDARD).
    pub metapool_underlying_style: u8,
}

impl Default for CurveStrategyDiscriminants {
    fn default() -> Self {
        Self {
            d_variant: 1,
            y_variant: 1,
            yd_variant: 1,
            swap_style: 1,
            lending_rate_style: 1,
            metapool_rate_style: 1,
            metapool_underlying_style: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Variant groups (transliterated from `_variant_groups.py`).
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

/// Resolve the `DVariant` discriminant (2–6, or 1 STANDARD) for a pool address.
#[must_use]
pub fn resolve_d_variant(addr: Address) -> u8 {
    if D_GROUP_0.contains(&addr) {
        2
    } else if D_GROUP_1.contains(&addr) {
        3
    } else if D_GROUP_2.contains(&addr) {
        5
    } else if D_GROUP_3.contains(&addr) {
        4
    } else if D_GROUP_4.contains(&addr) {
        6
    } else {
        1
    }
}

/// Resolve the `YVariant` discriminant (2–3, or 1 STANDARD) for a pool address.
/// `Y_GROUP_0 ⊂ Y_GROUP_1`, so `group0` → `VARIANT_0` (2) wins over `group1` → 3.
#[must_use]
pub fn resolve_y_variant(addr: Address) -> u8 {
    if Y_GROUP_0.contains(&addr) {
        2
    } else if Y_GROUP_1.contains(&addr) {
        3
    } else {
        1
    }
}

/// Resolve the `YDVariant` discriminant (2, or 1 STANDARD) for a pool address.
#[must_use]
pub fn resolve_yd_variant(addr: Address) -> u8 {
    if YD_GROUP_0.contains(&addr) {
        2
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// Address → swap/lending/metapool strategy map (transliterated from
// `_pool_strategies.py` `_POOL_STRATEGIES`). Each entry lists only the
// non-default fields as `(swap_style, lending_rate_style)` or
// `(metapool_rate_style, metapool_underlying_style)`; unlisted fields stay 1.
// ---------------------------------------------------------------------------

/// Look up the address-specific (non-variant) strategy discriminants.
/// Returns `None` for an unmapped address (→ all defaults).
#[must_use]
fn mapped_strategies(addr: Address) -> Option<(u8, u8, u8, u8)> {
    // (swap_style, lending_rate_style, metapool_rate_style,
    //  metapool_underlying_style); 1 = default.
    Some(match addr {
        // Metapool rate style variants.
        x if x == address!("0xC61557C5d177bd7DC889A3b621eEC333e168f68A") => (1, 1, 2, 3),
        x if x == address!("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d") => (1, 1, 3, 2),
        x if x == address!("0x4606326b4Db89373F5377C316d3b0F6e55Bc6A20") => (1, 1, 2, 3),
        // Live balances minus admin.
        x if x == address!("0x4e0915C88bC70750D68C481540F081fEFaF22273") => (5, 1, 1, 1),
        x if x == address!("0x1005F7406f32a61BD760CfA14aCCd2737913d546") => (5, 1, 1, 1),
        x if x == address!("0x6A274dE3e2462c7614702474D64d376729831dCa") => (5, 1, 1, 1),
        x if x == address!("0xb9446c4Ef5EBE66268dA6700D26f96273DE3d571") => (5, 1, 1, 1),
        x if x == address!("0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a") => (5, 1, 1, 1),
        // Crypto.
        x if x == address!("0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5") => (4, 1, 1, 1),
        // Rate-adjusted fee style.
        x if x == address!("0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F") => (2, 1, 1, 1),
        x if x == address!("0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714") => (2, 1, 1, 1),
        x if x == address!("0x93054188d876f558f4a66B2EF1d97d16eDf0895B") => (2, 1, 1, 1),
        x if x == address!("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7") => (2, 1, 1, 1),
        // Standard rate style.
        x if x == address!("0x0Ce6a5fF5217e38315f87032CF90686C96627CAA") => (1, 1, 1, 1),
        x if x == address!("0x19b080FE1ffA0553469D20Ca36219F17Fcf03859") => (1, 1, 1, 1),
        x if x == address!("0x1C5F80b6B68A9E1Ef25926EeE00b5255791b996B") => (1, 1, 1, 1),
        x if x == address!("0x1F6bb2a7a2A84d08bb821B89E38cA651175aeDd4") => (1, 1, 1, 1),
        x if x == address!("0x21B45B2c1C53fDFe378Ed1955E8Cc29aE8cE0132") => (1, 1, 1, 1),
        x if x == address!("0x3CFAa1596777CAD9f5004F9a0c443d912E262243") => (1, 1, 1, 1),
        x if x == address!("0x3F1B0278A9ee595635B61817630cC19DE792f506") => (1, 1, 1, 1),
        x if x == address!("0x4424b4A37ba0088D8a718b8fc2aB7952C7e695F5") => (1, 1, 1, 1),
        x if x == address!("0x602a9Abb10582768Fd8a9f13aD6316Ac2A5A2e2B") => (1, 1, 1, 1),
        x if x == address!("0x8461A004b50d321CB22B7d034969cE6803911899") => (1, 1, 1, 1),
        x if x == address!("0x857110B5f8eFD66CC3762abb935315630AC770B5") => (1, 1, 1, 1),
        x if x == address!("0x8818a9bb44Fbf33502bE7c15c500d0C783B73067") => (1, 1, 1, 1),
        x if x == address!("0x9c2C8910F113181783c249d8F6Aa41b51Cde0f0c") => (1, 1, 1, 1),
        x if x == address!("0xa1F8A6807c402E4A15ef4EBa36528A3FED24E577") => (1, 1, 1, 1),
        x if x == address!("0xaE34574AC03A15cd58A92DC79De7B1A0800F1CE3") => (1, 1, 1, 1),
        x if x == address!("0xAf25fFe6bA5A8a29665adCfA6D30C5Ae56CA0Cd3") => (1, 1, 1, 1),
        x if x == address!("0xBa3436Fd341F2C8A928452Db3C5A3670d1d5Cc73") => (1, 1, 1, 1),
        x if x == address!("0xbB2dC673E1091abCA3eaDB622b18f6D4634b2CD9") => (1, 1, 1, 1),
        x if x == address!("0xc5424B857f758E906013F3555Dad202e4bdB4567") => (1, 1, 1, 1),
        x if x == address!("0xc8a7C1c4B748970F57cA59326BcD49F5c9dc43E3") => (1, 1, 1, 1),
        x if x == address!("0xcbD5cC53C5b846671C6434Ab301AD4d210c21184") => (1, 1, 1, 1),
        x if x == address!("0xD6Ac1CB9019137a896343Da59dDE6d097F710538") => (1, 1, 1, 1),
        x if x == address!("0xD7C10449A6D134A9ed37e2922F8474EAc6E5c100") => (1, 1, 1, 1),
        x if x == address!("0xDC24316b9AE028F1497c275EB9192a3Ea0f67022") => (1, 1, 1, 1),
        x if x == address!("0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2") => (1, 1, 1, 1),
        x if x == address!("0xe7A3b38c39F97E977723bd1239C3470702568e7B") => (1, 1, 1, 1),
        x if x == address!("0xf083FBa98dED0f9C970e5a418500bad08D8b9732") => (1, 1, 1, 1),
        x if x == address!("0xF178C0b5Bb7e7aBF4e12A4838C7b7c5bA2C623c0") => (1, 1, 1, 1),
        x if x == address!("0xf253f83AcA21aAbD2A20553AE0BF7F65C755A07F") => (1, 1, 1, 1),
        x if x == address!("0xfC8c34a3B3CFE1F1Dd6DBCCEC4BC5d3103b80FF0") => (1, 1, 1, 1),
        x if x == address!("0xFD5dB7463a3aB53fD211b4af195c5BCCC1A03890") => (1, 1, 1, 1),
        // Raw balance style.
        x if x == address!("0x04c90C198b2eFF55716079bc06d7CCc4aa4d7512") => (3, 1, 1, 1),
        x if x == address!("0x320B564Fb9CF36933eC507a846ce230008631fd3") => (3, 1, 1, 1),
        x if x == address!("0x48fF31bBbD8Ab553Ebe7cBD84e1eA3dBa8f54957") => (3, 1, 1, 1),
        x if x == address!("0x55A8a39bc9694714E2874c1ce77aa1E599461E18") => (3, 1, 1, 1),
        x if x == address!("0x875DF0bA24ccD867f8217593ee27253280772A97") => (3, 1, 1, 1),
        x if x == address!("0x9D0464996170c6B9e75eED71c68B99dDEDf279e8") => (3, 1, 1, 1),
        x if x == address!("0xBaaa1F5DbA42C3389bDbc2c9D2dE134F5cD0Dc89") => (3, 1, 1, 1),
        x if x == address!("0xDa5B670CcD418a187a3066674A8002Adc9356Ad1") => (3, 1, 1, 1),
        x if x == address!("0xf03bD3cfE85f00bF5819AC20f0870cE8a8d1F0D8") => (3, 1, 1, 1),
        x if x == address!("0xFB9a265b5a1f52d97838Ec7274A0b1442efAcC87") => (3, 1, 1, 1),
        // Live admin + oracle.
        x if x == address!("0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492") => (8, 7, 1, 1),
        x if x == address!("0xBfAb6FA95E0091ed66058ad493189D2cB29385E6") => (8, 7, 1, 1),
        // CTOKEN rate-adjusted-no-one.
        x if x == address!("0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C") => (11, 2, 1, 1),
        x if x == address!("0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56") => (11, 2, 1, 1),
        x if x == address!("0xA5407eAE9Ba41422680e2e00537571bcC53efBfD") => (11, 1, 1, 1),
        // CYTOKEN.
        x if x == address!("0x2dded6Da1BF5DBdF597C45fcFaa3194e53EcfeAF") => (10, 4, 1, 1),
        // YTOKEN (rate-adjusted).
        x if x == address!("0x06364f10B501e868329afBc005b3492902d6C763") => (2, 3, 1, 1),
        // YTOKEN (rate-adjusted-no-one).
        x if x == address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51") => (11, 3, 1, 1),
        x if x == address!("0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27") => (11, 3, 1, 1),
        // AETH.
        x if x == address!("0xA96A65c051bF88B4095Ee1f2451C2A9d43F53Ae2") => (9, 5, 1, 1),
        // RETH.
        x if x == address!("0xF9440930043eb3997fc70e1339dBb11F341de7A8") => (9, 6, 1, 1),
        // Live admin + dynamic fee.
        x if x == address!("0xEB16Ae0052ed37f479f7fe63849198Df1765a733") => (6, 1, 1, 1),
        x if x == address!("0xDeBF20617708857ebe4F679508E7b7863a8A8EeE") => (7, 1, 1, 1),
        _ => return None,
    })
}

/// Resolve the complete strategy-discriminant set for a pool address.
///
/// Combines the variant-group resolution (d/y/yd) with the address-specific
/// swap/lending/metapool map. Unlisted pools fall through to `STANDARD`/`NONE`
/// defaults (mirrors `_pool_strategies.resolve_pool_strategies`).
#[must_use]
pub fn resolve_curve_strategy_discriminants(addr: Address) -> CurveStrategyDiscriminants {
    let (swap_style, lending_rate_style, metapool_rate_style, metapool_underlying_style) =
        mapped_strategies(addr).unwrap_or((1, 1, 1, 1));
    CurveStrategyDiscriminants {
        d_variant: resolve_d_variant(addr),
        y_variant: resolve_y_variant(addr),
        yd_variant: resolve_yd_variant(addr),
        swap_style,
        lending_rate_style,
        metapool_rate_style,
        metapool_underlying_style,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d_variant_groups_resolve_to_correct_discriminants() {
        // One address from each group + the STANDARD default.
        assert_eq!(
            resolve_d_variant(address!("0x06364f10B501e868329afBc005b3492902d6C763")),
            2
        );
        assert_eq!(
            resolve_d_variant(address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51")),
            3
        );
        assert_eq!(
            resolve_d_variant(address!("0x0AD66FeC8dB84F8A3365ADA04aB23ce607ac6E24")),
            5
        );
        assert_eq!(
            resolve_d_variant(address!("0xDC24316b9AE028F1497c275EB9192a3Ea0f67022")),
            4
        );
        assert_eq!(
            resolve_d_variant(address!("0x1062FD8eD633c1f080754c19317cb3912810B5e5")),
            6
        );
        assert_eq!(
            resolve_d_variant(address!("0x2222222222222222222222222222222222222222")),
            1
        );
    }

    #[test]
    fn y_variant_group0_wins_over_group1() {
        // In both groups (group0 ⊂ group1) → VARIANT_0 (2).
        assert_eq!(
            resolve_y_variant(address!("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51")),
            2
        );
        // group1 only → VARIANT_1 (3).
        assert_eq!(
            resolve_y_variant(address!("0x06364f10B501e868329afBc005b3492902d6C763")),
            3
        );
        // neither → STANDARD (1).
        assert_eq!(
            resolve_y_variant(address!("0x3F1B0278A9ee595635B61817630cC19DE792f506")),
            1
        );
    }

    #[test]
    fn yd_variant_resolves() {
        assert_eq!(
            resolve_yd_variant(address!("0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2")),
            2
        );
        assert_eq!(
            resolve_yd_variant(address!("0x3F1B0278A9ee595635B61817630cC19DE792f506")),
            1
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
        // 0xbEbc4478 (tripool): D-group0 (2) + Y-group1 (3) + RATE_ADJUSTED (2).
        let s = resolve_curve_strategy_discriminants(address!(
            "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"
        ));
        assert_eq!(s.d_variant, 2);
        assert_eq!(s.y_variant, 3);
        assert_eq!(s.yd_variant, 1);
        assert_eq!(s.swap_style, 2);
        assert_eq!(s.lending_rate_style, 1);
    }

    #[test]
    fn metapool_style_overrides_defaults() {
        let s = resolve_curve_strategy_discriminants(address!(
            "0x618788357D0EBd8A37e763ADab3bc575D54c2C7d"
        ));
        assert_eq!(s.metapool_rate_style, 3); // REDEMPTION_VP
        assert_eq!(s.metapool_underlying_style, 2); // REDEMPTION
        assert_eq!(s.swap_style, 1); // default stays STANDARD
    }

    #[test]
    fn crypto_and_leverage_pools() {
        let crypto = resolve_curve_strategy_discriminants(address!(
            "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5"
        ));
        assert_eq!(crypto.swap_style, 4); // CRYPTO

        // CTOKEN rate-adjusted-no-one.
        let ct = resolve_curve_strategy_discriminants(address!(
            "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C"
        ));
        assert_eq!(ct.swap_style, 11);
        assert_eq!(ct.lending_rate_style, 2); // CTOKEN
    }
}
