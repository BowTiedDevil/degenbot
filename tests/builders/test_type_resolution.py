"""Tests for the shared type resolution module.

Verifies the pure-logic ``pool_class_for_descriptor`` function and the
I/O-dependent ``resolve``/``fetch``/``probe`` functions in
``src/degenbot/builders/type_resolution.py``.

The resolve/fetch/probe functions call
``io.fetch_X()`` / ``io.probe_pool_type()`` directly (the Python ``io.call()``
parity-gate fallback is retired). The fakes here are duck-typed objects
exposing those ``fetch_*`` methods — no ``BotIo`` subclass needed (Q4 alpha):
builders never ``isinstance(io, BotIo)``.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from degenbot.balancer import BalancerFamily
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.builders.type_resolution import (
    fetch_factory_from_chain,
    pool_class_for_descriptor,
    resolve_pool_type,
    resolve_pool_type_by_probing,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.exceptions.base import DegenbotValueError
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import PoolFamily, PoolProbe, PoolTypeDescriptor
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

CHAIN_ID = 1


class TestPoolClassForDescriptor:
    """Tests for the pure pool_class_for_descriptor function."""

    def test_constant_product_returns_v2_class(self) -> None:
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant=None,
            kind="uniswap_v2",
            factory=None,
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert result is UniswapV2Pool

    def test_concentrated_liquidity_returns_v3_class(self) -> None:
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.CONCENTRATED_LIQUIDITY,
            variant=None,
            kind="uniswap_v3",
            factory=None,
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert result is UniswapV3Pool

    def test_unknown_family_raises(self) -> None:
        # PoolFamily.WEIGHTED has no default class registered
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.WEIGHTED,
            variant=None,
            kind="weighted",
            factory=None,
        )
        with pytest.raises(DegenbotValueError, match="No pool class for WEIGHTED"):
            pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)

    def test_stableswap_returns_curve_class(self) -> None:
        """A positive Curve identity (variant=None) keeps its default class."""
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant=None,
            kind="stableswap",
            factory=None,
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert result is CurveStableswapPool

    def test_stableswap_balancer_variant_without_registered_factory_is_balancer(self) -> None:
        """A Balancer identity never falls back to CurveStableswapPool.

        The variant is authoritative (probed on-chain or read from the DB kind);
        the factory only refines it to one deployment, so an unregistered
        revision (the MetaStablePool factory is not a registry row) still
        resolves to the Balancer stable class.
        """
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant="balancer_stable",
            kind="balancer_stable",
            factory=None,
        )
        assert pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID) is BalancerV2StablePool

    def test_weighted_balancer_variant_without_registered_factory_is_balancer(self) -> None:
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.WEIGHTED,
            variant="balancer_weighted",
            kind="balancer_weighted",
            factory=None,
        )
        assert pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID) is BalancerV2Pool

    def test_stableswap_unrecognized_variant_with_unregistered_factory_raises(self) -> None:
        """An unrecognized stableswap variant raises instead of silently
        defaulting to CurveStableswapPool — classify or raise, never guess."""
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant="mycustom",
            kind="mycustom",
            factory=None,
        )
        with pytest.raises(DegenbotValueError, match="Unrecognized stableswap variant"):
            pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)

    def test_unmodeled_balancer_family_has_no_class(self) -> None:
        """The unmodeled family exists to reach the construction switchboard,
        which refuses it under its own name — no wrapper class can exist, so the
        lookup refuses loudly instead of guessing a family's class."""
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.UNMODELED,
            variant="balancer_unmodeled",
            kind="balancer_unmodeled",
            factory=None,
        )
        with pytest.raises(DegenbotValueError, match="No pool class for family 'unmodeled'"):
            pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)

    def test_registered_factory_returns_registered_class(self) -> None:
        """When the descriptor has a registered factory, returns the registered class."""
        factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant=None,
            kind="uniswap_v2",
            factory=factory,  # type: ignore[arg-type]
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert issubclass(result, UniswapV2Pool)


