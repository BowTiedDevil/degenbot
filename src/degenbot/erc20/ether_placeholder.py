"""Wrapped Ether placeholder for native ETH in pool reserves."""

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20 import Erc20Token


class EtherPlaceholder(Erc20Token):
    """An Erc20Token-like adapter for the 'all Es' or zero address placeholder.

    Used by pools to represent native Ether. Under ADR-005, metadata
    (name="Ether Placeholder", symbol="ETH", decimals=18) is registered in the
    Rust ``Bot`` when an ``EtherPlaceholder`` is built; the inherited
    delegating properties read it back through the Rust ``Erc20Token`` handle.

    Direct construction is forbidden (inherited from :class:`Erc20Token`). Use
    :meth:`Erc20Token._from_py_token` — which produces an ``EtherPlaceholder``
    instance when called on the subclass — after registering the metadata in a
    ``Bot``.
    """

    addresses = (
        ZERO_ADDRESS,
        get_checksum_address("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE"),
    )
