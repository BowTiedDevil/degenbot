"""Curve fetcher factory.

Creates fetcher closures for Curve StableSwap pools. Each fetcher captures
chain_id and optionally pool_address at creation, then uses ConnectionManager
to perform I/O when called.

This factory is created by Bot (or the CurvePoolBuilder) and its methods
are called during pool construction. The resulting closures are injected
into the I/O-free CurveStableswapPool.
"""

from __future__ import annotations

# ruff: noqa: ANN401, N802
from typing import TYPE_CHECKING, Any

import eth_abi.abi
from web3 import Web3
from web3.types import TxParams

from degenbot.checksum_cache import get_checksum_address
from degenbot.functions import encode_function_calldata

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.connection.connection_manager import ConnectionManager
    from degenbot.types.aliases import ChainId


class CurveFetcherFactory:
    """
    Creates fetcher closures for Curve StableSwap pools.

    Each fetcher captures chain_id and optionally pool_address at creation,
    then uses the ConnectionManager to perform I/O when called.

    This factory is created by Bot (or the CurvePoolBuilder) and its
    methods are called during pool construction. The resulting closures
    are injected into the I/O-free CurveStableswapPool.
    """

    def __init__(self, *, connections: ConnectionManager, chain_id: ChainId) -> None:
        self._connections = connections
        self._chain_id = chain_id

    def virtual_price_fetcher(
        self,
        pool_address: ChecksumAddress,
        base_pool_address: ChecksumAddress | None = None,
    ) -> Any:
        """Create a virtual price fetcher closure for a Curve pool.

        For metapools, this calls get_virtual_price() on the base pool's contract.
        For non-metapools, this calls get_virtual_price() on the pool itself.
        """
        chain_id = self._chain_id
        target_address = base_pool_address if base_pool_address is not None else pool_address

        def fetcher(block_number: int) -> int:
            w3 = self._connections.get_web3(chain_id)
            (vp,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    TxParams(
                        to=target_address,
                        data=Web3.keccak(text="get_virtual_price()")[:4],
                    ),
                    block_identifier=block_number,
                ),
            )
            return vp

        return fetcher

    def base_virtual_price_fetcher(
        self, pool_address: ChecksumAddress
    ) -> Any:
        """Create a base virtual price fetcher closure for a Curve metapool.

        Calls base_virtual_price() on the metapool contract, which returns the
        virtual price of the base pool LP token.
        """
        chain_id = self._chain_id

        def fetcher(block_number: int) -> int:
            w3 = self._connections.get_web3(chain_id)
            (vp,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    TxParams(
                        to=pool_address,
                        data=Web3.keccak(text="base_virtual_price()")[:4],
                    ),
                    block_identifier=block_number,
                ),
            )
            return vp

        return fetcher

    def timestamp_fetcher(self) -> Any:
        """Create a timestamp fetcher closure for a Curve pool."""
        chain_id = self._chain_id

        def fetcher(block_number: int) -> int:
            w3 = self._connections.get_web3(chain_id)
            block = w3.eth.get_block(block_identifier=block_number)
            return block["timestamp"]

        return fetcher

    def redemption_price_fetcher(
        self, pool_address: ChecksumAddress
    ) -> Any:
        """Create a redemption price fetcher closure for a Curve pool."""
        chain_id = self._chain_id
        redemption_price_scale = 10**9

        def fetcher(block_number: int) -> int:
            w3 = self._connections.get_web3(chain_id)

            (snap_contract_address,) = eth_abi.abi.decode(
                types=["address"],
                data=w3.eth.call(
                    TxParams(
                        to=pool_address,
                        data=Web3.keccak(text="redemption_price_snap()")[:4],
                    ),
                    block_identifier=block_number,
                ),
            )

            (rate,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    TxParams(
                        to=get_checksum_address(snap_contract_address),
                        data=Web3.keccak(text="snappedRedemptionPrice()")[:4],
                    ),
                    block_identifier=block_number,
                ),
            )
            return rate // redemption_price_scale

        return fetcher

    def admin_balances_fetcher(
        self, pool_address: ChecksumAddress
    ) -> Any:
        """Create an admin balances fetcher closure for a Curve pool."""
        chain_id = self._chain_id

        def fetcher(block_number: int) -> tuple[int, ...]:
            provider = self._connections.get_provider(chain_id)
            admin_balances: list[int] = []
            for token_index in range(8):  # max 8 tokens for Curve V1
                try:
                    (admin_balance,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=provider.call(
                            to=pool_address,
                            data=encode_function_calldata(
                                function_prototype="admin_balances(uint256)",
                                function_arguments=[token_index],
                            ),
                            block=block_number,
                        ),
                    )
                    admin_balances.append(admin_balance)
                except Exception:  # noqa: BLE001
                    break
            return tuple(admin_balances)

        return fetcher

    def block_number_fetcher(self) -> Any:
        """Create a block number fetcher closure for a Curve pool."""
        chain_id = self._chain_id

        def fetcher() -> int:
            provider = self._connections.get_provider(chain_id)
            return provider.get_block_number()

        return fetcher

    def total_supply_fetcher(self) -> Any:
        """Create a total supply fetcher closure for a Curve pool.

        Calls totalSupply() directly via the provider rather than delegating
        to Bot.get_token_total_supply(). The Curve pool uses this for its
        internal calculations (computing D, virtual price) which need the
        supply at a specific block — caching on Erc20Token is per-address,
        not per-pool.
        """
        chain_id = self._chain_id

        def fetcher(token: Any, *, block_identifier: int | None = None) -> int:
            provider = self._connections.get_provider(chain_id)
            (total_supply,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call(
                    to=get_checksum_address(token.address),
                    data=encode_function_calldata("totalSupply()", None),
                    block=block_identifier,
                ),
            )
            return total_supply

        return fetcher

    def token_balance_fetcher(self) -> Any:
        """Create a token balance fetcher closure for a Curve pool.

        Calls balanceOf() directly via the provider rather than delegating
        to Bot.get_token_balance(). Same reasoning as total_supply_fetcher().
        """
        chain_id = self._chain_id

        def fetcher(
            token: Any, address: Any, *, block_identifier: int | None = None
        ) -> int:
            provider = self._connections.get_provider(chain_id)
            (balance,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call(
                    to=get_checksum_address(token.address),
                    data=encode_function_calldata(
                        "balanceOf(address)", [get_checksum_address(address)]
                    ),
                    block=block_identifier,
                ),
            )
            return balance

        return fetcher

    def provider_call(self) -> Any:
        """Create a raw provider.call() closure for a Curve pool.

        This is used by pool-type-specific rate fetching methods that need
        low-level contract calls (e.g. cToken exchangeRateStored, oracle_method, etc.).
        """
        chain_id = self._chain_id

        def fetcher(*, to: Any, data: Any, block: int) -> bytes:
            w3 = self._connections.get_web3(chain_id)
            return w3.eth.call(
                {"to": to, "data": data},
                block_identifier=block,
            )

        return fetcher

    def D_fetcher(self, pool_address: ChecksumAddress) -> Any:
        """Create a D() fetcher closure for a crypto Curve pool."""
        chain_id = self._chain_id

        def fetcher(block_number: int) -> int:
            w3 = self._connections.get_web3(chain_id)
            (d,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    TxParams(
                        to=pool_address,
                        data=Web3.keccak(text="D()")[:4],
                    ),
                    block_identifier=block_number,
                ),
            )
            return d

        return fetcher

    def gamma_fetcher(self, pool_address: ChecksumAddress) -> Any:
        """Create a gamma() fetcher closure for a crypto Curve pool."""
        chain_id = self._chain_id

        def fetcher(block_number: int) -> int:
            w3 = self._connections.get_web3(chain_id)
            (gamma,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    TxParams(
                        to=pool_address,
                        data=Web3.keccak(text="gamma()")[:4],
                    ),
                    block_identifier=block_number,
                ),
            )
            return gamma

        return fetcher

    def price_scale_fetcher(
        self, pool_address: ChecksumAddress, n_coins: int
    ) -> Any:
        """Create a price_scale() fetcher closure for a crypto Curve pool."""
        chain_id = self._chain_id

        def fetcher(block_number: int) -> tuple[int, ...]:
            w3 = self._connections.get_web3(chain_id)
            price_scale = [0] * (n_coins - 1)
            for token_index in range(n_coins - 1):
                (price_scale[token_index],) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        TxParams(
                            to=pool_address,
                            data=Web3.keccak(text="price_scale(uint256)")[:4]
                            + eth_abi.abi.encode(types=["uint256"], args=[token_index]),
                        ),
                        block_identifier=block_number,
                    ),
                )
            return tuple(price_scale)

        return fetcher
