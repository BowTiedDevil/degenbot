"""Wrapped Ether placeholder for native ETH in pool reserves."""

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.degenbot_rs import PyErc20Token
from degenbot.erc20 import Erc20Token


class EtherPlaceholder(Erc20Token):
    """An Erc20Token-like adapter for the 'all Es' or zero address placeholder.

    Used by pools to represent native Ether. Under ADR-005, metadata
    (name="Ether Placeholder", symbol="ETH", decimals=18) is registered in the
    Rust ``Bot`` when an ``EtherPlaceholder`` is built; the inherited
    delegating properties read it back through the ``PyErc20Token`` handle.
    """

    addresses = (
        ZERO_ADDRESS,
        get_checksum_address("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE"),
    )

    def __init__(
        self,
        py_token: PyErc20Token,
        *,
        state_cache_depth: int = 8,
    ) -> None:
        """Initialize the instance."""
        super().__init__(py_token, state_cache_depth=state_cache_depth)
        self._state_cache_depth = state_cache_depth
