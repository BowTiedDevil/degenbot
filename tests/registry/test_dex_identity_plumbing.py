"""ADR-005 slice 7 step 3 — additive `dex` plumbing tests.

Non-breaking foundation: the pool type registry gains a `dex_identity`
field, `LiquidityPool.__init__` gains an additive `dex` param (explicit
params still take precedence — existing callers unaffected), and `make_v2_pool`
threads `dex=` through. The DEX `__init__.py` self-registration sites pass
the canonical preset for each DEX.

These tests verify the additive plumbing without exercising the breaking
subclass collapse (step 4): the 4 V2 subclasses still exist + still carry
their `variant` ClassVar; `dex` rides alongside as metadata.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot, PyDexIdentity, dex_identity
from degenbot.erc20.erc20 import Erc20Token
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.liquidity_pool import LiquidityPool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

_PY_BOT = PyBot()


def _make_token(addr: str, *, symbol: str, decimals: int, name: str = "") -> Erc20Token:
    return make_erc20(_PY_BOT, addr, name=name or symbol, symbol=symbol, decimals=decimals)


# Canonical-chain factories (from src/degenbot/{uniswap,sushiswap,pancakeswap,
# swapbased,camelot}/__init__.py — cross-checked against the Rust DexIdentity
# presets in slice 6).
UNISWAP_V2_MAINNET_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
SUSHISWAP_V2_MAINNET_FACTORY = "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
PANCAKESWAP_V2_MAINNET_FACTORY = "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362"
SWAPBASED_FACTORY = "0x04C9f118d21e8B767D2e50C946f0cC9F6C367300"
CAMELOT_FACTORY = "0x6EcCab422D763aC031210895C81787E87B43A652"

UNISWAP_INIT_HASH = "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"

_TEST_CHAIN = 999
_TEST_FACTORY = "0x" + "11" * 20


@pytest.fixture(autouse=True)
def _clean_test_factory():
    """Ensure the _TEST_FACTORY is unregistered before + after each test."""
    if pool_type_registry.has_registration(_TEST_CHAIN, _TEST_FACTORY):
        pool_type_registry.unregister(chain_id=_TEST_CHAIN, factory_address=_TEST_FACTORY)
    yield
    if pool_type_registry.has_registration(_TEST_CHAIN, _TEST_FACTORY):
        pool_type_registry.unregister(chain_id=_TEST_CHAIN, factory_address=_TEST_FACTORY)


class TestRegistryDexIdentity:
    """`register()` stores `dex_identity`; `get_v2_identity()` resolves it."""

    def test_register_with_dex_identity_round_trips_through_get_v2_identity(self) -> None:
        """register(dex_identity=...) → get_v2_identity() returns the same preset."""
        uniswap = dex_identity("uniswap-v2")
        assert uniswap is not None
        pool_type_registry.register(
            LiquidityPool,
            chain_id=_TEST_CHAIN,
            factory_address=_TEST_FACTORY,
            pool_init_hash=UNISWAP_INIT_HASH,
            dex_identity=uniswap,
        )
        try:
            resolved = pool_type_registry.get_v2_identity(_TEST_CHAIN, _TEST_FACTORY)
            assert resolved is not None
            assert resolved.variant == "uniswap-v2"
            assert resolved.factory == UNISWAP_V2_MAINNET_FACTORY
            assert resolved.init_hash == UNISWAP_INIT_HASH
        finally:
            pool_type_registry.unregister(chain_id=_TEST_CHAIN, factory_address=_TEST_FACTORY)

    def test_get_v2_identity_returns_none_when_registered_without_preset(self) -> None:
        """A registration without dex_identity → get_v2_identity() returns None."""
        pool_type_registry.register(
            LiquidityPool,
            chain_id=_TEST_CHAIN,
            factory_address=_TEST_FACTORY,
        )
        assert pool_type_registry.get_v2_identity(_TEST_CHAIN, _TEST_FACTORY) is None

    def test_get_v2_identity_returns_none_for_unregistered_factory(self) -> None:
        """get_v2_identity() returns None for an unregistered (chain, factory)."""
        assert pool_type_registry.get_v2_identity(88888, "0x" + "22" * 20) is None

    def test_dex_identity_is_independent_of_pool_class_lookup(self) -> None:
        """get_v2_class() still returns the class; get_v2_identity() the preset.

        Both coexist on the same registry entry — step 3 keeps the class lookup
        (subclasses still registered) AND adds the identity lookup alongside.
        """
        uniswap = dex_identity("uniswap-v2")
        assert uniswap is not None
        pool_type_registry.register(
            LiquidityPool,
            chain_id=_TEST_CHAIN,
            factory_address=_TEST_FACTORY,
            pool_init_hash=UNISWAP_INIT_HASH,
            dex_identity=uniswap,
        )
        try:
            assert pool_type_registry.get_v2_class(_TEST_CHAIN, _TEST_FACTORY) is LiquidityPool
            resolved = pool_type_registry.get_v2_identity(_TEST_CHAIN, _TEST_FACTORY)
            assert resolved is uniswap
        finally:
            pool_type_registry.unregister(chain_id=_TEST_CHAIN, factory_address=_TEST_FACTORY)


class TestSingletonDexPresets:
    """The 5 V2-family DEX self-registration sites now carry dex_identity.

    After `import degenbot` (which triggers each DEX module's self-
    registration), `get_v2_identity()` must return the canonical preset for
    each registered V2 DEX factory. This is the cross-check that the
    `__init__.py` self-reg sites were updated to pass `dex_identity=`.
    """

    @pytest.mark.parametrize(
        ("chain_id", "factory", "expected_variant"),
        [
            (1, UNISWAP_V2_MAINNET_FACTORY, "uniswap-v2"),
            (1, SUSHISWAP_V2_MAINNET_FACTORY, "sushiswap-v2"),
            (1, PANCAKESWAP_V2_MAINNET_FACTORY, "pancakeswap-v2"),
            (8453, SWAPBASED_FACTORY, "swapbased-v2"),
            (42161, CAMELOT_FACTORY, "camelot-v2-volatile"),
            # Uniswap V2 also deploys on Base — same preset variant, different factory.
            (
                8453,
                "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6",
                "uniswap-v2",
            ),
            # Sushi V2 on Arbitrum.
            (
                42161,
                "0xc35DADB65012eC5796536bD9864eD8773aBc74C4",
                "sushiswap-v2",
            ),
        ],
    )
    def test_preset_registered_for_each_v2_dex_factory(
        self, chain_id: int, factory: str, expected_variant: str
    ) -> None:
        """get_v2_identity() returns the preset matching the registered factory."""
        ident = pool_type_registry.get_v2_identity(chain_id, factory)
        assert ident is not None, (
            f"no dex_identity registered for chain {chain_id} factory {factory}"
        )
        assert ident.variant == expected_variant, (
            f"expected {expected_variant}, got {ident.variant}"
        )

    def test_preset_fields_match_rust_constants(self) -> None:
        """The registered preset's fields match the slice-6 Rust constants."""
        ident = pool_type_registry.get_v2_identity(1, UNISWAP_V2_MAINNET_FACTORY)
        assert ident is not None
        # Cross-checked against `UNISWAP_V2` Rust preset (slice 6).
        assert ident.factory == UNISWAP_V2_MAINNET_FACTORY
        assert ident.deployer == ident.factory  # Uniswap V2 uses factory as deployer
        assert ident.init_hash == UNISWAP_INIT_HASH
        # (gamma_numer, fee_denom) = (997, 1000) for a 3/1000 fee.
        assert ident.fee_token0 == (997, 1000)
        assert ident.fee_token1 == (997, 1000)
        assert ident.reserves_abi == ["uint112", "uint112"]

    def test_aerodrome_v2_has_no_dex_identity_yet(self) -> None:
        """Aerodrome V2 is deferred (TODO-e30504ed) — no dex_identity registered.

        The Aerodrome V2 factory IS registered (with AerodromeV2Pool as the
        pool_class), but step 3 does NOT wire a dex_identity for it — Aerodrome
        doesn't collapse in slice 7. So get_v2_identity() returns None for it,
        even though get_v2_class() returns AerodromeV2Pool.
        """
        from degenbot.aerodrome.pools import (  # noqa: PLC0415
            AerodromeV2Pool,
        )

        aerodrome_factory = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"
        assert pool_type_registry.get_v2_class(8453, aerodrome_factory) is AerodromeV2Pool
        assert pool_type_registry.get_v2_identity(8453, aerodrome_factory) is None


