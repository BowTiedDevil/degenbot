"""Curve pool variant group membership.

Maps pool addresses to their D-variant, Y-variant, and Y_D-variant group,
determining which calculation formula to use in _get_d(), _get_y(), and _get_y_d().

The builder resolves variant group membership at construction time and passes
the result as strategy enums to the pool constructor.
"""

from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.types import DVariant, YDVariant, YVariant

_D_VARIANT_GROUP_0: frozenset[ChecksumAddress] = frozenset(
    get_checksum_address(addr)
    for addr in (
        "0x06364f10B501e868329afBc005b3492902d6C763",
        "0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F",
        "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C",
        "0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714",
        "0x93054188d876f558f4a66B2EF1d97d16eDf0895B",
        "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
    )
)

_D_VARIANT_GROUP_1: frozenset[ChecksumAddress] = frozenset(
    get_checksum_address(addr)
    for addr in (
        "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
        "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
        "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56",
        "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD",
    )
)

_D_VARIANT_GROUP_2: frozenset[ChecksumAddress] = frozenset(
    get_checksum_address(addr)
    for addr in (
        "0x0AD66FeC8dB84F8A3365ADA04aB23ce607ac6E24",
        "0x1c899dED01954d0959E034b62a728e7fEbE593b0",
        "0x3F1B0278A9ee595635B61817630cC19DE792f506",
        "0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a",
        "0x447Ddd4960d9fdBF6af9a790560d0AF76795CB08",
        "0x453D92C7d4263201C69aACfaf589Ed14202d83a4",
        "0x663aC72a1c3E1C4186CD3dCb184f216291F4878C",
        "0x6A274dE3e2462c7614702474D64d376729831dCa",
        "0x7C0d189E1FecB124487226dCbA3748bD758F98E4",
        "0x875DF0bA24ccD867f8217593ee27253280772A97",
        "0x99f5aCc8EC2Da2BC0771c32814EFF52b712de1E5",
        "0x9D0464996170c6B9e75eED71c68B99dDEDf279e8",
        "0xB37D6c07482Bc11cd28a1f11f1a6ad7b66Dec933",
        "0xB657B895B265C38c53FFF00166cF7F6A3C70587d",
        "0xD6Ac1CB9019137a896343Da59dDE6d097F710538",
        "0xE95E4c2dAC312F31Dc605533D5A4d0aF42579308",
        "0xf7b55C3732aD8b2c2dA7c24f30A69f55c54FB717",
    )
)

_D_VARIANT_GROUP_3: frozenset[ChecksumAddress] = frozenset(
    get_checksum_address(addr)
    for addr in (
        "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022",
        "0xDeBF20617708857ebe4F679508E7b7863a8A8EeE",
        "0xEB16Ae0052ed37f479f7fe63849198Df1765a733",
    )
)

_D_VARIANT_GROUP_4: frozenset[ChecksumAddress] = frozenset(
    get_checksum_address(addr)
    for addr in (
        "0x1062FD8eD633c1f080754c19317cb3912810B5e5",
        "0x1C5F80b6B68A9E1Ef25926EeE00b5255791b996B",
        "0x26f3f26F46cBeE59d1F8860865e13Aa39e36A8c0",
        "0x2d600BbBcC3F1B6Cb9910A70BaB59eC9d5F81B9A",
        "0x320B564Fb9CF36933eC507a846ce230008631fd3",
        "0x3b21C2868B6028CfB38Ff86127eF22E68d16d53B",
        "0x69ACcb968B19a53790f43e57558F5E443A91aF22",
        "0x971add32Ea87f10bD192671630be3BE8A11b8623",
        "0xCA0253A98D16e9C1e3614caFDA19318EE69772D0",
        "0xfBB481A443382416357fA81F16dB5A725DC6ceC8",
    )
)


_Y_VARIANT_GROUP_0: frozenset[ChecksumAddress] = frozenset(
    get_checksum_address(addr)
    for addr in (
        "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
        "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C",
        "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
        "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56",
        "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD",
    )
)

_Y_VARIANT_GROUP_1: frozenset[ChecksumAddress] = frozenset(
    get_checksum_address(addr)
    for addr in (
        "0x06364f10B501e868329afBc005b3492902d6C763",
        "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
        "0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F",
        "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C",
        "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
        "0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714",
        "0x93054188d876f558f4a66B2EF1d97d16eDf0895B",
        "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56",
        "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD",
        "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
    )
)


_Y_D_VARIANT_GROUP_0: frozenset[ChecksumAddress] = frozenset(
    get_checksum_address(addr)
    for addr in (
        "0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2",
        "0xf253f83AcA21aAbD2A20553AE0BF7F65C755A07F",
    )
)


def resolve_d_variant(pool_address: ChecksumAddress | str) -> DVariant:
    """Resolve the D-calculation variant for a Curve pool address.

    Returns DVariant.STANDARD for unrecognized addresses.
    """
    pool_address = get_checksum_address(pool_address)
    if pool_address in _D_VARIANT_GROUP_0:
        return DVariant.VARIANT_ALPHA
    if pool_address in _D_VARIANT_GROUP_1:
        return DVariant.VARIANT_ALPHA_DP_ALPHA
    if pool_address in _D_VARIANT_GROUP_2:
        return DVariant.VARIANT_BETA_DP
    if pool_address in _D_VARIANT_GROUP_3:
        return DVariant.VARIANT_DP_ALPHA
    if pool_address in _D_VARIANT_GROUP_4:
        return DVariant.VARIANT_GAMMA_DP
    return DVariant.STANDARD


def resolve_y_variant(pool_address: ChecksumAddress | str) -> YVariant:
    """Resolve the Y-calculation variant for a Curve pool address.

    Y_VARIANT_GROUP_0 ⊂ Y_VARIANT_GROUP_1, so addresses resolve to:
    - VARIANT_0: in both groups (amp without A_PRECISION divisor + c/b without A_PRECISION)
    - VARIANT_1: in GROUP_1 only (amp with A_PRECISION + c/b without A_PRECISION)
    - STANDARD: in neither group (amp with A_PRECISION + c/b with A_PRECISION)

    Returns YVariant.STANDARD for unrecognized addresses.
    """
    pool_address = get_checksum_address(pool_address)
    if pool_address in _Y_VARIANT_GROUP_0:
        return YVariant.VARIANT_0
    if pool_address in _Y_VARIANT_GROUP_1:
        return YVariant.VARIANT_1
    return YVariant.STANDARD


def resolve_yd_variant(pool_address: ChecksumAddress | str) -> YDVariant:
    """Resolve the Y_D-calculation variant for a Curve pool address.

    Returns YDVariant.STANDARD for unrecognized addresses.
    """
    pool_address = get_checksum_address(pool_address)
    if pool_address in _Y_D_VARIANT_GROUP_0:
        return YDVariant.VARIANT_0
    return YDVariant.STANDARD
