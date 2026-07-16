"""Companion-layer re-exports of Rust price-oracle FFI pyclasses.

The three-layer architecture (ADR-005) requires that driver / test code import
price-oracle pyclasses from the companion package — not from the PyO3 wrapper
``degenbot._ffi``. The ``Py*`` prefix is the FFI seam naming itself; it should
never appear in driver code.

These re-exports must be **direct aliases** (``from degenbot._ffi import PyX
as X``), not Python subclasses — the Rust engine constructs and consumes these
pyclasses directly; subclassing would break type identity at the FFI boundary.
"""

from __future__ import annotations

from degenbot._ffi.price import PyAavePriceOracle, PyChainlinkPriceFeed


def test_chainlink_price_feed_alias_identity() -> None:
    """The stable companion name is the exact FFI pyclass."""
    from degenbot.chainlink import ChainlinkPriceFeed

    assert ChainlinkPriceFeed is PyChainlinkPriceFeed


def test_aave_price_oracle_alias_identity() -> None:
    """The stable companion name is the exact FFI pyclass."""
    from degenbot.aave import AavePriceOracle

    assert AavePriceOracle is PyAavePriceOracle
