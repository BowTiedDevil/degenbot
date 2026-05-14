"""Curve pool strategy resolution.

Maps pool addresses to their PoolStrategies, combining all calculation
variant enums into a single lookup. This replaces the address-based
conditional dispatch in CurveStableswapPool.get_dy() and related methods.

The mapping is resolved once at construction time by CurvePoolBuilder.
The pool class is address-agnostic — it only reads strategy enum values.
"""

from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve._variant_groups import resolve_d_variant, resolve_y_variant, resolve_yd_variant
from degenbot.curve.types import (
    LendingRateStyle,
    MetapoolRateStyle,
    MetapoolUnderlyingStyle,
    PoolStrategies,
    SwapStyle,
)


def _a(addr: str) -> ChecksumAddress:
    """Shorthand for checksummed addresses in the mapping below."""
    return get_checksum_address(addr)


# ── Full address → PoolStrategies mapping ──
# Every address that previously appeared in an `if self.address` block
# in get_dy(), _get_dy_underlying(), or the variant group frozensets
# must appear here with the correct strategies.

_POOL_STRATEGIES: dict[ChecksumAddress, PoolStrategies] = {
    # ── Metapool rate style variants ──
    # 0xC61557C5d177bd7DC889A3b621eEC333e168f68A
    #   get_dy metapool: (PRECISION, virtual_price)
    #   _get_dy_underlying: (PRECISION, virtual_price) with special base_i/base_j logic
    _a("0xC61557C5d177bd7DC889A3b621eEC333e168f68A"): PoolStrategies(
        metapool_rate_style=MetapoolRateStyle.PRECISION_VP,
        metapool_underlying_style=MetapoolUnderlyingStyle.PRECISION_VP,
    ),
    # 0x618788357D0EBd8A37e763ADab3bc575D54c2C7d
    #   get_dy metapool: (redemption_price, virtual_price)
    #   _get_dy_underlying: redemption_price for first coin, VP for second
    _a("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d"): PoolStrategies(
        metapool_rate_style=MetapoolRateStyle.REDEMPTION_VP,
        metapool_underlying_style=MetapoolUnderlyingStyle.REDEMPTION,
    ),
    # 0x4606326b4Db89373F5377C316d3b0F6e55Bc6A20
    #   _get_dy_underlying: same as 0xC61557 — (PRECISION, VP) path
    _a("0x4606326b4Db89373F5377C316d3b0F6e55Bc6A20"): PoolStrategies(
        metapool_rate_style=MetapoolRateStyle.PRECISION_VP,
        metapool_underlying_style=MetapoolUnderlyingStyle.PRECISION_VP,
    ),

    # ── Live balances minus admin (SwapStyle.LIVE_ADMIN) ──
    _a("0x4e0915C88bC70750D68C481540F081fEFaF22273"): PoolStrategies(
        swap_style=SwapStyle.LIVE_ADMIN,
    ),
    _a("0x1005F7406f32a61BD760CfA14aCCd2737913d546"): PoolStrategies(
        swap_style=SwapStyle.LIVE_ADMIN,
    ),
    _a("0x6A274dE3e2462c7614702474D64d376729831dCa"): PoolStrategies(
        swap_style=SwapStyle.LIVE_ADMIN,
    ),
    _a("0xb9446c4Ef5EBE66268dA6700D26f96273DE3d571"): PoolStrategies(
        swap_style=SwapStyle.LIVE_ADMIN,
    ),
    _a("0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a"): PoolStrategies(
        swap_style=SwapStyle.LIVE_ADMIN,
    ),

    # ── Crypto pool ──
    _a("0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5"): PoolStrategies(
        swap_style=SwapStyle.CRYPTO,
    ),

    # ── Rate-adjusted fee style (SwapStyle.RATE_ADJUSTED) ──
    # dy computed as (xp[j] - y - 1) * PRECISION // rates[j], then fee applied
    _a("0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED,
    ),
    _a("0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED,
    ),
    _a("0x93054188d876f558f4a66B2EF1d97d16eDf0895B"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED,
    ),
    _a("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED,
    ),

    # ── Standard rate style (SwapStyle.STANDARD_RATE) ──
    # dy computed as xp[j] - y - 1, fee applied, then rate conversion
    # 31 addresses
    _a("0x0Ce6a5fF5217e38315f87032CF90686C96627CAA"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x19b080FE1ffA0553469D20Ca36219F17Fcf03859"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x1C5F80b6B68A9E1Ef25926EeE00b5255791b996B"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x1F6bb2a7a2A84d08bb821B89E38cA651175aeDd4"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x21B45B2c1C53fDFe378Ed1955E8Cc29aE8cE0132"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x3CFAa1596777CAD9f5004F9a0c443d912E262243"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x3F1B0278A9ee595635B61817630cC19DE792f506"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x4424b4A37ba0088D8a718b8fc2aB7952C7e695F5"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x602a9Abb10582768Fd8a9f13aD6316Ac2A5A2e2B"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x8461A004b50d321CB22B7d034969cE6803911899"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x857110B5f8eFD66CC3762abb935315630AC770B5"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x8818a9bb44Fbf33502bE7c15c500d0C783B73067"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0x9c2C8910F113181783c249d8F6Aa41b51Cde0f0c"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xa1F8A6807c402E4A15ef4EBa36528A3FED24E577"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xaE34574AC03A15cd58A92DC79De7B1A0800F1CE3"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xAf25fFe6bA5A8a29665adCfA6D30C5Ae56CA0Cd3"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xBa3436Fd341F2C8A928452Db3C5A3670d1d5Cc73"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xbB2dC673E1091abCA3eaDB622b18f6D4634b2CD9"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xc5424B857f758E906013F3555Dad202e4bdB4567"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xc8a7C1c4B748970F57cA59326BcD49F5c9dc43E3"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xcbD5cC53C5b846671C6434Ab301AD4d210c21184"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xD6Ac1CB9019137a896343Da59dDE6d097F710538"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xD7C10449A6D134A9ed37e2922F8474EAc6E5c100"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xDC24316b9AE028F1497c275EB9192a3Ea0f67022"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xe7A3b38c39F97E977723bd1239C3470702568e7B"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xf083FBa98dED0f9C970e5a418500bad08D8b9732"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xF178C0b5Bb7e7aBF4e12A4838C7b7c5bA2C623c0"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xf253f83AcA21aAbD2A20553AE0BF7F65C755A07F"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xfC8c34a3B3CFE1F1Dd6DBCCEC4BC5d3103b80FF0"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),
    _a("0xFD5dB7463a3aB53fD211b4af195c5BCCC1A03890"): PoolStrategies(
        swap_style=SwapStyle.STANDARD,
    ),

    # ── Raw balance style (SwapStyle.RAW_BALANCE) ──
    # No rate conversion, direct fee application
    _a("0x04c90C198b2eFF55716079bc06d7CCc4aa4d7512"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),
    _a("0x320B564Fb9CF36933eC507a846ce230008631fd3"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),
    _a("0x48fF31bBbD8Ab553Ebe7cBD84e1eA3dBa8f54957"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),
    _a("0x55A8a39bc9694714E2874c1ce77aa1E599461E18"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),
    _a("0x875DF0bA24ccD867f8217593ee27253280772A97"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),
    _a("0x9D0464996170c6B9e75eED71c68B99dDEDf279e8"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),
    _a("0xBaaa1F5DbA42C3389bDbc2c9D2dE134F5cD0Dc89"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),
    _a("0xDa5B670CcD418a187a3066674A8002Adc9356Ad1"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),
    _a("0xf03bD3cfE85f00bF5819AC20f0870cE8a8d1F0D8"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),
    _a("0xFB9a265b5a1f52d97838Ec7274A0b1442efAcC87"): PoolStrategies(
        swap_style=SwapStyle.RAW_BALANCE,
    ),

    # ── Live admin + oracle (SwapStyle.LIVE_ADMIN_ORACLE) ──
    _a("0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492"): PoolStrategies(
        swap_style=SwapStyle.LIVE_ADMIN_ORACLE,
        lending_rate_style=LendingRateStyle.ORACLE,
    ),
    _a("0xBfAb6FA95E0091ed66058ad493189D2cB29385E6"): PoolStrategies(
        swap_style=SwapStyle.LIVE_ADMIN_ORACLE,
        lending_rate_style=LendingRateStyle.ORACLE,
    ),

    # ── CTOKEN swap style (RATE_ADJUSTED_NO_ONE) ──
    _a("0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED_NO_ONE,
        lending_rate_style=LendingRateStyle.CTOKEN,
    ),
    _a("0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED_NO_ONE,
        lending_rate_style=LendingRateStyle.CTOKEN,
    ),
    _a("0xA5407eAE9Ba41422680e2e00537571bcC53efBfD"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED_NO_ONE,
    ),

    # ── CYTOKEN swap style ──
    _a("0x2dded6Da1BF5DBdF597C45fcFaa3194e53EcfeAF"): PoolStrategies(
        swap_style=SwapStyle.CYTOKEN,
        lending_rate_style=LendingRateStyle.CYTOKEN,
    ),

    # ── YTOKEN swap style ──
    # 0x06364f10: dy before rate with -1, fee on converted (RATE_ADJUSTED)
    _a("0x06364f10B501e868329afBc005b3492902d6C763"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED,
        lending_rate_style=LendingRateStyle.YTOKEN,
    ),
    # 0x45F783CC, 0x79a8C46D: dy before rate without -1, fee on converted (RATE_ADJUSTED_NO_ONE)
    _a("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED_NO_ONE,
        lending_rate_style=LendingRateStyle.YTOKEN,
    ),
    _a("0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27"): PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED_NO_ONE,
        lending_rate_style=LendingRateStyle.YTOKEN,
    ),

    # ── AETH swap style (NO_ONE_FEE_RATE) ──
    _a("0xA96A65c051bF88B4095Ee1f2451C2A9d43F53Ae2"): PoolStrategies(
        swap_style=SwapStyle.NO_ONE_FEE_RATE,
        lending_rate_style=LendingRateStyle.AETH,
    ),

    # ── RETH swap style (NO_ONE_FEE_RATE) ──
    _a("0xF9440930043eb3997fc70e1339dBb11F341de7A8"): PoolStrategies(
        swap_style=SwapStyle.NO_ONE_FEE_RATE,
        lending_rate_style=LendingRateStyle.RETH,
    ),

    # ── Live admin + dynamic fee ──
    _a("0xEB16Ae0052ed37f479f7fe63849198Df1765a733"): PoolStrategies(
        swap_style=SwapStyle.LIVE_ADMIN_DYNAMIC,
    ),
    _a("0xDeBF20617708857ebe4F679508E7b7863a8A8EeE"): PoolStrategies(
        swap_style=SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION,
    ),
}