class TestLiquidityPoolDexIdentity:
    """`LiquidityPool.__init__` gains an additive `dex` param.

    When `dex` is provided AND explicit factory/init_hash/fees are NOT, the
    preset fills them in. When explicit params are also provided, explicit
    takes precedence (non-breaking — existing callers pass all explicit params).
    """

    @property
    def _uniswap_preset(self) -> PyDexIdentity:
        ident = dex_identity("uniswap-v2")
        assert ident is not None
        return ident

    def test_dex_stored_on_pool_and_provides_factory_when_omitted(self) -> None:
        """LiquidityPool(dex=preset, no explicit factory/init_hash/fees) fills from preset."""

        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        uniswap = self._uniswap_preset

        pool = make_v2_pool(
            "0x" + "cc" * 20,
            token0=token0,
            token1=token1,
            dex=uniswap,
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
            # NO factory, fee_token0/1, init_hash — all resolved from `dex`.
        )
        assert pool.dex is uniswap
        # factory defaults to the preset's (canonical-chain) factory.
        assert pool.factory == uniswap.factory
        # init_hash defaults to the preset's.
        assert pool.init_hash == uniswap.init_hash
        # fee_token0/1 are FEE Fractions (gamma → fee conversion):
        # preset (997, 1000) → Fraction(1000-997, 1000) = Fraction(3, 1000).
        assert pool.fee_token0 == Fraction(3, 1000)
        assert pool.fee_token1 == Fraction(3, 1000)

    def test_explicit_params_override_dex_defaults(self) -> None:
        """When both dex + explicit factory/fees given, explicit wins (non-breaking)."""

        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        uniswap = self._uniswap_preset
        explicit_factory = "0x" + "ee" * 20

        pool = make_v2_pool(
            "0x" + "cc" * 20,
            token0=token0,
            token1=token1,
            dex=uniswap,
            factory=explicit_factory,
            fee_token0=Fraction(5, 1000),
            fee_token1=Fraction(5, 1000),
            init_hash="0x" + "ff" * 32,
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
        )
        assert pool.dex is uniswap
        # Explicit overrides win (factory is EIP-55 checksummed on store).
        assert pool.factory == get_checksum_address(explicit_factory)
        assert pool.fee_token0 == Fraction(5, 1000)
        assert pool.fee_token1 == Fraction(5, 1000)
        assert pool.init_hash == "0x" + "ff" * 32

    def test_dex_is_none_by_default(self) -> None:
        """Without dex, pool.dex is None (backward compat — existing callers)."""

        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)

        pool = make_v2_pool(
            "0x" + "cc" * 20,
            token0=token0,
            token1=token1,
            factory="0x" + "dd" * 20,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
        )
        assert pool.dex is None

    def test_pancakeswap_preset_carries_3_tuple_reserves_abi_via_dex(self) -> None:
        """PancakeSwap's preset (via dex) carries the 3-tuple reserves_abi shape."""

        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        pancake = dex_identity("pancakeswap-v2")
        assert pancake is not None

        pool = make_v2_pool(
            "0x" + "cc" * 20,
            token0=token0,
            token1=token1,
            dex=pancake,
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
        )
        assert pool.dex is pancake
        assert pool.dex.reserves_abi == ["uint112", "uint112", "uint32"]
        # Fee: preset (9975, 10000) → Fraction(25, 10000).
        assert pool.fee_token0 == Fraction(25, 10000)
