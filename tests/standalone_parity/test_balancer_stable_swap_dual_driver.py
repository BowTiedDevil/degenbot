"""Tier-2 behavioral dual-driver parity — Balancer V2 stable swap, ComposableStable `bpt_idx` path.

The behavioral companion to `rust/crates/degenbot/tests/
parity_balancer_stable_swap.rs`. Proves the **same** canonical fixture
produces the **same** `amount_out` through both consumers: the Python
companion path (`BalancerV2StablePool.calculate_tokens_out_from_tokens_in`,
a thin shell delegating to `LiquidityPool.calculate_tokens_out_for_pair`
→ `simulate_balancer_stable_swap_pair`) and the Rust core path
(`BotState::calculate_tokens_out_miss_aware` → `simulate_swap` →
`simulate_balancer_stable_swap` → `skip_bpt`).

The MetaStable (`bpt_idx = None`) counterpart in
`rust/crates/degenbot-pools/tests/pool_handle_balance_vector.rs` records the
same oracle value.

## The BPT-drop equivalence (the oracle)

The fixture is a 3-token `ComposableStablePool` `[token0, token1, BPT]` with
`bpt_idx = 2` (BPT at the END). After `_skip_bpt_index` / `skip_bpt` drops
the BPT, the invariant + outGivenIn run over **only** the two non-BPT
balances — byte-identical to the MetaStable (`bpt_idx = None`) fixture
(equal balances `1_000_000`, `amp = 100 * 1000`, ZERO fee, identity scaling,
`invariant_version = 2`). That MetaStable fixture records
`amount_in = 1_000 → amount_out = 989`. Therefore the ComposableStable
fixture MUST yield the same `989`; if `_skip_bpt_index` failed to drop the
BPT, the invariant would run over three balances and the output would
diverge. The Rust parity test re-derives the SAME `989` through the
BPT-drop path, so a fixture edit that drifts one side breaks BOTH tests.

## Index-past-BPT coverage

The `bpt_idx = 1` (BPT in the MIDDLE) `token0 → token2` case — one index
PAST the BPT — is not exercised here: this fixture keeps the BPT at the END
so the swap runs `0 ↔ 1` on both paths. The "index PAST the BPT" rebase
branch of `skip_bpt` is covered by a direct unit test in
`rust/crates/degenbot-pools/src/simulate_swap.rs`.
"""

from __future__ import annotations

from degenbot._ffi import Bot
from tests.helpers.balancer_pool_factory import make_balancer_stable_pool
from tests.helpers.erc20_factory import make_erc20

# ---- the shared canonical fixture (mirror in the Rust parity test) ----
# 3-token ComposableStable [token0, token1, BPT] with bpt_idx = 2 (BPT at end).
_BPT_IDX: int = 2
# Non-BPT balances mirror the MetaStable fixture (1_000_000 each).
_BALANCE0 = 1_000_000
_BALANCE1 = 1_000_000
_BALANCE_BPT = 1_000_000  # irrelevant — dropped before the invariant
_AMP = 100 * 1000  # on-chain amp = A * 1000
_SWAP_FEE = 0  # ZERO fee — isolates the stable math from the fee step
_INVARIANT_VERSION = 2
_ONE = 10**18  # identity scaling for 18dp tokens

_AMOUNT_IN = 1_000  # matches the MetaStable fixture's probe amount

# Canonical expected output. Equal to the MetaStable (`bpt_idx = None`)
# fixture's recorded `989` because the BPT is dropped from the invariant,
# leaving an identical 2-token stable swap. Independently re-derived by the
# Rust parity test (`rust/crates/degenbot/tests/parity_balancer_stable_swap.rs`).
_EXPECTED_AMOUNT_OUT = 989


class _CapturedRateProvider:
    """Non-static rate provider returning the identity rates (1e18 each).

    The ComposableStable `StaleRateResult` guard fires for pools with a
    static (or absent) rate provider — construction-time rates are treated as
    stale. This provider returns the identity scaling rates so the guard is
    satisfied and the swap math proceeds on the recorded rates (mirrors the
    pattern in `tests/balancer/test_balancer_stable_onchain_parity.py`'s
    `_CapturedRateProvider`).
    """

    requires_io_at_calculation_time = True

    def __init__(self, rates: tuple[int, ...]) -> None:
        self._rates = rates

    def get_rates(self, block_identifier: int | str | None = None) -> tuple[int, ...]:
        return self._rates


