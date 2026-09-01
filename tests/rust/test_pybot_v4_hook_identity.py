"""V4 identity round-trip with a REAL hook address (pool-ID mismatch regression).

Incident: during live runs, every V4 pool with a nonzero hook contract address
failed at `UniswapV4Pool._from_py_pool` with

    Skip V4 <id>: Supplied pool ID <id> does not match calculated ID <other>

The Rust core hardcoded `hooks: Address::ZERO` into the registered pool key
and kept only the 16-bit flag mask, so the companion's re-derivation
`keccak(abi.encode(pool_key))` never matched the onchain pool ID.

Observed mainnet incident vector (from the DB row for pool
0xde17806ed61e8840767d6532bd4fc75660c5edf9a122f994b042e5c4974cb9c1):
WBTC/USDC, fee=0, tick_spacing=10, hooks=0x64f4861c0A45C0Ab9Ec7E4B076dCFA05898f4888.
keccak(abi.encode(that key)) reproduces the onchain ID exactly — the identity
was always correct; only the Rust registration dropped the hook address.

These tests pin the fix: the handle identity must expose the real hook
address, and a hooked pool constructed via `make_v4_pool` (the I/O-free test
seam over `Bot.register_v4_pool`) must round-trip its pool ID through
`keccak(abi.encode(pool_key))`. Also pins the ADR-037 guard surface: the
core derives hook flags FROM the stored address, so the guard and the identity
can never disagree.
"""

from __future__ import annotations

from degenbot.abi import encode
from degenbot.checksum_cache import get_checksum_address
from degenbot.crypto import keccak256
from degenbot.utils.bytes import to_0x_hex
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v4_pool_factory import make_v4_pool

# The exact incident key: mainnet WBTC/USDC, fee 0, tick spacing 10, hooked.
_HOOKS = "0x64f4861c0A45C0Ab9Ec7E4B076dCFA05898f4888"
_HOOKS_CHECKSUMMED = get_checksum_address(_HOOKS)
_CURRENCY0 = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"  # WBTC
_CURRENCY1 = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"  # USDC
_FEE = 0
_TICK_SPACING = 10
# Recorded from the incident DB row (== the onchain Initialize event id).
_INCIDENT_POOL_ID = "0xde17806ed61e8840767d6532bd4fc75660c5edf9a122f994b042e5c4974cb9c1"


def _fresh_bot():
    from degenbot._ffi import Bot

    return Bot()


def _pool_id_for_key(
    currency0: str,
    currency1: str,
    fee: int,
    tick_spacing: int,
    hooks: str,
) -> str:
    """keccak(abi.encode(poolKey)) — v4-core PoolId derivation."""
    return "0x" + keccak256(
        encode(
            types=["address", "address", "uint24", "int24", "address"],
            args=[currency0, currency1, fee, tick_spacing, hooks],
        ),
    ).hex()


def test_incident_pool_key_hashes_to_recorded_id() -> None:
    """The incident DB key derivation reproduces the onchain Initialize ID."""
    assert (
        _pool_id_for_key(
            _CURRENCY0, _CURRENCY1, _FEE, _TICK_SPACING, _HOOKS_CHECKSUMMED
        )
        == _INCIDENT_POOL_ID
    ), "incident vector changed — the recorded identity must stay pinned"


def test_hooked_pool_round_trips_pool_id_through_identity() -> None:
    """The full incident path: register a hooked pool under its onchain ID.

    Before the fix this raised AssertionError from `_from_py_pool`: the
    registered pool key carried hooks=0x0, so the companion's re-derived ID
    missed the recorded one.
    """
    pool = make_v4_pool(
        pool_id=_INCIDENT_POOL_ID,
        # mainnet V4 PoolManager singleton
        pool_manager_address="0x000000000004444c5dc75cb358380d2e3de08a90",
        token0=make_erc20(
            bot := _fresh_bot(),
            _CURRENCY0,
            name="Wrapped Bitcoin",
            symbol="WBTC",
            decimals=8,
        ),
        token1=make_erc20(
            bot,
            _CURRENCY1,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
        ),
        py_bot=bot,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        hook_address=_HOOKS_CHECKSUMMED,
        sqrt_price_x96=1 << 96,
        tick=0,
        liquidity=1_000_000,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=0,
    )

    # The handle identity must expose the REAL hook address (not 0x0).
    assert pool.hook_address == _HOOKS_CHECKSUMMED
    # The pool key must re-derive the onchain pool ID.
    assert pool.pool_key.hooks == _HOOKS_CHECKSUMMED
    assert _pool_id_for_key(
        pool.pool_key.currency0,
        pool.pool_key.currency1,
        pool.pool_key.fee,
        pool.pool_key.tick_spacing,
        pool.pool_key.hooks,
    ) == to_0x_hex(pool.pool_id)
