"""Tests for the path-composition rejection predicate (Plan 102, D7KMQO).

A bot can refuse an ``ArbitragePath`` / ``EngineRegistry.register_path``
candidate by *policy* (token denylist/allowlist, hop-count min/max,
min-liquidity gate, duplicate-pool guard) — distinct from the Rust core's
pool *admission* floor (``HookedPoolRejectedError`` /
``DynamicFeePoolRejectedError``). Policy rejections surface as a typed
``PathRejectedError`` family so callers classify by type, mirroring the
Plan 102 typed-exception pattern.
"""

from __future__ import annotations

import pytest

import examples.eth_backrun_v2_v3_v4_rust as runner
from degenbot.arbitrage.policy import (
    NoOpPathPredicate,
    PathCompositionPredicate,
    PathPolicy,
    touched_tokens,
)
from degenbot.exceptions import (
    ArbitrageError,
    DegenbotError,
    DuplicatePoolError,
    HopCountExceededError,
    HopCountInsufficientError,
    InsufficientLiquidityError,
    PathRejectedError,
    TokenDenylistedError,
)
from tests.types.test_concrete_pool_construction import (
    _make_uniswap_v2_pool,
    _make_uniswap_v3_pool,
    _make_uniswap_v4_pool,
)

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"


class FakeUniswapArbEngine:
    """Records register_and_solve_path calls; returns monotonic path ids."""

    def __init__(self) -> None:
        self.calls: list[list[tuple[int, bool]]] = []
        self._next_id = 1

    def register_and_solve_path(self, hops: list[tuple[int, bool]]) -> int:
        self.calls.append(list(hops))
        path_id = self._next_id
        self._next_id += 1
        return path_id


def _registry_with_fake_engine(predicate=None) -> tuple[runner.EngineRegistry, FakeUniswapArbEngine]:
    fake = FakeUniswapArbEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake, path_predicate=predicate)
    return registry, fake


# ---------------------------------------------------------------------------
# Exception family
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("exc_cls", "kwargs"),
    [
        (TokenDenylistedError, {"token": WETH}),
        (HopCountExceededError, {"hop_count": 4, "max_hops": 3}),
        (HopCountInsufficientError, {"hop_count": 1, "min_hops": 2}),
        (InsufficientLiquidityError, {"liquidity": 10, "min_liquidity": 100}),
        (DuplicatePoolError, {"pool": "0xabc"}),
    ],
)
def test_rejection_exceptions_subclass_path_rejected_error(exc_cls, kwargs) -> None:
    """Every policy rejection type is a PathRejectedError (→ ArbitrageError)."""
    exc = exc_cls(**kwargs)
    assert isinstance(exc, PathRejectedError)
    assert isinstance(exc, ArbitrageError)
    assert isinstance(exc, DegenbotError)


def test_token_denylisted_error_carries_token() -> None:
    exc = TokenDenylistedError(token=USDC)
    assert exc.token == USDC


def test_path_rejected_error_is_not_value_error() -> None:
    """Policy rejection is an ArbitrageError, NOT a ValueError — it must not be
    confused with the Rust pool-admission floor (HookedPoolRejectedError /
    DynamicFeePoolRejectedError, both subclass ValueError). Distinct hierarchies
    so callers' ``except`` arms classify policy vs. admission separately."""
    assert not issubclass(PathRejectedError, ValueError)


# ---------------------------------------------------------------------------
# touched_tokens helper
# ---------------------------------------------------------------------------


def test_touched_tokens_captures_input_intermediate_output() -> None:
    """touched_tokens returns input + every intermediate + output address.

    A 2-hop USDC→WETH→USDC path touches {USDC, WETH} (USDC is both input and
    final output, WETH the intermediate). Uses V2 + V4 (both USDC/WETH)."""
    v2a = _make_uniswap_v2_pool()  # USDC/WETH (token0=USDC, token1=WETH)
    v4 = _make_uniswap_v4_pool()  # USDC/WETH (currency0=USDC, currency1=WETH)
    # v2 zfo=True → in=USDC, out=WETH
    # v4 zfo=False → in=WETH, out=USDC
    pools_and_zfos = [(v2a, True), (v4, False)]
    touched = set(touched_tokens(pools_and_zfos))
    assert touched == {USDC, WETH}


