"""Tier-3 companion gate - python Balancer companions vs the rust engine.

The rust engine (degenbot-pools simulate_balancer_weighted/stable_swap) is
pinned byte-for-byte to canonical Solidity (balancer-v2-monorepo cores
vendored @ f8b6f44) by rust/crates/degenbot-pools/tests/
tier3_balancer_swap_vs_revm.rs. This gate pins the PYTHON companion methods
to that same engine across a decimals/weights/fee/pow-version/amount grid,
which transitively pins them to canonical Solidity.

(Executing the solidity harness directly from python was attempted: anvil
1.7.1 raises EVM error StackOverflow on the committed solc-0.7 runtime
bytecode even on legacy hardfork specs, while revm inside the rust test runs
it fine - see the epic task notes.)

Acceptance gate for the thin-shell migration (ergo epic GTDLLN): every commit
that touches the Balancer companions keeps this green.
"""

from __future__ import annotations

import contextlib
from fractions import Fraction

import pytest

from degenbot._ffi import Bot
from tests.helpers.balancer_pool_factory import (
    make_balancer_stable_pool,
    make_balancer_weighted_pool,
)
from tests.helpers.erc20_factory import make_erc20

_MULTS = (0.001, 0.01, 0.1, 0.25, 0.5, 0.75, 0.9)
_VAULT = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"

INVARIANT_V1 = 1
INVARIANT_V2 = 2


def _addr(prefix: int) -> str:
    from degenbot._ffi import to_checksum_address

    return to_checksum_address("0x" + f"{prefix:08x}" + "00" * 16)


def _token(bot: Bot, decimals: int, tag: int) -> Any:
    return make_erc20(
        bot,
        _addr(0x1000 + tag * 0x100 + decimals),
        name=f"T{decimals}_{tag}",
        symbol=f"T{decimals}_{tag}",
        decimals=decimals,
        chain_id=31337,
    )


def _sf(decimals: int) -> int:
    return 10 ** (18 - decimals)


class TestWeightedCompanionVsEngine:
    """WeightedPool: python companion == rust engine (both directions)."""

    @pytest.mark.parametrize("d0,d1", [(18, 18), (18, 6), (6, 18), (8, 18), (18, 8)])
    @pytest.mark.parametrize("w0,w1", [(50, 50), (80, 20), (20, 80)])
    @pytest.mark.parametrize("fee_frac", [Fraction(0), Fraction(3, 1000), Fraction(7, 10000)])
    @pytest.mark.parametrize("pow_version", [1, 2])
    def test_given_in_matches_engine(
        self,
        d0: int,
        d1: int,
        w0: int,
        w1: int,
        fee_frac: Fraction,
        pow_version: int,
    ) -> None:
        bot = Bot()
        t0, t1 = _token(bot, d0, 1), _token(bot, d1, 2)
        balances = [10_000 * 10 ** d0, 10_000 * 10 ** d1]
        weights = [w0 * 10 ** 16, w1 * 10 ** 16]
        lp = make_balancer_weighted_pool(
            _addr(0xAA01 + pow_version),
            pool_id=bytes([pow_version]) + bytes(range(31)),
            vault=_VAULT,
            tokens=[t0, t1],
            balances=balances,
            fee=fee_frac,
            weights=weights,
            pow_version=pow_version,
        )
        for i, zfo in ((0, True), (1, False)):
            t_in, t_out = (t0, t1)[i], (t0, t1)[1 - i]
            for mult in _MULTS:
                amount = int(balances[i] * mult)
                engine = lp._py_pool.calculate_tokens_out(zero_for_one=zfo, amount_in=amount)
                calc = lp.calculate_tokens_out_from_tokens_in(
                    token_in=t_in,
                    token_out=t_out,
                    token_in_quantity=amount,
                )
                assert calc == engine, (
                    f"dec=({d0},{d1}) w=({w0},{w1}) fee={fee_frac} pow={pow_version} "
                    f"mult={mult}: py={calc} engine={engine}"
                )


class TestStableCompanionVsEngine:
    """StablePool: python companion == rust engine (V1 + V2 invariants)."""

    def test_composable_v1_matches_engine(self) -> None:
        """ComposableStablePool (INVARIANT_V1, BPT in balances)."""
        bot = Bot()
        tokens = [
            _token(bot, 18, 9),
            _token(bot, 6, 10),
            make_erc20(bot, _addr(0xBEEF), name="BPT", symbol="BPT", decimals=18, chain_id=31337),
        ]
        balances = [25_000 * 10 ** 18, 25_000 * 10 ** 6, 12_345 * 10 ** 18]
        lp = make_balancer_stable_pool(
            _addr(0xBB02),
            pool_id=bytes(range(32)),
            vault=_VAULT,
            tokens=tokens,
            balances=balances,
            fee=Fraction(1, 1000),
            amp=250,
            scaling_factors=[_sf(18), _sf(6), _sf(18)],
            bpt_idx=2,
            invariant_version=INVARIANT_V1,
        )
        for i, zfo in ((0, True), (1, False)):
            t_in, t_out = tokens[i], tokens[1 - i]
            for mult in _MULTS:
                amount = int(balances[i] * mult)
                engine = lp._py_pool.calculate_tokens_out(zero_for_one=zfo, amount_in=amount)
                with contextlib.suppress(Exception):
                    calc = lp.calculate_tokens_out_from_tokens_in(
                        token_in=t_in,
                        token_out=t_out,
                        token_in_quantity=amount,
                    )
                assert calc == engine, (
                    f"composable V1 zfo={zfo} mult={mult}: py={calc} engine={engine}"
                )

    def test_metastable_v2_matches_engine(self) -> None:
        """MetaStablePool (INVARIANT_V2) with explicit scaling factors."""
        bot = Bot()
        tokens = [_token(bot, 18, 11), _token(bot, 18, 12)]
        balances = [100_000 * 10 ** 18, 100_000 * 10 ** 18]
        lp = make_balancer_stable_pool(
            _addr(0xBB03),
            pool_id=bytes(range(32)),
            vault=_VAULT,
            tokens=tokens,
            balances=balances,
            fee=Fraction(4, 10000),
            amp=1000,
            scaling_factors=[_sf(18), _sf(18)],
            bpt_idx=None,
            invariant_version=INVARIANT_V2,
        )
        for i, zfo in ((0, True), (1, False)):
            t_in, t_out = tokens[i], tokens[1 - i]
            for mult in _MULTS:
                amount = int(balances[i] * mult)
                engine = lp._py_pool.calculate_tokens_out(zero_for_one=zfo, amount_in=amount)
                with contextlib.suppress(Exception):
                    calc = lp.calculate_tokens_out_from_tokens_in(
                        token_in=t_in,
                        token_out=t_out,
                        token_in_quantity=amount,
                    )
                assert calc == engine, (
                    f"metastable V2 zfo={zfo} mult={mult}: py={calc} engine={engine}"
                )
