"""Stub for the dynamically-created ``degenbot._ffi.price`` submodule.

Created at runtime by ``add_price_module`` in the PyO3 wrapper crate
(``degenbot-python/src/price/mod.rs``). The classes are thin PyO3
wrappers over the pure-Rust ``degenbot-price`` core crate.
"""

from degenbot._ffi import AlloyProvider

class PyChainlinkPriceFeed:
    """Typed Chainlink aggregator price-feed reader."""

    def __init__(
        self,
        address: str,
        provider: AlloyProvider,
        chain_id: int | None = None,
    ) -> None: ...
    @property
    def address(self) -> str: ...
    @property
    def chain_id(self) -> int | None: ...
    def decimals(self) -> int: ...
    def latest_round_data(self) -> tuple[int, int, int, int, int]: ...
    def price(self) -> int: ...

class PyAavePriceOracle:
    """Typed Aave price-oracle reader."""

    def __init__(self, oracle_address: str, provider: AlloyProvider) -> None: ...
    @property
    def address(self) -> str: ...
    def get_asset_price(self, asset_address: str) -> int: ...
    def fetch(self, asset_addresses: list[str]) -> dict[str, int]: ...

__all__ = ["PyAavePriceOracle", "PyChainlinkPriceFeed"]
