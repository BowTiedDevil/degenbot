"""Coin and balance enumeration for Curve pools.

Discovers token addresses and balances by iterating the pool's coins() and
balances() methods, trying both uint256 and int128 index prototypes.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from web3.exceptions import Web3Exception

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.detection.types import CoinDiscoveryResult
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.builders.pool_io import PoolIO


def discover_coins(
    io: PoolIO,
    pool_address: ChecksumAddress,
    *,
    block_identifier: int,
    max_coins: int = 8,
) -> CoinDiscoveryResult:
    """Enumerate coins and balances for a Curve pool.

    Tries coins(uint256) first, falls back to coins(int128).
    Stops at first zero address or revert.
    """
    token_addresses: list[ChecksumAddress] = []
    balances: list[int] = []

    coin_prototype: str | None = None
    balance_prototype: str | None = None

    for i in range(max_coins):
        if coin_prototype is None:
            # Try uint256 first
            try:
                coin_addr = io.call_raw(
                    {
                        "to": pool_address,
                        "data": encode_function_calldata(
                            function_prototype="coins(uint256)",
                            function_arguments=[i],
                        ),
                    },
                    block=block_identifier,
                )
                (token_address,) = eth_abi.abi.decode(types=["address"], data=coin_addr)
                if int(token_address, 16) != 0:
                    coin_prototype = "coins(uint256)"
                    balance_prototype = "balances(uint256)"
                    token_address = get_checksum_address(token_address)
            except (Web3Exception, DecodingError, ValueError):
                pass

            # Try int128 if uint256 failed
            if coin_prototype is None:
                try:
                    coin_addr = io.call_raw(
                        {
                            "to": pool_address,
                            "data": encode_function_calldata(
                                function_prototype="coins(int128)",
                                function_arguments=[i],
                            ),
                        },
                        block=block_identifier,
                    )
                    (token_address,) = eth_abi.abi.decode(types=["address"], data=coin_addr)
                    if int(token_address, 16) != 0:
                        coin_prototype = "coins(int128)"
                        balance_prototype = "balances(int128)"
                        token_address = get_checksum_address(token_address)
                except (Web3Exception, DecodingError, ValueError):
                    pass

            if coin_prototype is None:
                # Neither worked, bail out
                break

            # We found the prototype, now decode the address we already fetched
            if int(token_address, 16) == 0:
                break
            token_addresses.append(token_address)
        else:
            # Use the known prototype (narrowed to str by the else branch)
            assert coin_prototype is not None
            assert balance_prototype is not None
            try:
                coin_addr = io.call_raw(
                    {
                        "to": pool_address,
                        "data": encode_function_calldata(
                            function_prototype=coin_prototype,
                            function_arguments=[i],
                        ),
                    },
                    block=block_identifier,
                )
                (token_address,) = eth_abi.abi.decode(types=["address"], data=coin_addr)
                if int(token_address, 16) == 0:
                    break
                token_address = get_checksum_address(token_address)
                token_addresses.append(token_address)
            except (Web3Exception, DecodingError, ValueError):
                break

        # Fetch balance
        assert balance_prototype is not None
        try:
            balance_result = io.call_raw(
                {
                    "to": pool_address,
                    "data": encode_function_calldata(
                        function_prototype=balance_prototype,
                        function_arguments=[i],
                    ),
                },
                block=block_identifier,
            )
            (balance,) = eth_abi.abi.decode(types=["uint256"], data=balance_result)
            balances.append(balance)
        except (Web3Exception, DecodingError, ValueError):
            break

    return CoinDiscoveryResult(
        token_addresses=tuple(token_addresses),
        balances=tuple(balances),
        coin_prototype=coin_prototype or "",
        balance_prototype=balance_prototype or "",
    )
