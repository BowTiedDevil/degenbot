"""The build-failure taxonomy is typed across the FFI: skip by type, not message.

``Bot.build_pool`` can fail before admission is ever decided, and those
pre-admission arms are now classifiable by exception TYPE:

``ValueError``
└─ ``PoolRegistrationError``
   ├─ ``SpecViolationError``      — out of spec (permanent: the pool is refused)
   ├─ ``PoolBuildRpcError``       — the provider read failed (transient: retry)
   └─ ``PoolBuildDecodeError``    — the read answered a value no pool can hold
                                    (permanent for that state: retry re-reads it)

One ``except PoolRegistrationError:`` still covers the whole taxonomy; the
three subclasses let a caller choose retry-vs-skip without string matching.

The driver here is an ``OfflineProvider`` cassette for a Balancer V2 weighted
pool: a Balancer pool carries no ``factory()`` getter, so the type resolver
identifies it by ``getPoolId()`` + ``getNormalizedWeights()`` and the build then
reads the Vault's ``getPoolTokens``, the pool's ``getSwapFeePercentage``, and
per-token ``decimals()``. Leaving one read unrecorded is a genuine transport
fault; a recorded token reporting more than 18 decimals is a genuine decode
fault; a pool on the broken-pool set is a genuine spec refusal.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

import degenbot._ffi
import degenbot.exceptions
from degenbot.bot import Bot
from degenbot.crypto import function_selector
from degenbot.exceptions import (
    PoolBuildDecodeError,
    PoolBuildRpcError,
    PoolRegistrationError,
    SpecViolationError,
)
from degenbot.provider import OfflineProvider

if TYPE_CHECKING:
    import pathlib

# An arbitrary Balancer weighted pool (not on the broken-pool set).
POOL = "0x" + "aa" * 20
TOKEN0 = "0x" + "b0" * 20
TOKEN1 = "0x" + "b1" * 20
# `balancer/deployments.py::BALANCER_V2_VAULT_ADDRESS` — the singleton Vault.
VAULT = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
POOL_ID = "11" * 32
BLOCK = 100
# `BROKEN_BALANCER_V2_POOLS[0]` — swaps disabled at the contract (BAL#327).
BROKEN_POOL = "0x753BD6a5bF0b14ae7e5d2877e5cD6a3398aA2AAB"

_ONE = 10**18


def _selector(signature: str) -> str:
    """4-byte function selector (lowercase hex, ``0x``-prefixed)."""
    return "0x" + function_selector(signature).hex()


def _word_uint(value: int) -> str:
    """A 32-byte ABI ``uint256`` word (hex, no ``0x``)."""
    return f"{value:064x}"


def _word_address(address: str) -> str:
    """A 20-byte address left-padded to a 32-byte ABI ``address`` word."""
    return "000000000000000000000000" + address.lower()[2:]


def _call_key(to: str, signature: str, arg: str = "") -> str:
    """The ``OfflineProvider`` ``(to, calldata)`` ``eth_call`` lookup key."""
    return f"{to.lower()}:{_selector(signature)}{arg}"


def _pool_tokens_return(tokens: list[str], balances: list[int]) -> str:
    """ABI-encode the Vault's ``(address[], uint256[], uint256)`` return tuple."""
    head = _word_uint(0x60) + _word_uint(0xC0) + _word_uint(0)
    arrays = _word_uint(len(tokens)) + "".join(_word_address(t) for t in tokens)
    values = _word_uint(len(balances)) + "".join(_word_uint(b) for b in balances)
    return head + arrays + values


def _blocks(
    *,
    pool: str = POOL,
    token0_decimals: int = 18,
    omit: str | None = None,
) -> dict[str, dict]:
    """One recorded block serving the reads a weighted-pool build makes."""
    calls = {
        _call_key(pool, "getPoolId()"): POOL_ID,
        # A single dynamic return value encodes as an offset word plus the array.
        _call_key(pool, "getNormalizedWeights()"): (
            _word_uint(0x20) + _word_uint(2) + _word_uint(5 * 10**17) + _word_uint(5 * 10**17)
        ),
        _call_key(VAULT, "getPoolTokens(bytes32)", POOL_ID): _pool_tokens_return(
            [TOKEN0, TOKEN1], [_ONE, 2 * _ONE]
        ),
        _call_key(pool, "getSwapFeePercentage()"): _word_uint(10**16),
        _call_key(TOKEN0, "decimals()"): _word_uint(token0_decimals),
        _call_key(TOKEN1, "decimals()"): _word_uint(18),
    }
    if omit is not None:
        del calls[omit]
    return {str(BLOCK): {"timestamp": 1_700_000_000, "calls": calls, "code": {}}}


