"""Account-query surface of the ``Bot`` facade (ADR-006 D5).

Split out of ``_bot.py`` as an inherited extension so the ``Bot`` class body
stays under the public-method complexity bar without trimming the facade:
the MRO puts these methods back on ``Bot`` unchanged, and every signature a
caller holds stays identical.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from degenbot._ffi import BotIo
    from degenbot.builders.erc20_builder import Erc20Builder
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.provider import AlloyProvider
    from degenbot.types.aliases import ChainId
    from degenbot.types.rpc_types import BlockIdentifier


class AccountQueryHost(Protocol):
    """Structural shape of ``Bot`` that the account queries reach through."""

    _erc20_builder: Erc20Builder
    _io: BotIo
    chain_id: ChainId
    provider: AlloyProvider


class AccountQueryMixin(AccountQueryHost):
    """ERC-20 and native-asset account queries surfaced on the ``Bot``.

    All reads flow through ``Erc20Builder`` on the session's single ``BotIo``
    seam, so the builder's Rust-side fetch + token cache write-through stays
    the one authority for balances, approvals, and total supply.
    """

    def get_token_balance(
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address.

        Returns:
            The computed integer value.

        """
        io = self._io
        return self._erc20_builder.get_token_balance(
            token,
            address,
            block_identifier=block_identifier,
            io=io,
        )

    def get_token_approval(
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`.

        Returns:
            The computed integer value.

        """
        io = self._io
        return self._erc20_builder.get_token_approval(
            token,
            owner,
            spender,
            block_identifier=block_identifier,
            io=io,
        )

    def get_token_total_supply(
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token.

        Returns:
            The computed integer value.

        """
        io = self._io
        return self._erc20_builder.get_token_total_supply(
            token,
            block_identifier=block_identifier,
            io=io,
        )

    def get_ether_balance(
        self,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address.

        Returns:
            The computed integer value.

        """
        io = self._io
        return self._erc20_builder.get_ether_balance(
            self.chain_id,
            address,
            block_identifier=block_identifier,
            io=io,
        )

    def get_provider(self) -> AlloyProvider:
        """Return this Bot's single provider.

        Returns:
            The computed value.

        """
        return self.provider
