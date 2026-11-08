"""Tests for the path-composition rejection predicate.

A bot can refuse an ``EngineRegistry.register_path`` candidate by *policy*
(token denylist/allowlist, hop-count min/max, min-liquidity gate,
duplicate-pool guard) — distinct from the Rust core's
pool *admission* floor (``HookedPoolRejectedError`` /
``DynamicFeePoolRejectedError``). Policy rejections surface as a typed
``PathRejectedError`` family so callers classify by type, the same
typed-exception pattern the core uses.
"""

from __future__ import annotations

import pytest

from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.arbitrage.policy import (
    NoOpPathPredicate,
    PathCompositionPredicate,
    PathPolicy,
    touched_tokens,
)
from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import (
    ArbitrageError,
    DegenbotError,
    DegenbotValueError,
    DuplicatePoolError,
    HopCountExceededError,
    HopCountInsufficientError,
    InsufficientLiquidityError,
    PathRejectedError,
    TokenDenylistedError,
)
from tests.helpers.erc20_factory import make_native_token
from tests.helpers.v4_pool_factory import make_v4_pool
from tests.types.test_concrete_pool_construction import (
    V4_POOL_MANAGER,
    _make_uniswap_v2_pool,
    _make_uniswap_v3_pool,
    _make_uniswap_v4_pool,
    _make_weth,
)

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"


def _make_native_leg_v4_pool(bot):
    """A V4 managed pool whose currency0 is the conceptual native asset.

    The V4 ``address(0)`` currency is translated to the session's
    ``NativeToken`` at the construction outcome (ADR-097 D3); the pair is
    token0=native, token1=WETH.
    """
    return make_v4_pool(
        # keccak(abi.encode(pool_key)) — the companion verifies the ID against
        # the key, so the fixture carries the real derivation.
        pool_id="0xe541b44249066366e0edc2f6b71f431169e5747a50cbc3506e85203079af215a",
        pool_manager_address=V4_POOL_MANAGER,
        token0=make_native_token(bot),
        token1=_make_weth(bot),
        fee=500,
        tick_spacing=10,
        hook_address=ZERO_ADDRESS,
        sqrt_price_x96=2_198_666_895_605_149_686_863,
        tick=-76020,
        liquidity=9876543210,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=500000,
        state_block=18_000_000,
        py_bot=bot,
    )


class FakeArbitrageEngine:
    """Records register_and_solve_path calls; returns monotonic path ids.

    Identity lookups answer from a declared table standing in for the shared
    BotState (the core's identity owner); the registry keeps no map of its own.
    """

    def __init__(self) -> None:
        self.calls: list[list[tuple[int, bool]]] = []
        self._identities: dict[tuple[str, str], int] = {}
        self._next_id = 1

    def register_identity(self, family_tag: str, address: str, pool_id: int) -> None:
        self._identities[family_tag, address.lower()] = pool_id

    def pool_id_for_pool(self, family_tag: str, address: str) -> int | None:
        return self._identities.get((family_tag, address.lower()))

    def pool_id_for_v4_pool(self, pool_manager: str, pool_id_hex: str) -> int | None:
        return self._identities.get(("v4", f"{pool_manager}/{pool_id_hex}".lower()))

    def register_and_solve_path(self, hops: list[tuple[int, bool]]) -> int:
        self.calls.append(list(hops))
        path_id = self._next_id
        self._next_id += 1
        return path_id


def _registry_with_fake_engine(
    predicate=None,
) -> tuple[EngineRegistry, FakeArbitrageEngine]:
    fake = FakeArbitrageEngine()
    registry = EngineRegistry(bot=None, engine=fake, path_predicate=predicate)
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


def test_touched_tokens_captures_input_intermediate_output(py_bot) -> None:
    """touched_tokens returns input + every intermediate + output address.

    A 2-hop USDC→WETH→USDC path touches {USDC, WETH} (USDC is both input and
    final output, WETH the intermediate). Uses V2 + V4 (both USDC/WETH)."""
    v2a = _make_uniswap_v2_pool(py_bot)  # USDC/WETH (token0=USDC, token1=WETH)
    v4 = _make_uniswap_v4_pool(py_bot)  # USDC/WETH (currency0=USDC, currency1=WETH)
    # v2 zfo=True → in=USDC, out=WETH
    # v4 zfo=False → in=WETH, out=USDC
    pools_and_zfos = [(v2a, True), (v4, False)]
    touched = set(touched_tokens(pools_and_zfos))
    assert touched == {USDC, WETH}


def test_touched_tokens_preserves_order_uniqueness(py_bot) -> None:
    v2a = _make_uniswap_v2_pool(py_bot)
    pools_and_zfos = [(v2a, True)]
    assert touched_tokens(pools_and_zfos) == [USDC, WETH]


