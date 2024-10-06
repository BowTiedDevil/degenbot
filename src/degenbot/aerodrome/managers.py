import contextlib
from typing import TYPE_CHECKING, Any, cast

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from web3 import Web3
from web3.types import BlockIdentifier

from degenbot.exceptions import AddressMismatch, DegenbotError, ManagerError, PoolNotAssociated
from degenbot.logging import logger
from degenbot.registry.all_pools import AllPools

from ..aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from ..config import get_web3
from ..functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from ..uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager
from .functions import generate_aerodrome_v3_pool_address


class AerodromeV2PoolManager(UniswapV2PoolManager):
    from .pools import AerodromeV2Pool as pool_creator

    def get_pair_from_factory(  # type: ignore[override]
        self,
        w3: Web3,
        token0: ChecksumAddress,
        token1: ChecksumAddress,
        stable: bool,
        block_identifier: BlockIdentifier | None = None,
    ) -> str:
        pool_address, *_ = raw_call(
            w3=w3,
            address=self._factory_address,
            calldata=encode_function_calldata(
                function_prototype="getPool(address,address,bool)",
                function_arguments=[token0, token1, stable],
            ),
            return_types=["address"],
            block_identifier=get_number_for_block_identifier(block_identifier),
        )
        return cast(str, pool_address)

    def get_pool_from_tokens(  # type: ignore[override]
        self,
        token_addresses: tuple[str, str],
        stable: bool,
        silent: bool = False,
        state_block: int | None = None,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> AerodromeV2Pool:
        """
        Get a pool by its token addresses and the stable bool
        """
        pool = self._build_pool(
            pool_address=to_checksum_address(
                self.get_pair_from_factory(
                    w3=get_web3(),
                    token0=to_checksum_address(token_addresses[0]),
                    token1=to_checksum_address(token_addresses[1]),
                    stable=stable,
                    block_identifier=None,
                )
            ),
            silent=silent,
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )
        assert isinstance(pool, AerodromeV2Pool)
        return pool


class AerodromeV3PoolManager(UniswapV3PoolManager):
    from .pools import AerodromeV3Pool as pool_creator

    IMPLEMENTATION_ADDRESS = to_checksum_address("0xeC8E5342B19977B4eF8892e02D8DAEcfa1315831")

    def _build_pool(
        self,
        pool_address: ChecksumAddress,
        silent: bool,
        state_block: int | None,
        pool_class_kwargs: dict[str, Any] | None,
    ) -> AerodromeV3Pool:
        with contextlib.suppress(KeyError):
            result = self._tracked_pools[pool_address]
            if TYPE_CHECKING:
                assert isinstance(result, AerodromeV3Pool)
            return result

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(
                f"Pool address {pool_address} not associated with factory {self._factory_address}"
            )

        # Check if the AllPools collection already has this pool
        if (known_pool_helper := AllPools(self._chain_id).get(pool_address)) is not None:
            if TYPE_CHECKING:
                assert isinstance(known_pool_helper, AerodromeV3Pool)
            if known_pool_helper.factory == self._factory_address:
                self._add_tracked_pool(known_pool_helper)
                return known_pool_helper
            else:
                self._untracked_pools.add(pool_address)
                raise PoolNotAssociated(f"Pool {pool_address} is not associated with this DEX")

        if pool_class_kwargs is None:
            pool_class_kwargs = dict()

        if self._snapshot is not None:
            pool_class_kwargs.update(
                {
                    "tick_bitmap": self._snapshot.get_tick_bitmap(pool_address),
                    "tick_data": self._snapshot.get_tick_data(pool_address),
                }
            )
        else:
            logger.info(f"Initializing pool without liquidity snapshot, {self._factory_address=}")
            logger.info(f"{self._snapshot=}")

        # The pool is unknown, so build and add it
        try:
            new_pool_helper = self.pool_creator(
                address=pool_address,
                silent=silent,
                state_block=state_block,
                **pool_class_kwargs,
            )
        except AddressMismatch:
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated from None
        except DegenbotError as exc:
            raise ManagerError(f"Could not build V3 pool {pool_address}: {exc}") from exc
        else:
            self._apply_pending_liquidity_updates(new_pool_helper)
            self._add_tracked_pool(new_pool_helper)
            return new_pool_helper

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        silent: bool = False,
        state_block: int | None = None,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> AerodromeV3Pool:
        return self._build_pool(
            pool_address=to_checksum_address(pool_address),
            silent=silent,
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )

    def get_pool_by_tokens_and_tick_spacing(
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
    ) -> AerodromeV3Pool:
        return self.get_pool(
            pool_address=generate_aerodrome_v3_pool_address(
                deployer_address=self._deployer_address,
                token_addresses=sorted(token_addresses),
                implementation_address=self.IMPLEMENTATION_ADDRESS,
                tick_spacing=tick_spacing,
            ),
            silent=silent,
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )
