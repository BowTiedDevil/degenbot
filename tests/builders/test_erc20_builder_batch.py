"""Erc20Builder.build_many rides the core's batch construction entry.

`build_many` is the batch call shape over the SAME per-token core `build`
uses (`Bot.build_erc20_tokens` -> `build_erc20_metadata_batch`): one result
per input address, in input order, each token's DB-first / fallback /
write-back / registration completing independently. The Python-side batch
DB/network/fallback/registration choreography is retired onto those core
entries (ADR-023 A6).

"""

from __future__ import annotations

import json

import pytest

from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.exceptions import DegenbotValueError
from degenbot.registry import TokenRegistry
from tests.helpers.seam import AlloyProvider as RustAlloyProvider
from tests.helpers.seam import Bot

TOKEN_A = "0x00000000000000000000000000000000000000A1"
TOKEN_B = "0x00000000000000000000000000000000000000B1"


def _offline(code: dict[str, str], calls: dict[str, str]) -> RustAlloyProvider:
    return RustAlloyProvider.offline_from_json_string(
        json.dumps({
            "chain_id": 1,
            "block_number": 100,
            "timestamp": 1_700_000_000,
            "calls": calls,
            "code": code,
        })
    )


def test_build_many_resolves_each_token_in_order() -> None:
    """Two registry-missing tokens resolve through one core batch call, in order."""
    py_bot = Bot(chain_id=1)
    provider = _offline(code={TOKEN_A.lower(): "6080", TOKEN_B.lower(): "6080"}, calls={})
    py_bot.attach_construction_io(provider, None)
    tokens = TokenRegistry(py_bot=py_bot)
    erc20 = Erc20Builder(default_chain_id=1, tokens=tokens, py_bot=py_bot)

    t_a, t_b = erc20.build_many([TOKEN_A, TOKEN_B], chain_id=1, silent=True)

    assert t_a.address == t_a.address  # built, in input order
    assert (t_a.symbol, t_b.symbol) == ("UNKNOWN", "UNKNOWN")
    assert (t_a.decimals, t_b.decimals) == (18, 18)
    # Registration is per token and complete before the call returns.
    assert py_bot.get_token(TOKEN_A) is not None
    assert py_bot.get_token(TOKEN_B) is not None


def test_build_many_isolates_a_broken_token() -> None:
    """One unbuildable token is a failure of that entry alone — no all-or-nothing.

    The token that resolved stays registered when the batch surfaces the
    failure.
    """
    py_bot = Bot(chain_id=1)
    provider = _offline(code={TOKEN_A.lower(): "6080"}, calls={})
    py_bot.attach_construction_io(provider, None)
    tokens = TokenRegistry(py_bot=py_bot)
    erc20 = Erc20Builder(default_chain_id=1, tokens=tokens, py_bot=py_bot)

    with pytest.raises(DegenbotValueError, match=TOKEN_B):
        erc20.build_many([TOKEN_A, TOKEN_B], chain_id=1, silent=True)

    assert py_bot.get_token(TOKEN_A) is not None
    assert py_bot.get_token(TOKEN_B) is None


def test_build_many_duplicates_get_or_create() -> None:
    """A repeated address resolves to one canonical token, twice in order."""
    py_bot = Bot(chain_id=1)
    provider = _offline(code={TOKEN_A.lower(): "6080"}, calls={})
    py_bot.attach_construction_io(provider, None)
    tokens = TokenRegistry(py_bot=py_bot)
    erc20 = Erc20Builder(default_chain_id=1, tokens=tokens, py_bot=py_bot)

    first, second = erc20.build_many([TOKEN_A, TOKEN_A], chain_id=1, silent=True)

    assert first is second


def test_build_many_refuses_raw_native_sentinels() -> None:
    """A raw sentinel is not a token contract address — loud, before any I/O."""
    py_bot = Bot(chain_id=1)
    tokens = TokenRegistry(py_bot=py_bot)
    erc20 = Erc20Builder(default_chain_id=1, tokens=tokens, py_bot=py_bot)

    for sentinel in ("0x0000000000000000000000000000000000000000", "0x" + "Ee" * 20):
        with pytest.raises(DegenbotValueError, match="native-asset sentinel"):
            erc20.build_many([sentinel], chain_id=1, silent=True)