def test_touched_tokens_native_leg_keys_the_wire_currency(py_bot) -> None:
    """A V4 native leg keys as that protocol's ``address(0)`` currency.

    The hop key is the wire spelling — the same key space the core's
    ``touched_tokens`` (``policy.rs``) reads off the hop facts."""
    v4 = _make_native_leg_v4_pool(py_bot)
    assert touched_tokens([(v4, True)]) == [ZERO_ADDRESS, WETH]


# ---------------------------------------------------------------------------
# NoOpPathPredicate + Protocol structural conformance
# ---------------------------------------------------------------------------


def test_noop_predicate_accepts_everything(py_bot) -> None:
    predicate = NoOpPathPredicate()
    pools_and_zfos = [(_make_uniswap_v2_pool(py_bot), True)]
    # Returns None (no raise) for any input.
    assert predicate.evaluate(pools_and_zfos) is None


def test_predicates_satisfy_protocol() -> None:
    assert isinstance(NoOpPathPredicate(), PathCompositionPredicate)
    assert isinstance(PathPolicy(), PathCompositionPredicate)


# ---------------------------------------------------------------------------
# PathPolicy rules
# ---------------------------------------------------------------------------


def test_policy_default_accepts_any_path(py_bot) -> None:
    """A default PathPolicy (all checks off/relaxed) accepts a 1-hop path."""
    policy = PathPolicy()
    pools_and_zfos = [(_make_uniswap_v2_pool(py_bot), True)]
    assert policy.evaluate(pools_and_zfos) is None  # no raise


