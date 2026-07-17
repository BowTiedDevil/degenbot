"""Aerodrome EIP-1167 clone-address derivation — Rust↔Python parity gate.

Ergo S5SJXF/U43OVR: the Aerodrome V2/V3 deterministic pool-address derivation
(a CREATE2 deployment of an EIP-1167 minimal proxy of a master
implementation contract, the salt keyed on ``stable`` (V2) /
``tick_spacing`` (V3)) is ported to the pure-Rust
``degenbot_uniswap::create2`` leaf and exposed as the
``degenbot._ffi.compute_aerodrome_v2_pool_address`` /
``compute_aerodrome_v3_pool_address`` pyfunctions.

Per the §4.2 red-green parity protocol, this module asserts byte-for-byte
agreement between the Rust pyfunctions and the Python parity oracle
(``aerodrome.functions.generate_aerodrome_*``) over a Base-deployment
fixture corpus. The Rust ``#[cfg(test)]`` corpus pins the same fixtures
in-repo; this is the Python-side gate proving the FFI seam lands the right
value.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import (
    compute_aerodrome_v2_pool_address,
    compute_aerodrome_v3_pool_address,
)
from degenbot.aerodrome.functions import (
    generate_aerodrome_v2_pool_address,
    generate_aerodrome_v3_pool_address,
)

AERODROME_V2_DEPLOYER = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"
AERODROME_V2_IMPLEMENTATION = "0xA4e46b4f701c62e14DF11B48dCe76A7d793CD6d7"
AERODROME_V3_DEPLOYER = "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"
AERODROME_V3_IMPLEMENTATION = "0xeC8E5342B19977B4eF8892e02D8DAEcfa1315831"

BASE_WETH = "0x4200000000000000000000000000000000000006"
BASE_AERO = "0x940181a94a35a4569e4529a3cdfb74e38fd98631"
BASE_USDC = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA20429"
BASE_DAI = "0x50c5725949A6F0c72E6C4a641F24049A9Ee73273"


V2_CASES: list[tuple[str, str, bool]] = [
    (BASE_WETH, BASE_AERO, False),  # the on-chain volatile AERO/WETH pool
    (BASE_AERO, BASE_WETH, False),  # reversed token order (must match)
    (BASE_USDC, BASE_DAI, True),  # stable
    (BASE_DAI, BASE_USDC, True),  # stable reversed
    (BASE_USDC, BASE_WETH, False),
    (BASE_USDC, BASE_WETH, True),
]


V3_CASES: list[tuple[str, str, int]] = [
    (BASE_WETH, BASE_AERO, 200),  # the on-chain Slipstream AERO/WETH 200 pool
    (BASE_AERO, BASE_WETH, 200),  # reversed token order (must match)
    (BASE_USDC, BASE_WETH, 100),
    (BASE_DAI, BASE_USDC, 1),
    (BASE_AERO, BASE_DAI, 10_000),
]


@pytest.mark.parametrize(("t0", "t1", "stable"), V2_CASES)
def test_v2_rust_matches_python_oracle(t0: str, t1: str, *, stable: bool) -> None:
    py = generate_aerodrome_v2_pool_address(
        deployer_address=AERODROME_V2_DEPLOYER,
        token_addresses=(t0, t1),
        implementation_address=AERODROME_V2_IMPLEMENTATION,
        stable=stable,
    )
    rs = compute_aerodrome_v2_pool_address(
        AERODROME_V2_DEPLOYER,
        t0,
        t1,
        stable=stable,
        implementation_address=AERODROME_V2_IMPLEMENTATION,
    )
    assert rs == py, f"V2 {t0}/{t1} stable={stable}: rust={rs} py={py}"


@pytest.mark.parametrize(("t0", "t1", "tick_spacing"), V3_CASES)
def test_v3_rust_matches_python_oracle(t0: str, t1: str, tick_spacing: int) -> None:
    py = generate_aerodrome_v3_pool_address(
        deployer_address=AERODROME_V3_DEPLOYER,
        token_addresses=(t0, t1),
        implementation_address=AERODROME_V3_IMPLEMENTATION,
        tick_spacing=tick_spacing,
    )
    rs = compute_aerodrome_v3_pool_address(
        AERODROME_V3_DEPLOYER,
        t0,
        t1,
        tick_spacing,
        AERODROME_V3_IMPLEMENTATION,
    )
    assert rs == py, (
        f"V3 {t0}/{t1} ts={tick_spacing}: rust={rs} py={py}"
    )


def test_v2_known_onchain_address() -> None:
    # Pins the real on-chain BASE_AERO_WETH_V2 pool (matches the Rust
    # #[cfg(test)] fixture in degenbot-uniswap/src/create2.rs).
    assert (
        compute_aerodrome_v2_pool_address(
            AERODROME_V2_DEPLOYER,
            BASE_WETH,
            BASE_AERO,
            stable=False,
            implementation_address=AERODROME_V2_IMPLEMENTATION,
        )
        == "0x7f670f78B17dEC44d5Ef68a48740b6f8849cc2e6"
    )


def test_v3_known_onchain_address() -> None:
    # Pins the real on-chain BASE_AERO_WETH_V3 Slipstream pool (ts=200).
    assert (
        compute_aerodrome_v3_pool_address(
            AERODROME_V3_DEPLOYER,
            BASE_WETH,
            BASE_AERO,
            200,
            AERODROME_V3_IMPLEMENTATION,
        )
        == "0x82321f3BEB69f503380D6B233857d5C43562e2D0"
    )
