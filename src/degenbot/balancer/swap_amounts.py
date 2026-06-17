"""Balancer V2 swap amount types with Vault ABI encoding."""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

import eth_abi.abi
from web3 import Web3

from degenbot.arbitrage.encoding import EncodedCall
from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import DegenbotValueError

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

_POOL_ID_LENGTH = 32
_GIVEN_IN_KIND = 0
_MAX_UINT256 = 2**256 - 1


@dataclasses.dataclass(slots=True, frozen=True)
class BalancerV2SwapAmounts:
    """Swap amounts for a Balancer V2 Vault swap.

    Encodes a call to Vault.swap() with:
    - SingleSwap: (poolId, kind, assetIn, assetOut, amount, userData)
    - FundManagement: (sender, fromInternalBalance, recipient, toInternalBalance)
    - limit: minimum output amount
    - deadline: max uint256
    """

    pool_id: bytes
    vault: ChecksumAddress
    zero_for_one: bool
    amount_in: int
    amount_out: int
    token_in: ChecksumAddress
    token_out: ChecksumAddress

    def __post_init__(self) -> None:
        """Post-initialization hook.

        Raises:
            DegenbotValueError: See function documentation.

        """
        if isinstance(self.pool_id, (bytes, bytearray)) and len(self.pool_id) != _POOL_ID_LENGTH:
            msg = f"pool_id must be {_POOL_ID_LENGTH} bytes, got {len(self.pool_id)}"
            raise DegenbotValueError(message=msg)

    def input_amount(self) -> int:
        """Return input amount.

        Returns:
            The computed integer value.

        """
        return self.amount_in

    def output_amount(self) -> int:
        """Return output amount.

        Returns:
            The computed integer value.

        """
        return self.amount_out

    def encode(self, *, recipient: ChecksumAddress | None = None) -> EncodedCall:
        """Encode Vault.swap() call.

        FundManagement defaults:
        - sender: ZERO_ADDRESS (filled by executor at runtime)
        - fromInternalBalance: False
        - recipient: the ``recipient`` parameter (must be provided)
        - toInternalBalance: False

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: See function documentation.

        """
        if recipient is None:
            msg = "recipient is required for Balancer V2 swap encoding"
            raise DegenbotValueError(message=msg)

        # Swap kind: 0 = GIVEN_IN
        swap_signature = (
            "swap((bytes32,uint8,address,address,uint256,bytes),"
            "(address,bool,address,bool),uint256,uint256)"
        )
        selector = Web3.keccak(text=swap_signature)[:4]
        single_swap_type = "(bytes32,uint8,address,address,uint256,bytes)"
        fund_management_type = "(address,bool,address,bool)"
        data = selector + eth_abi.abi.encode(
            types=[single_swap_type, fund_management_type, "uint256", "uint256"],
            args=[
                (
                    self.pool_id,
                    _GIVEN_IN_KIND,
                    self.token_in,
                    self.token_out,
                    self.amount_in,
                    b"",
                ),  # SingleSwap
                (ZERO_ADDRESS, False, recipient, False),  # FundManagement
                self.amount_out,  # limit (minimum output)
                _MAX_UINT256,  # deadline
            ],
        )
        return EncodedCall(to=self.vault, data=data)