def test_policy_token_denylist_rejects_intermediate_token(py_bot) -> None:
    """A touched (intermediate) token on the denylist raises
    TokenDenylistedError carrying that token."""
    policy = PathPolicy(disallowed_tokens=frozenset({USDC}))
    pools_and_zfos = [(_make_uniswap_v2_pool(py_bot), True), (_make_uniswap_v3_pool(py_bot), False)]
    with pytest.raises(TokenDenylistedError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.token == USDC


def test_policy_token_denylist_case_insensitive_via_checksum(py_bot) -> None:
    """Denylist membership normalizes by checksummed address, so a lowercase
    denylist entry still matches the checksummed pool token address."""
    policy = PathPolicy(disallowed_tokens=frozenset({USDC.lower()}))
    pools_and_zfos = [(_make_uniswap_v2_pool(py_bot), True)]
    with pytest.raises(TokenDenylistedError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.token == USDC


def test_policy_denylist_sentinel_input_refused_loudly(py_bot) -> None:
    """A raw protocol sentinel in a policy input is refused, never resolved to
    the native asset (ADR-097 D1) — mirroring the seam's own config parse."""
    policy = PathPolicy(disallowed_tokens=frozenset({ZERO_ADDRESS}))
    with pytest.raises(DegenbotValueError, match="native-asset sentinel"):
        policy.evaluate([(_make_uniswap_v2_pool(py_bot), True)])


def test_policy_allowlist_excludes_the_native_leg(py_bot) -> None:
    """An address allowlist excludes the native leg: its wire key is a
    protocol sentinel no allowlist entry can name (ADR-097 D1)."""
    v4 = _make_native_leg_v4_pool(py_bot)
    policy = PathPolicy(allowed_tokens=frozenset({WETH}))
    with pytest.raises(TokenDenylistedError) as exc_info:
        policy.evaluate([(v4, True)])
    assert exc_info.value.token == ZERO_ADDRESS


def test_policy_token_allowlist_rejects_unknown_token(py_bot) -> None:
    """When an allowlist is set, any touched token absent from it rejects."""
    policy = PathPolicy(allowed_tokens=frozenset({WETH}))  # USDC not allowed
    pools_and_zfos = [(_make_uniswap_v2_pool(py_bot), True)]
    with pytest.raises(TokenDenylistedError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.token == USDC


def test_policy_token_allowlist_accepts_when_all_allowed(py_bot) -> None:
    policy = PathPolicy(allowed_tokens=frozenset({USDC, WETH}))
    pools_and_zfos = [(_make_uniswap_v2_pool(py_bot), True)]
    assert policy.evaluate(pools_and_zfos) is None


def test_policy_min_hops_rejects_too_short(py_bot) -> None:
    policy = PathPolicy(min_hops=3)
    pools_and_zfos = [(_make_uniswap_v2_pool(py_bot), True), (_make_uniswap_v3_pool(py_bot), False)]
    with pytest.raises(HopCountInsufficientError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.hop_count == 2
    assert exc_info.value.min_hops == 3


def test_policy_max_hops_rejects_too_long(py_bot) -> None:
    policy = PathPolicy(max_hops=1)
    pools_and_zfos = [(_make_uniswap_v2_pool(py_bot), True), (_make_uniswap_v3_pool(py_bot), False)]
    with pytest.raises(HopCountExceededError) as exc_info:
        policy.evaluate(pools_and_zfos)
    assert exc_info.value.hop_count == 2
    assert exc_info.value.max_hops == 1


def test_policy_duplicate_pool_rejects_same_pool_twice(py_bot) -> None:
    """The same pool object appearing twice in the path is rejected (a
    degenerate, non-executable cycle). V2/V3 keyed by address, V4 by pool_id."""
    pool = _make_uniswap_v2_pool(py_bot)
    policy = PathPolicy(reject_duplicate_pools=True)
    with pytest.raises(DuplicatePoolError):
        policy.evaluate([(pool, True), (pool, False)])


def test_policy_duplicate_pool_can_be_disabled(py_bot) -> None:
    pool = _make_uniswap_v2_pool(py_bot)
    policy = PathPolicy(reject_duplicate_pools=False)
    assert policy.evaluate([(pool, True), (pool, False)]) is None


def test_policy_duplicate_pool_distinguishes_v2_and_v3_by_address(py_bot) -> None:
    """Two different pools are not flagged as duplicates."""
    policy = PathPolicy(reject_duplicate_pools=True)
    pools_and_zfos = [(_make_uniswap_v2_pool(py_bot), True), (_make_uniswap_v3_pool(py_bot), False)]
    assert policy.evaluate(pools_and_zfos) is None


def test_policy_liquidity_gate_rejects_below_minimum(py_bot) -> None:
    """min_liquidity + liquidity_of extractor rejects pools below the floor."""
    v2 = _make_uniswap_v2_pool(py_bot)  # reserves 2_000_000e6 USDC / 1000e18 WETH

    def reserves_sum(pool) -> int:
        return pool.reserves_token0 + pool.reserves_token1

    policy = PathPolicy(min_liquidity=10**30, liquidity_of=reserves_sum)
    with pytest.raises(InsufficientLiquidityError) as exc_info:
        policy.evaluate([(v2, True)])
    assert exc_info.value.min_liquidity == 10**30
    assert exc_info.value.liquidity == reserves_sum(v2)


def test_policy_liquidity_gate_skipped_without_extractor(py_bot) -> None:
    """min_liquidity with no extractor is a no-op (the library does not encode
    pool-type-specific liquidity math — callers supply the proxy)."""
    v2 = _make_uniswap_v2_pool(py_bot)
    policy = PathPolicy(min_liquidity=10**30, liquidity_of=None)
    assert policy.evaluate([(v2, True)]) is None


def test_policy_rules_compose(py_bot) -> None:
    """All enabled rules run; the first violation (in rule order) raises."""
    # denylist + max_hops both violated; denylist is checked after hop-count,
    # so a 2-hop path under max_hops=1 raises HopCountExceededError first.
    v2 = _make_uniswap_v2_pool(py_bot)
    v4 = _make_uniswap_v4_pool(py_bot)
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


def test_register_path_invokes_predicate_before_engine(py_bot) -> None:
    """register_path runs the injected predicate BEFORE build_hops/dispatch — a
    rejection must surface as the typed exception and the engine must NOT be
    called."""
    registry, fake = _registry_with_fake_engine(
        predicate=PathPolicy(disallowed_tokens=frozenset({USDC}))
    )
    v2 = _make_uniswap_v2_pool(py_bot)
    fake.register_identity("v2", v2.address, 100)

    with pytest.raises(TokenDenylistedError):
        registry.register_path([(v2, True)])

    assert fake.calls == [], "engine must not be reached on a policy rejection"


def test_register_path_default_predicate_accepts(py_bot) -> None:
    registry, fake = _registry_with_fake_engine()  # no predicate → NoOp
    v2 = _make_uniswap_v2_pool(py_bot)
    fake.register_identity("v2", v2.address, 100)

    path_id = registry.register_path([(v2, True)])

    assert fake.calls == [[(100, True)]]
    assert isinstance(path_id, int)


def test_register_path_custom_predicate_injection(py_bot) -> None:
    """A caller-supplied predicate (structural, not PathPolicy) is honored."""
    calls: list[int] = []

    class RecordingPredicate:
        def evaluate(self, pools_and_zfos) -> None:
            calls.append(len(pools_and_zfos))

    registry, fake = _registry_with_fake_engine(predicate=RecordingPredicate())
    v2 = _make_uniswap_v2_pool(py_bot)
    fake.register_identity("v2", v2.address, 7)

    registry.register_path([(v2, True)])

    assert calls == [1]
    assert fake.calls == [[(7, True)]]


def test_engine_registry_predicate_is_noop_by_default() -> None:
    """Absent an injected predicate, the registry uses NoOpPathPredicate."""
    registry, _ = _registry_with_fake_engine()
    assert isinstance(registry.path_predicate, NoOpPathPredicate)