def test_touched_tokens_preserves_order_uniqueness() -> None:
    v2a = _make_uniswap_v2_pool()
    pools_and_zfos = [(v2a, True)]
    assert touched_tokens(pools_and_zfos) == [USDC, WETH]


# ---------------------------------------------------------------------------
# NoOpPathPredicate + Protocol structural conformance
# ---------------------------------------------------------------------------


def test_noop_predicate_accepts_everything() -> None:
    predicate = NoOpPathPredicate()
    pools_and_zfos = [(_make_uniswap_v2_pool(), True)]
    # Returns None (no raise) for any input.
    assert predicate.evaluate(pools_and_zfos) is None


def test_predicates_satisfy_protocol() -> None:
    assert isinstance(NoOpPathPredicate(), PathCompositionPredicate)
    assert isinstance(PathPolicy(), PathCompositionPredicate)


# ---------------------------------------------------------------------------
# PathPolicy rules
# ---------------------------------------------------------------------------


def test_policy_default_accepts_any_path() -> None:
    """A default PathPolicy (all checks off/relaxed) accepts a 1-hop path."""
    policy = PathPolicy()
    pools_and_zfos = [(_make_uniswap_v2_pool(), True)]
    assert policy.evaluate(pools_and_zfos) is None  # no raise


def test_policy_token_denylist_rejects_intermediate_token() -> None:
    """A touched (intermediate) token on the denylist raises
    TokenDenylistedError carrying that token."""
    policy = PathPolicy(disallowed_tokens=frozenset({USDC}))
    pools_and_zfos = [(_make_uniswap_v2_pool(), True), (_make_uniswap_v3_pool(), False)]
    with pytest.raises(TokenDenylistedError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.token == USDC


def test_policy_token_denylist_case_insensitive_via_checksum() -> None:
    """Denylist membership normalizes by checksummed address, so a lowercase
    denylist entry still matches the checksummed pool token address."""
    policy = PathPolicy(disallowed_tokens=frozenset({USDC.lower()}))
    pools_and_zfos = [(_make_uniswap_v2_pool(), True)]
    with pytest.raises(TokenDenylistedError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.token == USDC


def test_policy_token_allowlist_rejects_unknown_token() -> None:
    """When an allowlist is set, any touched token absent from it rejects."""
    policy = PathPolicy(allowed_tokens=frozenset({WETH}))  # USDC not allowed
    pools_and_zfos = [(_make_uniswap_v2_pool(), True)]
    with pytest.raises(TokenDenylistedError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.token == USDC


def test_policy_token_allowlist_accepts_when_all_allowed() -> None:
    policy = PathPolicy(allowed_tokens=frozenset({USDC, WETH}))
    pools_and_zfos = [(_make_uniswap_v2_pool(), True)]
    assert policy.evaluate(pools_and_zfos) is None


def test_policy_min_hops_rejects_too_short() -> None:
    policy = PathPolicy(min_hops=3)
    pools_and_zfos = [(_make_uniswap_v2_pool(), True), (_make_uniswap_v3_pool(), False)]
    with pytest.raises(HopCountInsufficientError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.hop_count == 2
    assert exc_info.value.min_hops == 3


def test_policy_max_hops_rejects_too_long() -> None:
    policy = PathPolicy(max_hops=1)
    pools_and_zfos = [(_make_uniswap_v2_pool(), True), (_make_uniswap_v3_pool(), False)]
    with pytest.raises(HopCountExceededError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.hop_count == 2
    assert exc_info.value.max_hops == 1


def test_policy_duplicate_pool_rejects_same_pool_twice() -> None:
    """The same pool object appearing twice in the path is rejected (a
    degenerate, non-executable cycle). V2/V3 keyed by address, V4 by pool_id."""
    pool = _make_uniswap_v2_pool()
    policy = PathPolicy(reject_duplicate_pools=True)
    with pytest.raises(DuplicatePoolError):
        policy.evaluate([(pool, True), (pool, False)])


def test_policy_duplicate_pool_can_be_disabled() -> None:
    pool = _make_uniswap_v2_pool()
    policy = PathPolicy(reject_duplicate_pools=False)
    assert policy.evaluate([(pool, True), (pool, False)]) is None


def test_policy_duplicate_pool_distinguishes_v2_and_v3_by_address() -> None:
    """Two different pools are not flagged as duplicates."""
    policy = PathPolicy(reject_duplicate_pools=True)
    pools_and_zfos = [(_make_uniswap_v2_pool(), True), (_make_uniswap_v3_pool(), False)]
    assert policy.evaluate(pools_and_zfos) is None


def test_policy_liquidity_gate_rejects_below_minimum() -> None:
    """min_liquidity + liquidity_of extractor rejects pools below the floor."""
    v2 = _make_uniswap_v2_pool()  # reserves 2_000_000e6 USDC / 1000e18 WETH

    def reserves_sum(pool) -> int:
        return pool.reserves_token0 + pool.reserves_token1

    policy = PathPolicy(min_liquidity=10**30, liquidity_of=reserves_sum)
    with pytest.raises(InsufficientLiquidityError) as exc_info:
        policy.evaluate([(v2, True)])
    assert exc_info.value.min_liquidity == 10**30
    assert exc_info.value.liquidity == reserves_sum(v2)


def test_policy_liquidity_gate_skipped_without_extractor() -> None:
    """min_liquidity with no extractor is a no-op (the library does not encode
    pool-type-specific liquidity math — callers supply the proxy)."""
    v2 = _make_uniswap_v2_pool()
    policy = PathPolicy(min_liquidity=10**30, liquidity_of=None)
    assert policy.evaluate([(v2, True)]) is None


def test_policy_rules_compose() -> None:
    """All enabled rules run; the first violation (in rule order) raises."""
    # denylist + max_hops both violated; denylist is checked after hop-count,
    # so a 2-hop path under max_hops=1 raises HopCountExceededError first.
    v2 = _make_uniswap_v2_pool()
    v4 = _make_uniswap_v4_pool()
    pools_and_zfos = [(v2, True), (v4, False)]
    policy = PathPolicy(
        disallowed_tokens=frozenset({USDC}),
        max_hops=1,
    )
    with pytest.raises(HopCountExceededError):
        policy.evaluate(pools_and_zfos)

    # With hop-count relaxed, the denylist violation surfaces.
    policy = PathPolicy(disallowed_tokens=frozenset({USDC}))
    with pytest.raises(TokenDenylistedError):
        policy.evaluate(pools_and_zfos)


# ---------------------------------------------------------------------------
# EngineRegistry wiring
# ---------------------------------------------------------------------------


def test_register_path_invokes_predicate_before_engine() -> None:
    """register_path runs the injected predicate BEFORE build_hops/dispatch — a
    rejection must surface as the typed exception and the engine must NOT be
    called."""
    registry, fake = _registry_with_fake_engine(
        predicate=PathPolicy(disallowed_tokens=frozenset({USDC}))
    )
    v2 = _make_uniswap_v2_pool()
    registry._v2_keys[v2.address] = 100

    with pytest.raises(TokenDenylistedError):
        registry.register_path([(v2, True)])

    assert fake.calls == [], "engine must not be reached on a policy rejection"


def test_register_path_default_predicate_accepts() -> None:
    registry, fake = _registry_with_fake_engine()  # no predicate → NoOp
    v2 = _make_uniswap_v2_pool()
    registry._v2_keys[v2.address] = 100

    path_id = registry.register_path([(v2, True)])

    assert fake.calls == [[(100, True)]]
    assert path_id in registry.paths


def test_register_path_custom_predicate_injection() -> None:
    """A caller-supplied predicate (structural, not PathPolicy) is honored."""
    calls: list[int] = []

    class RecordingPredicate:
        def evaluate(self, pools_and_zfos) -> None:
            calls.append(len(pools_and_zfos))

    registry, fake = _registry_with_fake_engine(predicate=RecordingPredicate())
    v2 = _make_uniswap_v2_pool()
    registry._v2_keys[v2.address] = 7

    registry.register_path([(v2, True)])

    assert calls == [1]
    assert fake.calls == [[(7, True)]]


def test_engine_registry_predicate_is_noop_by_default() -> None:
    """Absent an injected predicate, the registry uses NoOpPathPredicate."""
    registry, _ = _registry_with_fake_engine()
    assert isinstance(registry.path_predicate, NoOpPathPredicate)
