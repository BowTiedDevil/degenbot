from typing import Dict, Union

from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.logging import logger
from degenbot.token import Erc20Token


class SimulationLedger:
    """
    A dictionary-like class for tracking token balances across addresses.

    Token balances are organized first by the holding address, then by the
    token contract address.
    """

    def __init__(self):
        # Entries are recorded as a dict-of-dicts, keyed by address, then by
        # token address
        self._balances: Dict[
            ChecksumAddress,  # address holding balance
            Dict[
                ChecksumAddress,  # token address
                int,  # balance
            ],
        ] = dict()

    def adjust(
        self,
        address: Union[str, ChecksumAddress],
        token: Union[Erc20Token, str, ChecksumAddress],
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
        elif isinstance(token, str):
            _token_address = Web3.toChecksumAddress(token)
        elif isinstance(token, ChecksumAddress):
            _token_address = token
        else:
            raise ValueError(
                f"Token may be type Erc20Token, str, or ChecksumAddress. Was {type(token)}"
            )

        _address = Web3.toChecksumAddress(address)

        address_balance: Dict[ChecksumAddress, int]
        try:
            address_balance = self._balances[_address]
        except KeyError:
            address_balance = {}
            self._balances[_address] = address_balance

        logger.debug(
            f"BALANCE: {_address} {'+' if amount > 0 else ''}{amount} {_token_address}"
        )

        try:
            address_balance[_token_address]
        except KeyError:
            address_balance[_token_address] = 0
        finally:
            address_balance[_token_address] += amount
            if address_balance[_token_address] == 0:
                del address_balance[_token_address]
            if not address_balance:
                del self._balances[_address]

    def token_balance(
        self,
        address: Union[str, ChecksumAddress],
        token: Union[Erc20Token, str, ChecksumAddress],
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

        _address = Web3.toChecksumAddress(address)

        if isinstance(token, Erc20Token):
            _token_address = token.address
        elif isinstance(token, str):
            _token_address = Web3.toChecksumAddress(token)
        elif isinstance(token, ChecksumAddress):
            _token_address = token
        else:
            raise ValueError(
                f"Expected token type Erc20Token, str, or ChecksumAddress. Was {type(token)}"
            )

        address_balances: Dict[ChecksumAddress, int]
        try:
            address_balances = self._balances[_address]
        except KeyError:
            address_balances = {}

        return address_balances.get(_token_address, 0)

    def transfer(
        self,
        token: Union[Erc20Token, str, ChecksumAddress],
        amount: int,
        from_addr: Union[ChecksumAddress, str],
        to_addr: Union[ChecksumAddress, str],
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
        elif isinstance(token, str):
            _token_address = Web3.toChecksumAddress(token)
        elif isinstance(token, ChecksumAddress):
            _token_address = token
        else:
            raise ValueError(
                f"Expected token type Erc20Token, str, or ChecksumAddress. Was {type(token)}"
            )

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
