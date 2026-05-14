"""Curve fetcher factory.

Creates fetcher closures for Curve StableSwap pools. Each fetcher captures
chain_id and optionally pool_address at creation, then uses ProviderAdapter
to perform I/O when called.

This factory is created by Bot (or the CurvePoolBuilder) and its methods
are called during pool construction. The resulting closures are injected
into the I/O-free CurveStableswapPool.
"""

from __future__ import annotations

# ruff: noqa: ANN401, N802
from typing import TYPE_CHECKING, Any

import eth_abi.abi
from hexbytes import HexBytes
from web3 import Web3

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
            provider = self._connections.get_provider(chain_id)
            (vp,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call_raw(
                    {"to": target_address, "data": Web3.keccak(text="get_virtual_price()")[:4]},
                    block=block_number,
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
            provider = self._connections.get_provider(chain_id)
            (vp,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call_raw(
                    {"to": pool_address, "data": Web3.keccak(text="base_virtual_price()")[:4]},
                    block=block_number,
                ),
            )
            return vp

        return fetcher

    def base_cache_updated_fetcher(
        self, pool_address: ChecksumAddress
    ) -> Any:
        """Create a base_cache_updated fetcher closure for a Curve metapool.

        Calls base_cache_updated() on the metapool contract, which returns the
        timestamp when the base pool virtual price cache was last updated.
        """
        chain_id = self._chain_id

        def fetcher(block_number: int) -> int:
            provider = self._connections.get_provider(chain_id)
            (bcu,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call_raw(
                    {"to": pool_address, "data": Web3.keccak(text="base_cache_updated()")[:4]},
                    block=block_number,
                ),
            )
            return bcu

        return fetcher

    def timestamp_fetcher(self) -> Any:
        """Create a timestamp fetcher closure for a Curve pool."""
        chain_id = self._chain_id

        def fetcher(block_number: int) -> int:
            provider = self._connections.get_provider(chain_id)
            return provider.get_block_timestamp(block=block_number)

        return fetcher

    def redemption_price_fetcher(
        self, pool_address: ChecksumAddress
    ) -> Any:
        """Create a redemption price fetcher closure for a Curve pool."""
        chain_id = self._chain_id
        redemption_price_scale = 10**9

        def fetcher(block_number: int) -> int:
            provider = self._connections.get_provider(chain_id)

            (snap_contract_address,) = eth_abi.abi.decode(
                types=["address"],
                data=provider.call_raw(
                    {"to": pool_address, "data": Web3.keccak(text="redemption_price_snap()")[:4]},
                    block=block_number,
                ),
            )

            (rate,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call_raw(
                    {
                        "to": get_checksum_address(snap_contract_address),
                        "data": Web3.keccak(text="snappedRedemptionPrice()")[:4],
                    },
                    block=block_number,
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

    def ctoken_rate_fetcher(
        self,
        pool_address: ChecksumAddress,
        tokens: list[Any],
        use_lending: list[bool],
        precision_multipliers: list[int],
    ) -> Any:
        """Create a cToken rate fetcher closure for a Curve lending pool.

        Fetches exchangeRateStored() + supplyRatePerBlock() + accrualBlockNumber()
        for each lending token, then computes the supply-adjusted rate.
        """
        chain_id = self._chain_id
        PRECISION = 10**18
        cache: dict[int, tuple[int, ...]] = {}

        def fetcher(block_number: int) -> tuple[int, ...]:
            try:
                return cache[block_number]
            except KeyError:
                pass

            provider = self._connections.get_provider(chain_id)
            result: list[int] = []
            rate: int
            for token, is_lending, multiplier in zip(
                tokens, use_lending, precision_multipliers, strict=True
            ):
                if not is_lending:
                    rate = PRECISION
                else:
                    (rate,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=provider.call_raw(
                            {
                                "to": token.address,
                                "data": Web3.keccak(text="exchangeRateStored()")[:4],
                            },
                            block=block_number,
                        ),
                    )
                    supply_rate: int
                    (supply_rate,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=provider.call_raw(
                            {
                                "to": token.address,
                                "data": Web3.keccak(text="supplyRatePerBlock()")[:4],
                            },
                            block=block_number,
                        ),
                    )
                    old_block: int
                    (old_block,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=provider.call_raw(
                            {
                                "to": token.address,
                                "data": Web3.keccak(text="accrualBlockNumber()")[:4],
                            },
                            block=block_number,
                        ),
                    )

                    rate += rate * supply_rate * (block_number - old_block) // PRECISION

                result.append(multiplier * rate)

            cache[block_number] = tuple(result)
            return tuple(result)

        return fetcher

    def ytoken_rate_fetcher(
        self,
        pool_address: ChecksumAddress,
        tokens: list[Any],
        use_lending: list[bool],
        precision_multipliers: list[int],
    ) -> Any:
        """Create a yToken rate fetcher closure for a Curve lending pool.

        Fetches getPricePerFullShare() for each lending token.
        """
        chain_id = self._chain_id
        LENDING_PRECISION = 10**18
        cache: dict[int, tuple[int, ...]] = {}

        def fetcher(block_number: int) -> tuple[int, ...]:
            try:
                return cache[block_number]
            except KeyError:
                pass

            provider = self._connections.get_provider(chain_id)
            result: list[int] = []
            for token, multiplier, is_lending in zip(
                tokens, precision_multipliers, use_lending, strict=True
            ):
                if is_lending:
                    rate: int
                    (rate,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=provider.call_raw(
                            {
                                "to": token.address,
                                "data": Web3.keccak(text="getPricePerFullShare()")[
                                    :4
                                ],
                            },
                            block=block_number,
                        ),
                    )
                else:
                    rate = LENDING_PRECISION

                result.append(rate * multiplier)

            cache[block_number] = tuple(result)
            return tuple(result)

        return fetcher

    def cytoken_rate_fetcher(
        self,
        pool_address: ChecksumAddress,
        tokens: list[Any],
        use_lending: list[bool],
        precision_multipliers: list[int],
    ) -> Any:
        """Create a cyToken rate fetcher closure for a Curve lending pool.

        Similar to cToken rate fetcher but all tokens are treated as lending tokens.
        Fetches exchangeRateStored() + supplyRatePerBlock() + accrualBlockNumber().
        """
        chain_id = self._chain_id
        PRECISION = 10**18
        cache: dict[int, tuple[int, ...]] = {}

        def fetcher(block_number: int) -> tuple[int, ...]:
            try:
                return cache[block_number]
            except KeyError:
                pass

            provider = self._connections.get_provider(chain_id)
            result: list[int] = []
            for token, precision_multiplier in zip(
                tokens, precision_multipliers, strict=True
            ):
                rate: int
                (rate,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=provider.call_raw(
                        {
                            "to": token.address,
                            "data": Web3.keccak(text="exchangeRateStored()")[:4],
                        },
                        block=block_number,
                    ),
                )
                supply_rate: int
                (supply_rate,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=provider.call_raw(
                        {
                            "to": token.address,
                            "data": Web3.keccak(text="supplyRatePerBlock()")[:4],
                        },
                        block=block_number,
                    ),
                )
                old_block: int
                (old_block,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=provider.call_raw(
                        {
                            "to": token.address,
                            "data": Web3.keccak(text="accrualBlockNumber()")[:4],
                        },
                        block=block_number,
                    ),
                )

                rate += rate * supply_rate * (block_number - old_block) // PRECISION
                result.append(precision_multiplier * rate)

            cache[block_number] = tuple(result)
            return tuple(result)

        return fetcher

    def reth_rate_fetcher(
        self,
        pool_address: ChecksumAddress,
        tokens: list[Any],
        use_lending: list[bool],
        precision_multipliers: list[int],
    ) -> Any:
        """Create a rETH rate fetcher closure for a Curve lending pool.

        Fetches getExchangeRate() on the second token (rETH).
        """
        chain_id = self._chain_id
        PRECISION = 10**18
        cache: dict[int, tuple[int, ...]] = {}

        def fetcher(block_number: int) -> tuple[int, ...]:
            try:
                return cache[block_number]
            except KeyError:
                pass

            provider = self._connections.get_provider(chain_id)
            ratio: int
            (ratio,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call_raw(
                    {
                        "to": tokens[1].address,
                        "data": Web3.keccak(text="getExchangeRate()")[:4],
                    },
                    block=block_number,
                ),
            )
            result = (PRECISION, ratio)
            cache[block_number] = result
            return result

        return fetcher

    def aeth_rate_fetcher(
        self,
        pool_address: ChecksumAddress,
        tokens: list[Any],
        use_lending: list[bool],
        precision_multipliers: list[int],
    ) -> Any:
        """Create an aETH rate fetcher closure for a Curve lending pool.

        Fetches ratio() on the second token (ankrETH), then inverts it.
        The aETH rate is computed as PRECISION * LENDING_PRECISION // ratio.
        """
        chain_id = self._chain_id
        PRECISION = 10**18
        LENDING_PRECISION = 10**18
        cache: dict[int, tuple[int, ...]] = {}

        def fetcher(block_number: int) -> tuple[int, ...]:
            try:
                return cache[block_number]
            except KeyError:
                pass

            provider = self._connections.get_provider(chain_id)
            ratio: int
            (ratio,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call_raw(
                    {
                        "to": tokens[1].address,
                        "data": Web3.keccak(text="ratio()")[:4],
                    },
                    block=block_number,
                ),
            )
            result = (PRECISION, PRECISION * LENDING_PRECISION // ratio)
            cache[block_number] = result
            return result

        return fetcher

    def oracle_rate_fetcher(
        self,
        pool_address: ChecksumAddress,
        tokens: list[Any],
        use_lending: list[bool],
        precision_multipliers: list[int],
        rate_multipliers: tuple[int, ...],
    ) -> Any:
        """Create an oracle rate fetcher closure for a Curve lending pool.

        Two-step fetch: first detects oracle_method via oracle_method() call,
        then uses the method bitmask to fetch the actual rate.
        """
        chain_id = self._chain_id
        PRECISION = 10**18
        cache: dict[int, tuple[int, ...]] = {}
        oracle_method_cache: list[int | None] = [None]  # mutable list closure trick

        def fetcher(block_number: int) -> tuple[int, ...]:
            try:
                return cache[block_number]
            except KeyError:
                pass

            provider = self._connections.get_provider(chain_id)

            # Lazy-once detection of oracle method
            if oracle_method_cache[0] is None:
                (oracle_method_value,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=provider.call_raw(
                        {
                            "to": pool_address,
                            "data": Web3.keccak(text="oracle_method()")[:4],
                        },
                        block=block_number,
                    ),
                )
                oracle_method_cache[0] = oracle_method_value

            oracle_method = oracle_method_cache[0]

            if oracle_method == 0:
                rates = rate_multipliers
            else:
                oracle_bit_mask = (2**32 - 1) * 256**28
                oracle_rate: int
                (oracle_rate,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=provider.call_raw(
                        {
                            "to": get_checksum_address(
                                HexBytes(oracle_method % 2**160)
                            ),
                            "data": HexBytes(oracle_method & oracle_bit_mask),
                        },
                        block=block_number,
                    ),
                )
                rates = (
                    rate_multipliers[0],
                    rate_multipliers[1] * oracle_rate // PRECISION,
                )

            cache[block_number] = rates
            return rates

        return fetcher

    def lending_rate_fetcher(
        self,
        pool_address: ChecksumAddress,
        tokens: list[Any],
        use_lending: list[bool],
        precision_multipliers: list[int],
        rate_multipliers: tuple[int, ...] | None = None,
        lending_rate_style: Any = None,
    ) -> Any:
        """Create the appropriate lending rate fetcher based on the style enum.

        Returns None for pools without lending tokens.
        """
        from degenbot.curve.types import LendingRateStyle

        if lending_rate_style is None or lending_rate_style == LendingRateStyle.NONE:
            return None

        match lending_rate_style:
            case LendingRateStyle.CTOKEN:
                return self.ctoken_rate_fetcher(
                    pool_address, tokens, use_lending, precision_multipliers
                )
            case LendingRateStyle.YTOKEN:
                return self.ytoken_rate_fetcher(
                    pool_address, tokens, use_lending, precision_multipliers
                )
            case LendingRateStyle.CYTOKEN:
                return self.cytoken_rate_fetcher(
                    pool_address, tokens, use_lending, precision_multipliers
                )
            case LendingRateStyle.AETH:
                return self.aeth_rate_fetcher(
                    pool_address, tokens, use_lending, precision_multipliers
                )
            case LendingRateStyle.RETH:
                return self.reth_rate_fetcher(
                    pool_address, tokens, use_lending, precision_multipliers
                )
            case LendingRateStyle.ORACLE:
                return self.oracle_rate_fetcher(
                    pool_address, tokens, use_lending, precision_multipliers, rate_multipliers
                )
            case _:
                return None

    def D_fetcher(self, pool_address: ChecksumAddress) -> Any:
        """Create a D() fetcher closure for a crypto Curve pool."""
        chain_id = self._chain_id

        def fetcher(block_number: int) -> int:
            provider = self._connections.get_provider(chain_id)
            (d,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call_raw(
                    {"to": pool_address, "data": Web3.keccak(text="D()")[:4]},
                    block=block_number,
                ),
            )
            return d

        return fetcher

    def gamma_fetcher(self, pool_address: ChecksumAddress) -> Any:
        """Create a gamma() fetcher closure for a crypto Curve pool."""
        chain_id = self._chain_id

        def fetcher(block_number: int) -> int:
            provider = self._connections.get_provider(chain_id)
            (gamma,) = eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call_raw(
                    {"to": pool_address, "data": Web3.keccak(text="gamma()")[:4]},
                    block=block_number,
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
            provider = self._connections.get_provider(chain_id)
            price_scale = [0] * (n_coins - 1)
            for token_index in range(n_coins - 1):
                (price_scale[token_index],) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=provider.call_raw(
                        {
                            "to": pool_address,
                            "data": Web3.keccak(text="price_scale(uint256)")[:4]
                            + eth_abi.abi.encode(types=["uint256"], args=[token_index]),
                        },
                        block=block_number,
                    ),
                )
            return tuple(price_scale)

        return fetcher