def _build_composable_stable_pool() -> object:
    """Build the shared ComposableStable fixture on a fresh short-lived Bot."""
    bot = Bot(chain_id=1)
    t0 = make_erc20(bot, "0x" + "0" * 38 + "aa", name="A", symbol="A", decimals=18)
    t1 = make_erc20(bot, "0x" + "0" * 38 + "bb", name="B", symbol="B", decimals=18)
    tbpt = make_erc20(bot, "0x" + "0" * 38 + "cc", name="BPT", symbol="BPT", decimals=18)
    return make_balancer_stable_pool(
        address="0x" + "0" * 38 + "dd",
        pool_id=b"\x00" * 12 + b"\x11" * 12 + b"\x00" * 8,
        vault="0x" + "0" * 38 + "ee",
        tokens=[t0, t1, tbpt],
        balances=[_BALANCE0, _BALANCE1, _BALANCE_BPT],
        fee=_SWAP_FEE,
        amp=_AMP,
        scaling_factors=[_ONE, _ONE, _ONE],
        bpt_idx=_BPT_IDX,
        invariant_version=_INVARIANT_VERSION,
        rate_provider=_CapturedRateProvider((_ONE, _ONE, _ONE)),
        py_bot=bot,
    ), (t0, t1)


def test_composable_stable_bpt_drop_matches_metastable_oracle() -> None:
    """The ComposableStable swap must equal the MetaStable oracle (989).

    If `_skip_bpt_index` failed to drop the BPT, the invariant would run over
    three balances and diverge from 989.
    """
    pool, (t0, t1) = _build_composable_stable_pool()
    amount_out = pool.calculate_tokens_out_from_tokens_in(t0, t1, _AMOUNT_IN)
    assert amount_out == _EXPECTED_AMOUNT_OUT, (
        f"ComposableStable bpt_idx={_BPT_IDX} swap gave {amount_out}, "
        f"expected {_EXPECTED_AMOUNT_OUT} (the MetaStable oracle); "
        "_skip_bpt_index may have failed to drop the BPT from the invariant"
    )


def test_composable_stable_bpt_drop_is_symmetric_on_equal_reserves() -> None:
    """On equal non-BPT reserves, token0→token1 and token1→token0 of the same
    amount_in MUST yield identical output (stableswap symmetry under equal
    balances). A BPT-drop bug that asymmetrically perturbed the invariant
    would break this.
    """
    pool, (t0, t1) = _build_composable_stable_pool()
    forward = pool.calculate_tokens_out_from_tokens_in(t0, t1, _AMOUNT_IN)
    reverse = pool.calculate_tokens_out_from_tokens_in(t1, t0, _AMOUNT_IN)
    assert forward == reverse, (
        f"ComposableStable swap must be symmetric on equal reserves: fwd={forward}, rev={reverse}"
    )


def test_composable_stable_bpt_drop_is_monotonic_and_bounded() -> None:
    """Larger amount_in → strictly larger amount_out (monotonic); output never
    exceeds the input (the conservation bound — no free money).

    (A strict sub-linearity `out(2x in) < 2x out(in)` is NOT robust here: with
    `amp = 100 * 1000` the curve is very flat near the peg, and integer
    flooring at small `amount_in` loses proportionally more than at large
    `amount_in`, so the ratio is dominated by rounding, not slippage.
    Monotonicity + the conservation bound are the robust sanity checks.)
    """
    pool, (t0, t1) = _build_composable_stable_pool()
    small = pool.calculate_tokens_out_from_tokens_in(t0, t1, _AMOUNT_IN)
    large = pool.calculate_tokens_out_from_tokens_in(t0, t1, 10 * _AMOUNT_IN)
    assert large > small, f"monotonicity violated: 10x amount_in gave {large} <= {small}"
    assert large <= 10 * _AMOUNT_IN, (
        f"conservation bound violated: amount_out {large} > amount_in "
        f"{10 * _AMOUNT_IN} (free money)"
    )
