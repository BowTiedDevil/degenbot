from typing import Any

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from web3 import Web3
from web3.types import BlockIdentifier

from ..aerodrome.pools import AerodromeV2Pool
from ..config import connection_manager
from ..functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from ..uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager
from .functions import generate_aerodrome_v3_pool_address


class AerodromeV2PoolManager(UniswapV2PoolManager):
    from .pools import AerodromeV2Pool as Pool

    def get_pool_address_from_factory_contract(
        self,
        w3: Web3,
        token0: ChecksumAddress,
        token1: ChecksumAddress,
        stable: bool,
        block_identifier: BlockIdentifier | None = None,
    ) -> ChecksumAddress:
        pool_address, *_ = raw_call(
            w3=w3,
            address=self._factory_address,
            calldata=encode_function_calldata(
                function_prototype="getPool(address,address,bool)",
                function_arguments=[token0, token1, stable],
            ),
            return_types=["address"],
            block_identifier=get_number_for_block_identifier(block_identifier, w3),
        )
        return to_checksum_address(pool_address)

    def get_pool_from_tokens(  # type: ignore[override]
        self,
        token_addresses: tuple[str, str],
        stable: bool,
        silent: bool = False,
        state_block: int | None = None,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
        """
        Get a pool by its token addresses and the stable bool. The token addresses may be passed in
        any order.
        """

        token0, token1 = sorted([token_address.lower() for token_address in token_addresses])

        pool_address = self.get_pool_address_from_factory_contract(
            w3=connection_manager.get_web3(self.chain_id),
            token0=to_checksum_address(token0),
            token1=to_checksum_address(token1),
            stable=stable,
            block_identifier=None,
        )

        pool = self.get_pool(
            pool_address=pool_address,
            silent=silent,
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )
        assert isinstance(pool, AerodromeV2Pool)
        return pool


class AerodromeV3PoolManager(UniswapV3PoolManager):
    from .pools import AerodromeV3Pool as Pool

    IMPLEMENTATION_ADDRESS = to_checksum_address("0xeC8E5342B19977B4eF8892e02D8DAEcfa1315831")

    def get_pool_from_tokens_and_tick_spacing(
        self,
        token_addresses: tuple[
            ChecksumAddress | str,
            ChecksumAddress | str,
        ],
        tick_spacing: int,
        silent: bool = False,
        state_block: int | None = None,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
        pool_address = generate_aerodrome_v3_pool_address(
            deployer_address=self._deployer_address,
            token_addresses=sorted(token_addresses),
            implementation_address=self.IMPLEMENTATION_ADDRESS,
            tick_spacing=tick_spacing,
        )

        pool = self.get_pool(
            pool_address=pool_address,
            silent=silent,
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )

        assert isinstance(pool, self.Pool)
        return pool
