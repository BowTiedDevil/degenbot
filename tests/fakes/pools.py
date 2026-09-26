"""Consolidated fake pool implementations.

Minimal protocol-fakes for testing:
- FakeV2Pool: captures external_update calls (test spy)
- FakeV3Pool: captures external_update and update_liquidity_map calls (test spy)
- FakeUniswapV4Pool: minimal V4 pool for registry tests
- FakeSessionPool: address-keyed companion with the family tag a registry reads

These fake pools are test spies, not mock math engines. For pool math testing,
use production pool classes (UniswapV2Pool, UniswapV3Pool, etc.) constructed
with FakeToken arguments.
"""

from dataclasses import dataclass

from degenbot.types.abstract import AbstractLiquidityPool


@dataclass(frozen=True)
class FakePoolHandle:
    """Stand-in for the live Rust ``Pool`` handle a companion wraps.

    The registries read a companion's registration family off its handle (the
    handle reports the family the core registered it under), so a fake pool
    needs one to be nameable in a session.
    """

    pool_family: str = "v2"


@dataclass(frozen=True)
class FakeSessionPool:
    """Minimal address-keyed pool companion carrying a family tag.

    Enough for a registry test that is about identity, not pool behavior: the
    registries only read ``address`` and the handle's family.
    """

    address: str
    pool_family: str = "v2"

    @property
    def _py_pool(self) -> FakePoolHandle:
        return FakePoolHandle(self.pool_family)


class FakeV2Pool:
    """Minimal fake V2 pool that captures external_update calls."""

    def __init__(self) -> None:
        self.last_update = None

    def external_update(self, update: object) -> None:
        self.last_update = update


class FakeV3Pool:
    """Minimal fake V3 pool that captures external_update and update_liquidity_map calls."""

    def __init__(self) -> None:
        self.last_update = None
        self.last_liquidity_update = None

    def external_update(self, update: object) -> None:
        self.last_update = update

    def update_liquidity_map(self, update: object) -> None:
        self.last_liquidity_update = update


class FakeUniswapV4Pool(AbstractLiquidityPool):
    """Minimal fake Uniswap V4 pool for registry tests."""

    def __init__(self, address: str, pool_id: str) -> None:
        self.address = address
        self.pool_id = pool_id
        self.name = f"FakeUniswapV4Pool-{address}"

    @property
    def tokens(self) -> tuple[object, object]:
        return (object(), object())

    def __eq__(self, other: object) -> bool:
        if isinstance(other, FakeUniswapV4Pool):
            return self.address == other.address and self.pool_id == other.pool_id
        return False

    def __hash__(self) -> int:
        return hash(self.address + self.pool_id)
