from eth_typing import ChecksumAddress

from degenbot.cache import get_checksum_address
from degenbot.erc20_token import Erc20Token
from degenbot.logging import logger


class SimulationLedger:
    """
    A dictionary-like class for tracking token balances across addresses.

    Token balances are organized first by the holding address, then by the
    token contract address.
    """

    def __init__(self) -> None:
        # Entries are recorded as a dict-of-dicts, keyed by address, then by
        # token address
        self.balances: dict[
            ChecksumAddress,  # address holding balance
            dict[
                ChecksumAddress,  # token address
                int,  # balance
            ],
        ] = {}

    def adjust(
        self,
        address: ChecksumAddress | str,
        token: Erc20Token | ChecksumAddress | str,
        amount: int,
    ) -> None:
        """
        Apply an adjustment to the balance for a token held by an address.

        The amount can be positive (credit) or negative (debit). The method
        checksums all addresses prior to use.

        Parameters
        ----------
        address: str | ChecksumAddress
            The address holding the token balance.
        token: Erc20Token | str | ChecksumAddress
            The token being held. May be passed as an address or an ``Erc20Token``
        amount: int
            The amount to adjust. May be negative or positive.

        Returns
        -------
        None

        Raises
        ------
        ValueError
            If inputs did not match the expected types.
        """

        _token_address: ChecksumAddress
        if isinstance(token, Erc20Token):
            _token_address = token.address
        else:
            _token_address = get_checksum_address(token)

        _address = get_checksum_address(address)

        address_balance: dict[ChecksumAddress, int]
        try:
            address_balance = self.balances[_address]
        except KeyError:
            address_balance = {}
            self.balances[_address] = address_balance

        logger.debug(f"BALANCE: {_address} {'+' if amount > 0 else ''}{amount} {_token_address}")

        try:
            address_balance[_token_address]
        except KeyError:
            address_balance[_token_address] = 0
        finally:
            address_balance[_token_address] += amount
            if address_balance[_token_address] == 0:
                del address_balance[_token_address]
            if not address_balance:
                del self.balances[_address]

    def token_balance(
        self,
        address: ChecksumAddress | str,
        token: Erc20Token | ChecksumAddress | str,
    ) -> int:
        """
        Get the balance for a given address and token.

        The method checksums all addresses prior to use.

        Parameters
        ----------
        address: str | ChecksumAddress
            The address holding the token balance.
        token: Erc20Token | str | ChecksumAddress
            The token being held. May be passed as an address or an ``Erc20Token``

        Returns
        -------
        int
            The balance of ``token`` at ``address``

        Raises
        ------
        ValueError
            If inputs did not match the expected types.
        """

        _address = get_checksum_address(address)

        if isinstance(token, Erc20Token):
            _token_address = token.address
        else:
            _token_address = get_checksum_address(token)

        address_balances: dict[ChecksumAddress, int]
        try:
            address_balances = self.balances[_address]
        except KeyError:
            address_balances = {}

        return address_balances.get(_token_address, 0)

    def transfer(
        self,
        token: Erc20Token | ChecksumAddress | str,
        amount: int,
        from_addr: ChecksumAddress | str,
        to_addr: ChecksumAddress | str,
    ) -> None:
        """
        Transfer a balance between addresses.

        The method checksums all addresses prior to use.

        Parameters
        ----------
        token: Erc20Token | str | ChecksumAddress
            The token being held. May be passed as an address or an ``Erc20Token``
        amount: int
            The balance to transfer.
        from_addr: str | ChecksumAddress
            The address holding the token balance.
        to_addr: str | ChecksumAddress
            The address holding the token balance.

        Returns
        -------
        None

        Raises
        ------
        ValueError
            If inputs did not match the expected types.
        """

        if isinstance(token, Erc20Token):
            _token_address = token.address
        else:
            _token_address = get_checksum_address(token)

        self.adjust(
            address=from_addr,
            token=_token_address,
            amount=-amount,
        )
        self.adjust(
            address=to_addr,
            token=_token_address,
            amount=amount,
        )
