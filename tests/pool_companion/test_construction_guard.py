"""Sealed companion construction seam (ADR-005) - collapsed, registry-driven.

Every pool/token companion is a Python wrapper over a Rust-owned handle: the
handle can only be produced by registering in a ``Bot`` (production) or the
test factories, and ``__init__`` is forbidden. This module collapses the
per-class ``TestDirectConstructionForbidden`` / ``TestWrongFamilyHandle``
classes into one parametrized body per invariant, keyed by the ``REGISTRY``
table (class, constructor-required kwargs, pool kind) so every class keeps a
distinct test id for sharp failure attribution.

ADR-005 pins the *types* - direct ``__init__`` raises ``TypeError`` and a
cross-family handle raises ``DegenbotValueError`` - NOT the message wording
(verified by ``rg`` across ``docs/adr``: ``Bot.build_pool`` appears only in
prose about return types / registration paths, never as a pinned literal).
Message-content assertions are therefore dropped here.
"""

from __future__ import annotations

from dataclasses import dataclass
from fractions import Fraction
from typing import Any
from unittest.mock import MagicMock

import pytest

from degenbot._ffi import Bot
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool


@dataclass(frozen=True)
class CompanionCase:
    """One companion class: its ``id``, the class, and the pre-seam kwargs.

    ``pool_kind`` records which family the class belongs to (used by the
    io-free no-I/O parametrization in ``test_io_free_construction.py``).
    """

    id: str
    cls: type
    pool_kind: str
    legacy_kwargs: dict[str, Any]


REGISTRY: tuple[CompanionCase, ...] = (
    CompanionCase(
        id="erc20",
        cls=Erc20Token,
        pool_kind="token",
        legacy_kwargs={"oracle_address": "0x" + "1" * 40, "state_cache_depth": 4},
    ),
    CompanionCase(
        id="v2",
        cls=UniswapV2Pool,
        pool_kind="v2",
        legacy_kwargs={
            "address": "0x" + "0" * 40,
            "token0": MagicMock(),
            "token1": MagicMock(),
            "factory": "0x" + "1" * 40,
            "fee_token0": Fraction(3, 1000),
            "fee_token1": Fraction(3, 1000),
        },
    ),
    CompanionCase(
        id="curve",
        cls=CurveStableswapPool,
        pool_kind="curve",
        legacy_kwargs={
            "address": "0x" + "0" * 40,
            "tokens": [MagicMock()],
            "a_coefficient": 100,
            "fee": 4_000_000,
            "admin_fee": 0,
        },
    ),
    CompanionCase(
        id="balancer-stable",
        cls=BalancerV2StablePool,
        pool_kind="balancer-stable",
        legacy_kwargs={
            "address": "0x" + "0" * 40,
            "pool_id": b"\x00" * 32,
            "vault": "0x" + "b" * 40,
            "tokens": [MagicMock()],
            "fee": MagicMock(),
            "amp": 100,
            "scaling_factors": [10**18],
        },
    ),
)


@pytest.mark.parametrize("shape", ["no_args", "legacy_kwargs"])
@pytest.mark.parametrize("case", REGISTRY, ids=[c.id for c in REGISTRY])
def test_direct_construction_raises_type_error(case: CompanionCase, shape: str) -> None:
    """Direct construction of every companion is rejected with ``TypeError``.

    The two shapes (no arguments / a fake handle + the pre-seam kwargs) are the
    old ``test_no_args_raises_type_error`` and ``test_with_*_raises_type_error``
    cases; they share one body because they assert the same invariant. Only the
    error *type* is asserted - the message wording is not ADR-pinned.
    """
    args: tuple[Any, ...] = () if shape == "no_args" else (MagicMock(),)
    kwargs = {} if shape == "no_args" else case.legacy_kwargs
    with pytest.raises(TypeError):
        case.cls(*args, **kwargs)


@dataclass(frozen=True)
class WrongFamilyCase:
    """A companion class fed a V2-family handle: must raise, not mis-read."""

    id: str
    target_cls: type


WRONG_FAMILY: tuple[WrongFamilyCase, ...] = (
    WrongFamilyCase(id="curve", target_cls=CurveStableswapPool),
    WrongFamilyCase(id="balancer-stable", target_cls=BalancerV2StablePool),
)


@pytest.mark.parametrize("case", WRONG_FAMILY, ids=[c.id for c in WRONG_FAMILY])
def test_v2_handle_raises_degenbot_value_error(case: WrongFamilyCase) -> None:
    """A V2 handle wrapped as another family raises ``DegenbotValueError``.

    ADR-005 pins the *type* (the ``pool_family()`` discriminator guard); the
    message is not pinned, so only the exception type is asserted.
    """
    bot = Bot()
    t0 = make_erc20(bot, address="0x" + "a1" * 20, name="T0", symbol="T0", decimals=18)
    t1 = make_erc20(bot, address="0x" + "b2" * 20, name="T1", symbol="T1", decimals=18)
    v2_pool = make_v2_pool(
        "0x" + "c3" * 20,
        token0=t0,
        token1=t1,
        factory="0x" + "ff" * 20,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=10**18,
        reserves_token1=10**18,
        py_bot=bot,
    )

    with pytest.raises(DegenbotValueError):
        case.target_cls._from_py_pool(v2_pool._py_pool)