class FakePyBotIo:
    """Duck-typed BotIo stand-in for type-resolution tests.

    Each seam method returns a configurable canned value; theresolve/fetch/
    probe functions only ever call ``fetch_factory_address`` /
    ``probe_pool_type`` / ``fetch_pool_row`` / ``fetch_exchange``.
    """

    def __init__(
        self,
        *,
        factory_address: str | None = None,
        probe_result: int = 0,
        pool_row: object | None = None,
        exchange_row: object | None = None,
        probe_exception: Exception | None = None,
        balancer_pool_id: bytes | None = None,
        balancer_family: BalancerFamily | None = None,
        balancer_factory: str | None = None,
    ) -> None:
        self._factory_address = factory_address
        self._probe_result = probe_result
        self._pool_row = pool_row
        self._exchange_row = exchange_row
        self._probe_exception = probe_exception
        self._balancer_pool_id = balancer_pool_id
        self._balancer_family = balancer_family
        self._balancer_factory = balancer_factory
        #: The candidate factories each membership probe was asked about.
        self.balancer_factory_candidates: list[str] = []

    def get_block_number(self) -> int:
        return 18_000_000

    def fetch_factory_address(self, _address: str) -> str | None:
        return self._factory_address

    def probe_pool_type(self, _address: str) -> int:
        if self._probe_exception is not None:
            raise self._probe_exception
        return self._probe_result

    def fetch_pool_row(self, chain_id: int, address: str) -> object | None:
        return self._pool_row

    def fetch_exchange(self, exchange_id: int) -> object | None:
        return self._exchange_row

    def fetch_balancer_pool_id(self, _address: str, block: object | None = None) -> bytes:
        if self._balancer_pool_id is None:
            msg = "getPoolId() reverted: not a Balancer pool"
            raise ValueError(msg)
        return self._balancer_pool_id

    def probe_balancer_pool_type(
        self, _address: str, block: object | None = None
    ) -> BalancerFamily:
        if self._balancer_family is None:
            msg = "neither getNormalizedWeights nor getAmplificationParameter answered"
            raise ValueError(msg)
        return self._balancer_family

    def fetch_balancer_factory(
        self,
        _address: str,
        factories: list[str],
        block: object | None = None,
    ) -> str | None:
        self.balancer_factory_candidates = list(factories)
        return self._balancer_factory


