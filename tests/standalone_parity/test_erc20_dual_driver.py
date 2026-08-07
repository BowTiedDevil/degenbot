"""Tier-2 behavioral dual-driver parity — ERC-20 metadata resolution.

The behavioral companion to the Rust `rust/crates/degenbot/tests/parity_erc20.rs`
test. Proves the **same** canonical ERC-20 fixture driven through the **Python
consumer** (`PyBot.build_erc20_token`, the PyO3 binding) resolves the **same**
`(name, symbol, decimals)` as the Rust consumer (`build_erc20_metadata` against
a `ConstructionIo`).

VK3YDM-S2 moved the ERC-20 *assembly* (DB-first metadata lookup, on-chain read,
UNKNOWN fallback, write-back, `BotState` registration) into the Rust core, so
both this test and its Rust twin must agree — divergence = a lossy FFI seam on
the metadata resolution that the pool-family parities cannot catch.

## The shared contract (HRT356 — single source of truth)

The plain canonical metadata is loaded from the SHARED file
`tests/standalone_parity/fixtures/erc20_build.json`, which the Rust parity test
ALSO loads. Both sides ABI-encode the SAME `name`/`symbol`/`decimals` from
`fixture` into their provider doubles (an offline alloy provider here) and
assert the resolved output equals `expected`. A fixture edit that drifts the
metadata fails BOTH sides mechanically — the shared-fixture contract that
replaced copied constants.
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider
from degenbot.bot import PyBot
from degenbot.crypto import function_selector

# ---- the shared canonical fixture (loaded, not copied) ----
_FIXTURE_PATH = Path(__file__).parent / "fixtures" / "erc20_build.json"
_FIXTURE = json.loads(_FIXTURE_PATH.read_text())
_F = _FIXTURE["fixture"]
_E = _FIXTURE["expected"]

_TOKEN = _F["token"]  # canonical token address (lowercase hex, no checksum)
_NAME = _F["name"]
_SYMBOL = _F["symbol"]
_DECIMALS = _F["decimals"]

_EXPECTED_NAME = _E["name"]
_EXPECTED_SYMBOL = _E["symbol"]
_EXPECTED_DECIMALS = _E["decimals"]


def _abi_string(s: str) -> str:
    """ABI-encode a top-level `string` return as a no-0x hex string."""
    n = len(s)
    return (
        (32).to_bytes(32, "big")
        + n.to_bytes(32, "big")
        + s.encode()
        + b"\0" * ((32 - (n % 32)) % 32)
    ).hex()


def _abi_uint(v: int) -> str:
    """ABI-encode a single `uint256` return as a no-0x hex string."""
    return v.to_bytes(32, "big").hex()


def _offline_provider() -> RustAlloyProvider:
    """An offline alloy provider serving the state.

    `code` is non-empty so the contract-present guard passes; `calls` holds the
    three canonical metadata reads keyed by `0x<addr>:0x<selector>`. Both the
    address (lowercase) and the selectors are derived from the shared fixture,
    mirroring how the Rust FakeRpc ABI-encodes the same plain values.
    """
    addr_no_x = _TOKEN[2:]  # strip the 0x prefix
    calls = {
        f"0x{addr_no_x}:0x{function_selector('name()').hex()}": _abi_string(_NAME),
        f"0x{addr_no_x}:0x{function_selector('symbol()').hex()}": _abi_string(_SYMBOL),
        f"0x{addr_no_x}:0x{function_selector('decimals()').hex()}": _abi_uint(_DECIMALS),
    }
    return RustAlloyProvider.offline_from_json_string(
        json.dumps(
            {
                "chain_id": 1,
                "block_number": 100,
                "timestamp": 1_700_000_000,
                "calls": calls,
                "code": {f"0x{addr_no_x}": "6080"},
            }
        )
    )


def test_erc20_metadata_dual_driver_matches_rust() -> None:
    """The Python consumer resolves the SAME metadata as the Rust consumer."""
    bot = PyBot(chain_id=1)
    bot.attach_construction_io(_offline_provider(), None)

    token = bot.build_erc20_token(_TOKEN, 1)

    assert token.name == _EXPECTED_NAME, "name must match the shared fixture"
    assert token.symbol == _EXPECTED_SYMBOL, "symbol must match the shared fixture"
    assert token.decimals == _EXPECTED_DECIMALS, "decimals must match the shared fixture"
    # Registration into BotState (ADR-006) is part of the same core call.
    assert bot.get_token(_TOKEN) is not None, "token must be registered in BotState"