def resolve_pool_strategies(pool_address: ChecksumAddress | str) -> PoolStrategies:
    """Resolve the complete PoolStrategies for a Curve pool address.

    Combines the address lookup from _POOL_STRATEGIES with the variant
    group resolution from _variant_groups. Returns a PoolStrategies with
    STANDARD/NONE defaults for any strategies not found in the mapping.
    """
    pool_address = get_checksum_address(pool_address)

    # Start with defaults
    d_variant = resolve_d_variant(pool_address)
    y_variant = resolve_y_variant(pool_address)
    yd_variant = resolve_yd_variant(pool_address)

    # Look up address-specific strategies
    mapped = _POOL_STRATEGIES.get(pool_address)

    if mapped is not None:
        # Merge: variant groups + mapped strategies
        return PoolStrategies(
            d_variant=d_variant,
            y_variant=y_variant,
            yd_variant=yd_variant,
            swap_style=mapped.swap_style,
            metapool_rate_style=mapped.metapool_rate_style,
            metapool_underlying_style=mapped.metapool_underlying_style,
            lending_rate_style=mapped.lending_rate_style,
        )

    # No mapping found — use defaults
    return PoolStrategies(
        d_variant=d_variant,
        y_variant=y_variant,
        yd_variant=yd_variant,
    )