class TestFetchFactoryFromChain:
    """Tests for the sync fetch_factory_from_chain."""

    def test_returns_decoded_factory_address(self) -> None:
        factory_address = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        io = FakePyBotIo(factory_address=factory_address)
        result = fetch_factory_from_chain(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result is not None
        assert result.lower() == factory_address.lower()

    def test_returns_none_on_failure(self) -> None:
        io = FakePyBotIo(factory_address=None)
        result = fetch_factory_from_chain(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result is None


class TestResolvePoolTypeByProbing:
    """Tests for the sync resolve_pool_type_by_probing."""

    def test_v3_probe_returns_concentrated_liquidity(self) -> None:
        io = FakePyBotIo(probe_result=PoolProbe.V3)
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_v2_probe_returns_constant_product(self) -> None:
        io = FakePyBotIo(probe_result=PoolProbe.V2)
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.CONSTANT_PRODUCT

    def test_balancer_weighted_probe_returns_weighted(self) -> None:
        io = FakePyBotIo(probe_result=PoolProbe.BALANCER_WEIGHTED)
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.WEIGHTED
        assert result.variant == "balancer_weighted"

    def test_balancer_stable_probe_returns_stableswap_variant(self) -> None:
        io = FakePyBotIo(probe_result=PoolProbe.BALANCER_STABLE)
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.STABLESWAP
        assert result.variant == "balancer_stable"

    def test_curve_probe_returns_stableswap(self) -> None:
        """coins(uint256) answered (verified Curve tag) → STABLESWAP."""
        io = FakePyBotIo(probe_result=PoolProbe.STABLESWAP)
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.STABLESWAP
        assert result.variant is None

    def test_unknown_identity_probe_raises(self) -> None:
        """The Rust seam raises for an unmatched identity; wrap as DegenbotValueError."""
        io = FakePyBotIo(probe_exception=ValueError("unknown pool identity"))
        with pytest.raises(DegenbotValueError, match="no identity selector answered"):
            resolve_pool_type_by_probing(
                "0xPool",  # type: ignore[arg-type]
                chain_id=CHAIN_ID,
                factory="0xFactory",  # type: ignore[arg-type]
                io=io,
            )

    def test_unknown_probe_code_raises(self) -> None:
        """An unrecognized probe code is seam drift — it must raise, never
        silently classify as STABLESWAP."""
        io = FakePyBotIo(probe_result=99)
        with pytest.raises(DegenbotValueError, match="Unrecognized probe result"):
            resolve_pool_type_by_probing(
                "0xPool",  # type: ignore[arg-type]
                chain_id=CHAIN_ID,
                factory="0xFactory",  # type: ignore[arg-type]
                io=io,
            )


class TestResolvePoolType:
    """Tests for the full sync resolve_pool_type flow."""

    def test_answers_generic_curve_when_factory_fails_and_no_db_entry(self) -> None:
        # Discovery-only identity (Curve exposes no factory(), no DB row, no
        # Balancer membership): the resolver answers the generic Curve
        # descriptor rather than raising — the caller routes it through the ONE
        # construction entry's Curve arm. The unverified-contract loud-abort is
        # a different path (the probing arm), tested separately.
        io = FakePyBotIo(factory_address=None, pool_row=None)
        descriptor = resolve_pool_type(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert descriptor.family is PoolFamily.STABLESWAP
        assert descriptor.variant is None
        assert descriptor.factory is None

    def test_probes_when_no_db_entry(self) -> None:
        factory_address = "0x1234567890AbCdEf1234567890aBcDeF12345678"
        io = FakePyBotIo(
            factory_address=factory_address,
            pool_row=None,
            probe_result=PoolProbe.V3,
        )
        result = resolve_pool_type(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_resolves_from_db_via_seam(self) -> None:
        """A DB hit returns through the seam without probing."""
        factory_address = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        pool_row = MagicMock()
        pool_row.kind = "uniswap_v3"
        pool_row.exchange_id = 42
        exchange_row = MagicMock()
        exchange_row.factory = factory_address
        io = FakePyBotIo(
            factory_address=None,
            pool_row=pool_row,
            exchange_row=exchange_row,
        )

        result = resolve_pool_type(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY
        assert result.factory == factory_address

    def test_balancer_stable_identity_resolves_from_factory_membership(self) -> None:
        """A Balancer pool (`factory()` reverts) resolves to the registry row of
        the factory that owns it — the bug this path exists to fix: without it
        the address fell through to the generic Curve arm and was refused as
        out-of-spec."""
        # Balancer V2 ComposableStable Pool Factory (rev3) — a shipped chain-1
        # registry row; bb-s-USD reports it via `isPoolFromFactory`.
        composable_factory = "0xdba127fBc23fb20F5929C546af220A991b5C6e01"
        io = FakePyBotIo(
            factory_address=None,
            pool_row=None,
            balancer_pool_id=bytes(32),
            balancer_family=BalancerFamily.Stable,
            balancer_factory=composable_factory,
        )
        result = resolve_pool_type(
            "0x779d01F939D78a918A3de18cC236ee89221dfd4E",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result.family == PoolFamily.STABLESWAP
        assert result.variant == "balancer_stable"
        assert result.factory == get_checksum_address(composable_factory)
        # Only the stable factory rows are asked about a stable pool.
        assert io.balancer_factory_candidates
        registered_stable_factories = {
            descriptor.factory
            for (row_chain, _factory), (_cls, descriptor, _dep) in (
                pool_type_registry.registrations.items()
            )
            if row_chain == CHAIN_ID and descriptor.variant == "balancer_stable"
        }
        assert set(io.balancer_factory_candidates) == registered_stable_factories

    def test_balancer_identity_without_a_registered_factory_keeps_the_variant(self) -> None:
        """An unregistered factory revision (the MetaStablePool factory) still
        resolves to a Balancer identity — the variant is probed, not guessed."""
        io = FakePyBotIo(
            factory_address=None,
            pool_row=None,
            balancer_pool_id=bytes(32),
            balancer_family=BalancerFamily.Stable,
            balancer_factory=None,
        )
        result = resolve_pool_type(
            "0x32296969Ef14EB0c6d29669C550D4a0449130230",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result.family == PoolFamily.STABLESWAP
        assert result.variant == "balancer_stable"
        assert result.factory is None
        assert pool_class_for_descriptor(result, chain_id=CHAIN_ID) is BalancerV2StablePool

    def test_balancer_weighted_identity_resolves_to_the_weighted_family(self) -> None:
        weighted_factory = "0x897888115Ada5773E02aA29F775430BFB5F34c51"
        io = FakePyBotIo(
            factory_address=None,
            pool_row=None,
            balancer_pool_id=bytes(32),
            balancer_family=BalancerFamily.Weighted,
            balancer_factory=weighted_factory,
        )
        result = resolve_pool_type(
            "0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result.family == PoolFamily.WEIGHTED
        assert result.variant == "balancer_weighted"
        assert result.factory == get_checksum_address(weighted_factory)
        assert pool_class_for_descriptor(result, chain_id=CHAIN_ID) is BalancerV2Pool

    def test_balancer_identity_without_a_family_probe_is_unmodeled(self) -> None:
        """`getPoolId()` answered, neither family probe did: the identity is a
        Balancer pool of a shape no construction arm models (a linear pool, a
        boosted pool's constituent, a gyro shape).

        The identity must survive resolution under its own name — dropping it is
        what routed these addresses into the generic Curve arm, which refused
        them as a Curve pool.
        """
        io = FakePyBotIo(
            factory_address=None,
            pool_row=None,
            balancer_pool_id=bytes(32),
            balancer_family=None,
        )
        result = resolve_pool_type(
            "0x2B218683178d029BAB6c9789b1073aA6c96E5176",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result.family is PoolFamily.UNMODELED
        assert result.variant == "balancer_unmodeled"
        assert result.kind == "balancer_unmodeled"
        assert result.factory is None
        # No membership probe: the family never reached a factory claim.
        assert io.balancer_factory_candidates == []

    def test_raises_on_unknown_db_kind_without_probing(self) -> None:
        """A present DB kind the registry doesn't know raises instead of
        falling through to on-chain probing (which would re-classify)."""
        factory_address = "0x1234567890AbCdEf1234567890aBcDeF12345678"
        pool_row = MagicMock()
        pool_row.kind = "nonexistent_kind"
        pool_row.exchange_id = 7
        exchange_row = MagicMock()
        exchange_row.factory = factory_address
        io = FakePyBotIo(
            factory_address=factory_address,
            probe_result=PoolProbe.V3,
            pool_row=pool_row,
            exchange_row=exchange_row,
        )
        with pytest.raises(DegenbotValueError, match="Unrecognized pool kind"):
            resolve_pool_type(
                "0xPool",  # type: ignore[arg-type]
                chain_id=CHAIN_ID,
                io=io,
            )