def _bot(tmp_path: pathlib.Path, blocks: dict[str, dict]) -> Bot:
    """A forkless Bot whose single provider serves one recorded block."""
    return Bot(
        chain_id=1,
        provider=OfflineProvider(chain_id=1, blocks=blocks),
        database=str(tmp_path / "build.db"),
    )


# ---------------------------------------------------------------------------
# Hierarchy: one parent, three distinguishable subclasses.
# ---------------------------------------------------------------------------


def test_build_failure_classes_are_exported_ffi_types_under_one_parent() -> None:
    """Both new classes are the FFI pyclasses and ride the admission parent."""
    assert degenbot.exceptions.PoolBuildRpcError is degenbot._ffi.PoolBuildRpcError
    assert degenbot.exceptions.PoolBuildDecodeError is degenbot._ffi.PoolBuildDecodeError
    for exc_type in (PoolBuildRpcError, PoolBuildDecodeError):
        assert issubclass(exc_type, PoolRegistrationError), exc_type
        # `PoolRegistrationError` roots at ValueError, so the broad
        # `except ValueError:` net in `build_paths` still catches these.
        assert issubclass(exc_type, ValueError), exc_type
    assert not issubclass(PoolBuildRpcError, PoolBuildDecodeError)
    assert not issubclass(PoolBuildDecodeError, PoolBuildRpcError)
    assert not issubclass(PoolBuildRpcError, SpecViolationError)
    assert not issubclass(PoolBuildDecodeError, SpecViolationError)


# ---------------------------------------------------------------------------
# Seam-triggered arms: the actual `Bot.build_pool` path.
# ---------------------------------------------------------------------------


def test_transport_fault_raises_pool_build_rpc_error(tmp_path: pathlib.Path) -> None:
    """An unreachable read is the retryable class, not a plain RuntimeError."""
    bot = _bot(tmp_path, _blocks(omit=_call_key(POOL, "getSwapFeePercentage()")))

    with pytest.raises(PoolBuildRpcError) as exc_info:
        bot.build_pool(POOL, state_block=BLOCK)

    message = str(exc_info.value)
    assert message.startswith("pool build RPC error: "), message
    assert "getSwapFeePercentage" not in message  # the selector is what surfaces
    assert not isinstance(exc_info.value, PoolBuildDecodeError)


def test_decode_fault_raises_pool_build_decode_error(tmp_path: pathlib.Path) -> None:
    """A read that answers an impossible value is the permanent class."""
    bot = _bot(tmp_path, _blocks(token0_decimals=20))

    with pytest.raises(PoolBuildDecodeError) as exc_info:
        bot.build_pool(POOL, state_block=BLOCK)

    message = str(exc_info.value)
    assert message.startswith("pool build decode failure: "), message
    assert "has 20 decimals (> 18, unsupported)" in message, message
    assert not isinstance(exc_info.value, PoolBuildRpcError)


def test_spec_refusal_still_raises_spec_violation_error(tmp_path: pathlib.Path) -> None:
    """The spec arm is unchanged: a broken pool keeps its own class."""
    bot = _bot(tmp_path, _blocks(pool=BROKEN_POOL))

    with pytest.raises(SpecViolationError) as exc_info:
        bot.build_pool(BROKEN_POOL, state_block=BLOCK)

    assert "balancer broken pool" in str(exc_info.value)
    assert not isinstance(exc_info.value, (PoolBuildRpcError, PoolBuildDecodeError))


@pytest.mark.parametrize(
    ("blocks", "pool"),
    [
        (_blocks(omit=_call_key(POOL, "getSwapFeePercentage()")), POOL),
        (_blocks(token0_decimals=20), POOL),
        (_blocks(pool=BROKEN_POOL), BROKEN_POOL),
    ],
    ids=["transport", "decode", "spec"],
)
def test_one_except_covers_the_whole_build_taxonomy(
    tmp_path: pathlib.Path,
    blocks: dict[str, dict],
    pool: str,
) -> None:
    """One `except PoolRegistrationError:` still catches every build refusal."""
    bot = _bot(tmp_path, blocks)

    with pytest.raises(PoolRegistrationError):
        bot.build_pool(pool, state_block=BLOCK)
