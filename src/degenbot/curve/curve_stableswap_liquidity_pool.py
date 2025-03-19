# TODO
# ----------------------------------------------------
# PRIORITY      TASK
# high          write state_update method
# high          create a manager for Curve pools
# medium        add liquidity modifying mode for external_update
# medium        investigate differences in get_dy_underlying vs exchange_underlying at GUSD-3Crv
# low           investigate providing overrides for live-lookup contracts

import contextlib
from collections.abc import Iterable, Sequence
from threading import Lock
from typing import TYPE_CHECKING, Any, cast
from weakref import WeakSet

import eth_abi.abi
import web3.exceptions
from eth_abi.exceptions import DecodingError, InsufficientDataBytes
from eth_typing import AnyAddress, BlockNumber, ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import Web3Exception
from web3.types import BlockIdentifier

from degenbot.cache import get_checksum_address
from degenbot.config import connection_manager
from degenbot.constants import ZERO_ADDRESS
from degenbot.curve.deployments import (
    BROKEN_CURVE_V1_POOLS,
    CURVE_V1_FACTORY_ADDRESS,
    CURVE_V1_METAREGISTRY_ADDRESS,
    CURVE_V1_REGISTRY_ADDRESS,
)
from degenbot.curve.types import CurveStableswapPoolState, CurveStableSwapPoolStateUpdated
from degenbot.erc20_token import Erc20Token
from degenbot.exceptions import (
    BrokenPool,
    DegenbotValueError,
    EVMRevertError,
    InvalidSwapInputAmount,
    NoLiquidity,
)
from degenbot.functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from degenbot.logging import logger
from degenbot.managers.erc20_token_manager import Erc20TokenManager
from degenbot.registry.all_pools import pool_registry
from degenbot.types import (
    AbstractArbitrage,
    AbstractLiquidityPool,
    BoundedCache,
    Message,
    Publisher,
    PublisherMixin,
    Subscriber,
)


class CurveStableswapPool(PublisherMixin, AbstractLiquidityPool):
    type PoolState = CurveStableswapPoolState
    _state_cache: BoundedCache[BlockNumber, PoolState]

    # Constants from contract
    # ref: https://github.com/curvefi/curve-contract/blob/master/contracts/pool-templates/base/SwapTemplateBase.vy
    PRECISION_DECIMALS: int = 18
    PRECISION: int = 10**PRECISION_DECIMALS
    LENDING_PRECISION: int = PRECISION
    FEE_DENOMINATOR: int = 10**10
    A_PRECISION: int = 100
    MAX_COINS: int = 8

    D_VARIANT_GROUP_0 = frozenset(
        get_checksum_address(pool_address)
        for pool_address in (
            "0x06364f10B501e868329afBc005b3492902d6C763",
            "0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F",
            "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C",
            "0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714",
            "0x93054188d876f558f4a66B2EF1d97d16eDf0895B",
            "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        )
    )
    D_VARIANT_GROUP_1 = frozenset(
        get_checksum_address(pool_address)
        for pool_address in (
            "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
            "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
            "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56",
            "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD",
        )
    )
    D_VARIANT_GROUP_2 = frozenset(
        get_checksum_address(pool_address)
        for pool_address in (
            "0x0AD66FeC8dB84F8A3365ADA04aB23ce607ac6E24",
            "0x1c899dED01954d0959E034b62a728e7fEbE593b0",
            "0x3F1B0278A9ee595635B61817630cC19DE792f506",
            "0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a",
            "0x453D92C7d4263201C69aACfaf589Ed14202d83a4",
            "0x663aC72a1c3E1C4186CD3dCb184f216291F4878C",
            "0x6A274dE3e2462c7614702474D64d376729831dCa",
            "0x7C0d189E1FecB124487226dCbA3748bD758F98E4",
            "0x875DF0bA24ccD867f8217593ee27253280772A97",
            "0x9D0464996170c6B9e75eED71c68B99dDEDf279e8",
            "0xB657B895B265C38c53FFF00166cF7F6A3C70587d",
            "0xD6Ac1CB9019137a896343Da59dDE6d097F710538",
            "0xf7b55C3732aD8b2c2dA7c24f30A69f55c54FB717",
        )
    )
    D_VARIANT_GROUP_3 = frozenset(
        get_checksum_address(pool_address)
        for pool_address in (
            "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022",
            "0xDeBF20617708857ebe4F679508E7b7863a8A8EeE",
            "0xEB16Ae0052ed37f479f7fe63849198Df1765a733",
        )
    )
    D_VARIANT_GROUP_4 = frozenset(
        get_checksum_address(pool_address)
        for pool_address in (
            "0x1062FD8eD633c1f080754c19317cb3912810B5e5",
            "0x1C5F80b6B68A9E1Ef25926EeE00b5255791b996B",
            "0x26f3f26F46cBeE59d1F8860865e13Aa39e36A8c0",
            "0x2d600BbBcC3F1B6Cb9910A70BaB59eC9d5F81B9A",
            "0x320B564Fb9CF36933eC507a846ce230008631fd3",
            "0x3b21C2868B6028CfB38Ff86127eF22E68d16d53B",
            "0x69ACcb968B19a53790f43e57558F5E443A91aF22",
            "0x971add32Ea87f10bD192671630be3BE8A11b8623",
            "0xCA0253A98D16e9C1e3614caFDA19318EE69772D0",
            "0xfBB481A443382416357fA81F16dB5A725DC6ceC8",
        )
    )

    Y_VARIANT_GROUP_0 = frozenset(
        get_checksum_address(pool_address)
        for pool_address in (
            "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
            "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C",
            "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
            "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56",
            "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD",
        )
    )
    Y_VARIANT_GROUP_1 = frozenset(
        get_checksum_address(pool_address)
        for pool_address in (
            "0x06364f10B501e868329afBc005b3492902d6C763",
            "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
            "0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F",
            "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C",
            "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
            "0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714",
            "0x93054188d876f558f4a66B2EF1d97d16eDf0895B",
            "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56",
            "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD",
            "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        )
    )

    Y_D_VARIANT_GROUP_0 = frozenset(
        get_checksum_address(pool_address)
        for pool_address in (
            "0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2",
            "0xf253f83AcA21aAbD2A20553AE0BF7F65C755A07F",
        )
    )

    def _notify_subscribers(self: Publisher, message: Message) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)

    def __init__(
        self,
        address: ChecksumAddress | str,
        *,
        chain_id: int | None = None,
        state_block: BlockNumber | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> None:
        """
        A Curve V1 (StableSwap) pool.

        Arguments
        ---------
        address:
            Address for the deployed pool contract.
        chain_id:
            The chain ID where the pool contract is deployed.
        state_block:
            Fetch initial state values from the chain at a particular block height. Defaults to the
            latest block if omitted.
        silent:
            Suppress status output.
        state_cache_depth:
            How many unique block-state pairs to hold in the state cache.
        """

        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        w3 = connection_manager.get_web3(self.chain_id)
        if state_block is None:
            state_block = w3.eth.block_number

        self.fee_gamma: int
        self.mid_fee: int
        self.offpeg_fee_multiplier: int
        self.out_fee: int
        self.precision_multipliers: tuple[int, ...]
        self.rate_multipliers: tuple[int, ...]
        self.use_lending: tuple[bool, ...]
        self.oracle_method: int | None

        self.initial_a_coefficient: int | None = None
        self.initial_a_coefficient_time: int | None = None
        self.future_a_coefficient: int | None = None
        self.future_a_coefficient_time: int | None = None

        def get_a_scaling_values() -> None:
            with (
                contextlib.suppress(Web3Exception, DecodingError),
                w3.batch_requests() as batch,
            ):
                batch.add(
                    w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="initial_A()",
                                function_arguments=None,
                            ),
                        },
                        block_identifier=self.update_block,
                    )
                )
                batch.add(
                    w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="initial_A_time()",
                                function_arguments=None,
                            ),
                        },
                        block_identifier=self.update_block,
                    )
                )
                batch.add(
                    w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="future_A()",
                                function_arguments=None,
                            ),
                        },
                        block_identifier=self.update_block,
                    )
                )
                batch.add(
                    w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="future_A_time()",
                                function_arguments=None,
                            ),
                        },
                        block_identifier=self.update_block,
                    )
                )

                initial_a, initial_a_time, future_a, future_a_time = batch.execute()

                (initial_a,) = eth_abi.abi.decode(
                    types=["uint256"], data=cast("HexBytes", initial_a)
                )
                (initial_a_time,) = eth_abi.abi.decode(
                    types=["uint256"], data=cast("HexBytes", initial_a_time)
                )
                (future_a,) = eth_abi.abi.decode(types=["uint256"], data=cast("HexBytes", future_a))
                (future_a_time,) = eth_abi.abi.decode(
                    types=["uint256"], data=cast("HexBytes", future_a_time)
                )

                self.initial_a_coefficient = cast("int", initial_a)
                self.initial_a_coefficient_time = cast("int", initial_a_time)
                self.future_a_coefficient = cast("int", future_a)
                self.future_a_coefficient_time = cast("int", future_a_time)

        self.a_coefficient: int
        self.fee: int
        self.admin_fee: int

        def get_coefficient_and_fees() -> None:
            with w3.batch_requests() as batch:
                batch.add(
                    w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="A()",
                                function_arguments=None,
                            ),
                        },
                        block_identifier=self.update_block,
                    )
                )
                batch.add(
                    w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="fee()",
                                function_arguments=None,
                            ),
                        },
                        block_identifier=self.update_block,
                    )
                )
                batch.add(
                    w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="admin_fee()",
                                function_arguments=None,
                            ),
                        },
                        block_identifier=self.update_block,
                    )
                )

                a_coefficient, pool_fee, admin_fee = batch.execute()

                (a_coefficient,) = eth_abi.abi.decode(
                    types=["uint256"], data=cast("HexBytes", a_coefficient)
                )
                (pool_fee,) = eth_abi.abi.decode(types=["uint256"], data=cast("HexBytes", pool_fee))
                (admin_fee,) = eth_abi.abi.decode(
                    types=["uint256"], data=cast("HexBytes", admin_fee)
                )

                self.a_coefficient = cast("int", a_coefficient)
                self.fee = cast("int", pool_fee)
                self.admin_fee = cast("int", admin_fee)

        def get_coin_index_type() -> str:
            # Identify the coins input format (int128 or uint256)
            # Some contracts accept token_id as an int128, some accept uint256

            _type = "uint256"
            with contextlib.suppress(InsufficientDataBytes, web3.exceptions.ContractLogicError):
                eth_abi.abi.decode(
                    types=["address"],
                    data=w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": Web3.keccak(text=f"coins({_type})")[:4]
                            + eth_abi.abi.encode(types=[_type], args=[0]),
                        },
                        block_identifier=state_block,
                    ),
                )

                return _type

            _type = "int128"
            with contextlib.suppress(InsufficientDataBytes, web3.exceptions.ContractLogicError):
                eth_abi.abi.decode(
                    types=["address"],
                    data=w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": Web3.keccak(text=f"coins({_type})")[:4]
                            + eth_abi.abi.encode(types=[_type], args=[0]),
                        },
                        block_identifier=state_block,
                    ),
                )

                return _type

            raise DegenbotValueError(
                message="Could not determine input type for pool"
            )  # pragma: no cover

        def get_token_addresses() -> tuple[ChecksumAddress, ...]:
            token_addresses = []
            for token_id in range(self.MAX_COINS):  # pragma: no branch
                try:
                    token_address: str
                    (token_address,) = raw_call(
                        w3=w3,
                        address=self.address,
                        calldata=encode_function_calldata(
                            function_prototype=f"coins({self._coin_index_type})",
                            function_arguments=[token_id],
                        ),
                        return_types=["address"],
                        block_identifier=state_block,
                    )
                    token_addresses.append(get_checksum_address(token_address))
                except web3.exceptions.ContractLogicError:
                    break

            return tuple(token_addresses)

        def get_lp_token_address() -> ChecksumAddress:
            for contract_address in (
                CURVE_V1_METAREGISTRY_ADDRESS,
                CURVE_V1_REGISTRY_ADDRESS,
                CURVE_V1_FACTORY_ADDRESS,
            ):
                with contextlib.suppress(Web3Exception, DecodingError):
                    (lp_token_address,) = raw_call(
                        w3=connection_manager.get_web3(chain_id=self.chain_id),
                        address=contract_address,
                        calldata=encode_function_calldata(
                            function_prototype="get_lp_token(address)",
                            function_arguments=[self.address],
                        ),
                        return_types=["address"],
                        block_identifier=state_block,
                    )
                    if lp_token_address == ZERO_ADDRESS:
                        continue
                    return get_checksum_address(lp_token_address)

            raise DegenbotValueError(
                message=f"Could not identify LP token for pool {self.address}"
            )  # pragma: no cover

        def get_pool_from_lp_token(token: AnyAddress) -> ChecksumAddress:
            for contract_address in (
                CURVE_V1_METAREGISTRY_ADDRESS,
                CURVE_V1_REGISTRY_ADDRESS,
                CURVE_V1_FACTORY_ADDRESS,
            ):
                with contextlib.suppress(Web3Exception, DecodingError):
                    (pool_address,) = raw_call(
                        w3=connection_manager.get_web3(chain_id=self.chain_id),
                        address=contract_address,
                        calldata=encode_function_calldata(
                            function_prototype="get_pool_from_lp_token(address)",
                            function_arguments=[get_checksum_address(token)],
                        ),
                        return_types=["address"],
                        block_identifier=state_block,
                    )
                    return get_checksum_address(pool_address)

            raise DegenbotValueError(
                message=f"Could not identify base pool from LP token for {self.address}"
            )  # pragma: no cover

        def is_metapool() -> bool:
            """
            Check if the registry contract and the factory contract report that this is registered
            as a metapool. Some metapools are not correctly marked in one of the contracts, so this
            function checks both.
            """
            w3 = connection_manager.get_web3(chain_id=self.chain_id)

            is_meta_results = [False]

            for contract_address in (
                CURVE_V1_METAREGISTRY_ADDRESS,
                CURVE_V1_REGISTRY_ADDRESS,
                CURVE_V1_FACTORY_ADDRESS,
            ):
                with contextlib.suppress(Web3Exception, DecodingError):
                    (result,) = raw_call(
                        w3=w3,
                        address=contract_address,
                        calldata=encode_function_calldata(
                            function_prototype="is_meta(address)",
                            function_arguments=[self.address],
                        ),
                        return_types=["bool"],
                        block_identifier=state_block,
                    )
                    is_meta_results.append(cast("bool", result))

            return any(is_meta_results)

        def set_pool_specific_attributes() -> None:
            match self.address:
                case "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56":
                    self.use_lending = (True, True)
                    self.precision_multipliers = (1, 10**12)
                case "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5":
                    self.fee_gamma = 10000000000000000
                    self.mid_fee = 4000000
                    self.out_fee = 40000000
                case "0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2":
                    self.precision_multipliers = (1, 1000000000000)
                case "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C":
                    self.use_lending = (True, True, False)
                    self.precision_multipliers = (1, 10**12, 10**12)
                case "0x06364f10B501e868329afBc005b3492902d6C763":
                    self.use_lending = (True, True, True, False)
                case "0xDeBF20617708857ebe4F679508E7b7863a8A8EeE":
                    self.precision_multipliers = (1, 10**12, 10**12)
                    (self.offpeg_fee_multiplier,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=w3.eth.call(
                            transaction={
                                "to": self.address,
                                "data": Web3.keccak(text="offpeg_fee_multiplier()")[:4],
                            },
                            block_identifier=state_block,
                        ),
                    )
                case "0x2dded6Da1BF5DBdF597C45fcFaa3194e53EcfeAF":
                    self.precision_multipliers = (1, 10**12, 10**12)
                case "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27":
                    self.precision_multipliers = (1, 10**12, 10**12, 1)
                    self.use_lending = (True, True, True, True)
                case "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51":
                    self.precision_multipliers = (1, 10**12, 10**12, 1)
                    self.use_lending = (True, True, True, True)
                case "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD":
                    self.use_lending = (False, False, False, False)
                case (
                    "0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492"
                    | "0xBfAb6FA95E0091ed66058ad493189D2cB29385E6"
                ):
                    self._set_oracle_method(block_number=state_block)
                case "0xEB16Ae0052ed37f479f7fe63849198Df1765a733":
                    (self.offpeg_fee_multiplier,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=w3.eth.call(
                            transaction={
                                "to": self.address,
                                "data": Web3.keccak(text="offpeg_fee_multiplier()")[:4],
                            },
                            block_identifier=state_block,
                        ),
                    )

        self.address = get_checksum_address(address)
        if self.address in BROKEN_CURVE_V1_POOLS:
            raise BrokenPool

        _block = w3.eth.get_block(state_block)

        self._create_timestamp = _block["timestamp"]

        self._block_timestamps: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_admin_balances: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_base_cache_updated: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_base_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_contract_D: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_gamma: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_price_scale: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_rates_from_aeth: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_rates_from_ctokens: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_rates_from_cytokens: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_rates_from_oracle: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_rates_from_reth: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_rates_from_ytokens: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_scaled_redemption_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )

        # token setup
        self._coin_index_type = get_coin_index_type()

        token_manager = Erc20TokenManager(chain_id=self.chain_id)
        self.lp_token = token_manager.get_erc20token(
            address=get_lp_token_address(),
            silent=silent,
        )
        self.tokens: tuple[Erc20Token, ...] = tuple(
            token_manager.get_erc20token(
                address=token_address,
                silent=silent,
            )
            for token_address in get_token_addresses()
        )

        # metapool setup
        self.base_pool: CurveStableswapPool | None = None
        if is_metapool():
            # Curve metapools hold the LP token for the base pool at index 1
            base_pool_address = get_pool_from_lp_token(self.tokens[1].address)

            if (
                base_pool := pool_registry.get(
                    pool_address=base_pool_address, chain_id=self.chain_id
                )
            ) is None:
                base_pool = CurveStableswapPool(
                    base_pool_address, state_block=state_block, silent=silent
                )
            if TYPE_CHECKING:
                assert isinstance(base_pool, CurveStableswapPool)

            self.base_pool = base_pool
            self.tokens_underlying = (self.tokens[0], *self.base_pool.tokens)

            self.base_cache_updated: int | None = None
            with contextlib.suppress(web3.exceptions.ContractLogicError):
                self.base_cache_updated = self._get_base_cache_updated(block_number=state_block)

            self.base_virtual_price: int
            with contextlib.suppress(web3.exceptions.ContractLogicError):
                self.base_virtual_price = self._get_base_virtual_price(block_number=state_block)

        _balances = []
        for token_id, _ in enumerate(self.tokens):
            (token_balance,) = eth_abi.abi.decode(
                types=[self._coin_index_type],
                data=w3.eth.call(
                    transaction={
                        "to": self.address,
                        "data": Web3.keccak(text=f"balances({self._coin_index_type})")[:4]
                        + eth_abi.abi.encode(types=[self._coin_index_type], args=[token_id]),
                    },
                    block_identifier=state_block,
                ),
            )
            _balances.append(token_balance)

        """
        3pool example
        rate_multipliers = [
          10**12000000,             <------ 10**18 == 10**(18 + 18 - 18)
          10**12000000000000000000, <------ 10**30 == 10**(18 + 18 - 6)
          10**12000000000000000000, <------ 10**30 == 10**(18 + 18 - 6)
        ]
        """
        self.rate_multipliers = tuple(
            10 ** (2 * self.PRECISION_DECIMALS - token.decimals) for token in self.tokens
        )
        self.precision_multipliers = tuple(
            cast("int", 10 ** (self.PRECISION_DECIMALS - token.decimals)) for token in self.tokens
        )

        self._state = CurveStableswapPoolState(
            address=self.address,
            balances=tuple(_balances),
            block=state_block,
        )
        self._state_cache = BoundedCache(max_items=state_cache_depth)
        self._state_cache[state_block] = self._state
        self._state_lock = Lock()
        self._block_timestamps[state_block] = _block["timestamp"]

        get_a_scaling_values()
        get_coefficient_and_fees()
        set_pool_specific_attributes()

        fee_string = f"{100 * self.fee / self.FEE_DENOMINATOR:.2f}"
        token_string = "-".join([token.symbol for token in self.tokens])
        self.name = f"{token_string} ({self.__class__.__name__}, {fee_string}%)"

        self._subscribers: WeakSet[Subscriber] = WeakSet()

        pool_registry.add(pool_address=self.address, chain_id=self.chain_id, pool=self)

        if not silent:
            logger.info(
                f"{self.name} @ {self.address}, A={self.a_coefficient}, fee={100 * self.fee / self.FEE_DENOMINATOR:.2f}%"  # noqa:E501
            )
            for token_id, (token, balance) in enumerate(
                zip(self.tokens, self.balances, strict=True)
            ):
                logger.info(f"• Token {token_id}: {token} - Reserves: {balance}")

    def __getstate__(self) -> dict[str, Any]:
        # Remove objects that cannot be pickled and are unnecessary to perform
        # the calculation
        dropped_attributes = (
            "_state_lock",
            "_subscribers",
        )

        with self._state_lock:
            return {k: v for k, v in self.__dict__.items() if k not in dropped_attributes}

    def __repr__(self) -> str:  # pragma: no cover
        token_string = "-".join([token.symbol for token in self.tokens])
        return f"CurveStableswapPool(address={self.address}, tokens={token_string}, fee={100 * self.fee / self.FEE_DENOMINATOR:.2f}%, A={self.a_coefficient})"  # noqa:E501

    @property
    def balances(self) -> tuple[int, ...]:
        return self.state.balances

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def state(self) -> CurveStableswapPoolState:
        return self._state

    @property
    def update_block(self) -> BlockNumber:
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

    def _a(self, timestamp: int | None = None) -> int:
        """
        Handle ramping A up or down
        """

        if any(
            [
                self.future_a_coefficient is None,
                self.initial_a_coefficient is None,
            ]
        ):
            return self.a_coefficient * self.A_PRECISION

        if TYPE_CHECKING:
            assert self.initial_a_coefficient is not None
            assert self.initial_a_coefficient_time is not None
            assert self.future_a_coefficient_time is not None
            assert self.future_a_coefficient is not None

        if self._create_timestamp >= self.future_a_coefficient_time:
            return self.future_a_coefficient

        if timestamp is None:
            timestamp = connection_manager.get_web3(self.chain_id).eth.get_block("latest")[
                "timestamp"
            ]

        a_1 = self.future_a_coefficient
        t_1 = self.future_a_coefficient_time

        # Modified from contract template to check timestamp argument instead
        # of block.timestamp
        if timestamp < t_1:
            a_0 = self.initial_a_coefficient
            t_0 = self.initial_a_coefficient_time
            if a_1 > a_0:
                scaled_a = a_0 + (a_1 - a_0) * (timestamp - t_0) // (t_1 - t_0)
            else:
                scaled_a = a_0 - (a_0 - a_1) * (timestamp - t_0) // (t_1 - t_0)
        else:
            scaled_a = a_1

        return scaled_a

    def _calc_token_amount(
        self,
        amounts: Sequence[int],
        deposit: bool,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        """
        Simplified method to calculate addition or reduction in token supply at
        deposit or withdrawal without taking fees into account (but looking at
        slippage).
        Needed to prevent front-running, not for precise calculations!
        """

        n_coins = len(self.tokens)

        pool_balances = (
            list(override_state.balances) if override_state is not None else list(self.balances)
        )

        block_number = (
            cast("BlockNumber", block_identifier)
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                connection_manager.get_web3(self.chain_id),
            )
        )

        xp = self._xp(rates=self.rate_multipliers, balances=pool_balances)
        amp = self._a(timestamp=self._block_timestamps[block_number])
        d_0 = self._get_d(_xp=xp, _amp=amp)

        for i in range(n_coins):
            if deposit:
                pool_balances[i] += amounts[i]
            else:
                pool_balances[i] -= amounts[i]

        xp = self._xp(rates=self.rate_multipliers, balances=pool_balances)
        d_1 = self._get_d(xp, amp)
        token_amount: int = self.lp_token.get_total_supply(block_identifier=block_number)

        diff = d_1 - d_0 if deposit else d_0 - d_1

        return diff * token_amount // d_0

    def _calc_withdraw_one_coin(
        self, _token_amount: int, i: int, block_identifier: BlockIdentifier | None = None
    ) -> tuple[int, ...]:
        block_number = (
            cast("BlockNumber", block_identifier)
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                connection_manager.get_web3(self.chain_id),
            )
        )

        n_coins = len(self.tokens)
        amp = self._a(timestamp=self._block_timestamps[block_number])
        total_supply = self.lp_token.get_total_supply(block_identifier=block_number)
        precisions = self.precision_multipliers
        xp = self._xp(rates=self.rate_multipliers, balances=self.balances)
        d_0 = self._get_d(xp, amp)
        d_1 = d_0 - _token_amount * d_0 // total_supply
        new_y = self._get_y_d(amp, i, xp, d_1)
        dy_0 = (xp[i] - new_y) // precisions[i]

        xp_reduced = list(xp)
        _fee = self.fee * n_coins // (4 * (n_coins - 1))
        for j in range(n_coins):
            dx_expected = xp[j] * d_1 // d_0 - new_y if j == i else xp[j] - xp[j] * d_1 // d_0
            xp_reduced[j] -= _fee * dx_expected // self.FEE_DENOMINATOR

        dy = xp_reduced[i] - self._get_y_d(amp, i, xp_reduced, d_1)
        dy = (dy - 1) // precisions[i]

        return dy, dy_0 - dy, total_supply

    def _get_scaled_redemption_price(self, block_number: BlockNumber) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_scaled_redemption_price[block_number]

        redemption_price_scale = 10**9

        w3 = connection_manager.get_web3(self.chain_id)

        snap_contract_address: str
        (snap_contract_address,) = eth_abi.abi.decode(
            types=["address"],
            data=w3.eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="redemption_price_snap()")[:4],
                },
                block_identifier=block_number,
            ),
        )

        rate: int
        (rate,) = eth_abi.abi.decode(
            types=["uint256"],
            data=w3.eth.call(
                transaction={
                    "to": get_checksum_address(snap_contract_address),
                    "data": Web3.keccak(text="snappedRedemptionPrice()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        result = rate // redemption_price_scale
        self._cached_scaled_redemption_price[block_number] = result
        return result

    def _get_dy(
        self,
        i: int,
        j: int,
        dx: int,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        """
        @notice Calculate the current output dy given input dx
        @dev Index values can be found via the `coins` public getter method
        @param i Index value for the coin to send
        @param j Index value of the coin to recieve
        @param dx Amount of `i` being exchanged
        @return Amount of `j` predicted

        Reference: https://github.com/curveresearch/notes/blob/main/stableswap.pdf
        """

        def _dynamic_fee(xpi: int, xpj: int, _fee: int, _feemul: int) -> int:
            if _feemul <= self.FEE_DENOMINATOR:
                return _fee
            xps2 = (xpi + xpj) ** 2
            return (_feemul * _fee) // (
                (_feemul - self.FEE_DENOMINATOR) * 4 * xpi * xpj // xps2 + self.FEE_DENOMINATOR
            )

        pool_balances = override_state.balances if override_state is not None else self.balances
        rates = self.rate_multipliers

        block_number = (
            cast("BlockNumber", block_identifier)
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                connection_manager.get_web3(self.chain_id),
            )
        )

        if self.base_pool is not None:
            if self.address in ("0xC61557C5d177bd7DC889A3b621eEC333e168f68A",):
                rates = (
                    self.PRECISION,
                    self._get_virtual_price(block_number=block_number),
                )
            elif self.address in ("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d",):
                rates = (
                    self._get_scaled_redemption_price(block_number=block_number),
                    self._get_virtual_price(block_number=block_number),
                )
            else:
                rates = (
                    self.rate_multipliers[0],
                    self._get_virtual_price(block_number=block_number),
                )

            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return (dy - fee) * self.PRECISION // rates[j]

        if self.address in (
            "0x4e0915C88bC70750D68C481540F081fEFaF22273",
            "0x1005F7406f32a61BD760CfA14aCCd2737913d546",
            "0x6A274dE3e2462c7614702474D64d376729831dCa",
            "0xb9446c4Ef5EBE66268dA6700D26f96273DE3d571",
            "0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a",
        ):
            live_balances = [
                token.get_balance(self.address, block_identifier=block_number)
                for token in self.tokens
            ]
            admin_balances = self._get_admin_balances(block_number=block_number)

            balances = [
                pool_balance - admin_balance
                for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
            ]

            xp = self._xp(rates=rates, balances=balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return (dy - fee) * self.PRECISION // rates[j]

        if self.address == "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5":
            # TODO: check if any functions (price_scale, gamma, D, fee_calc) can be calculated
            # off-chain

            def _d(block_number: BlockNumber) -> int:
                with contextlib.suppress(KeyError):
                    return self._cached_contract_D[block_number]

                w3 = connection_manager.get_web3(self.chain_id)

                d: int
                (d,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": Web3.keccak(text="D()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )
                self._cached_contract_D[block_number] = d
                return d

            def _gamma(block_number: BlockNumber) -> int:
                with contextlib.suppress(KeyError):
                    return self._cached_gamma[block_number]

                w3 = connection_manager.get_web3(self.chain_id)

                gamma: int
                (gamma,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": Web3.keccak(text="gamma()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )
                self._cached_gamma[block_number] = gamma
                return gamma

            def _price_scale(block_number: BlockNumber) -> tuple[int, ...]:
                with contextlib.suppress(KeyError):
                    return self._cached_price_scale[block_number]

                n_coins = len(self.tokens)

                w3 = connection_manager.get_web3(self.chain_id)

                price_scale = [0] * (n_coins - 1)
                for token_index in range(n_coins - 1):
                    (price_scale[token_index],) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=w3.eth.call(
                            transaction={
                                "to": self.address,
                                "data": Web3.keccak(text="price_scale(uint256)")[:4]
                                + eth_abi.abi.encode(
                                    types=["uint256"],
                                    args=[token_index],
                                ),
                            },
                            block_identifier=block_number,
                        ),
                    )
                self._cached_price_scale[block_number] = tuple(price_scale)
                return tuple(price_scale)

            def _newton_y(ann: int, gamma: int, xp: Sequence[int], d: int, token_index: int) -> int:
                """
                Calculating xp[i] given other balances xp[0..N_COINS-1] and invariant D
                _ann = A * N**N
                """

                n_coins = len(self.tokens)
                a_multiplier = self.A_PRECISION

                # Safety checks
                assert (
                    n_coins**n_coins * a_multiplier - 1
                    < ann
                    < 10000 * n_coins**n_coins * a_multiplier + 1
                ), "unsafe value for A"
                assert 10**10 - 1 < gamma < 10**16 + 1, "unsafe values for gamma"
                assert 10**17 - 1 < d < 10**15 * 10**18 + 1, "unsafe values for D"

                for index in range(3):
                    if index != token_index:
                        frac = xp[index] * 10**18 // d
                        assert 10**16 - 1 < frac < 10**20 + 1, (
                            f"{frac=} out of range"
                        )  # dev: unsafe values x[i]

                y = d // n_coins
                k_0_i = 10**18
                s_i = 0

                x_sorted = list(xp)
                x_sorted[token_index] = 0
                x_sorted = sorted(x_sorted, reverse=True)  # From high to low

                convergence_limit = max(x_sorted[0] // 10**14, d // 10**14, 100)
                for _j in range(2, n_coins + 1):
                    _x = x_sorted[n_coins - _j]
                    y = y * d // (_x * n_coins)  # Small _x first
                    s_i += _x

                for _k in range(n_coins - 1):
                    k_0_i = k_0_i * x_sorted[_k] * n_coins // d  # Large _x first

                for _ in range(255):  # pragma: no branch
                    y_prev = y

                    k_0 = k_0_i * y * n_coins // d
                    s = s_i + y

                    _g1k0 = gamma + 10**18
                    _g1k0 = _g1k0 - k_0 + 1 if _g1k0 > k_0 else k_0 - _g1k0 + 1

                    mul1 = 10**18 * d // gamma * _g1k0 // gamma * _g1k0 * a_multiplier // ann
                    mul2 = 10**18 + (2 * 10**18) * k_0 // _g1k0

                    yfprime = 10**18 * y + s * mul2 + mul1
                    _dyfprime = d * mul2

                    if yfprime < _dyfprime:
                        y = y_prev // 2
                        continue

                    yfprime -= _dyfprime
                    fprime = yfprime // y

                    y_minus = mul1 // fprime
                    y_plus = (yfprime + 10**18 * d) // fprime + y_minus * 10**18 // k_0
                    y_minus += 10**18 * s // fprime

                    y = y_prev // 2 if y_plus < y_minus else y_plus - y_minus
                    diff = y - y_prev if y > y_prev else y_prev - y

                    if diff < max(convergence_limit, y // 10**14):
                        frac = y * 10**18 // d
                        assert 10**16 - 1 < frac < 10**20 + 1, "unsafe value for y"
                        return y

                raise EVMRevertError(
                    error=f"_newton_y() did not converge for pool {self.address}"
                )  # pragma: no cover

            def _reduction_coefficient(x: Sequence[int], fee_gamma: int) -> int:
                """
                fee_gamma / (fee_gamma + (1 - K))
                where
                K = prod(x) / (sum(x) / N)**N
                (all normalized to 1e18)
                """
                k = 10**18
                s = 0
                for x_i in x:
                    s += x_i
                # Could be good to pre-sort x, but it is used only for dynamic fee,
                # so that is not so important
                for x_i in x:
                    k = k * n_coins * x_i // s
                if fee_gamma > 0:
                    k = fee_gamma * 10**18 // (fee_gamma + 10**18 - k)
                return k

            n_coins = len(self.tokens)

            assert i != j, "coin index out of range"
            assert i < n_coins, "coin index out of range"
            assert j < n_coins, "coin index out of range"
            assert dx > 0, "do not exchange 0 coins"

            precisions = [
                10**12,  # USDT
                10**10,  # WBTC
                1,  # WETH
            ]

            price_scale = _price_scale(block_number=block_number)

            _xp = list(pool_balances)
            _xp[i] += dx
            _xp[0] *= precisions[0]

            for k in range(n_coins - 1):
                _xp[k + 1] = _xp[k + 1] * price_scale[k] * precisions[k + 1] // self.PRECISION

            amp = self._a(timestamp=self._block_timestamps[block_number])
            gamma = _gamma(block_number=block_number)
            d = _d(block_number=block_number)
            y = _newton_y(amp, gamma, _xp, d, j)
            dy = _xp[j] - y - 1

            _xp[j] = y
            if j > 0:
                dy = dy * self.PRECISION // price_scale[j - 1]
            dy //= precisions[j]

            f = _reduction_coefficient(_xp, self.fee_gamma)
            fee_calc = (self.mid_fee * f + self.out_fee * (10**18 - f)) // 10**18

            dy -= fee_calc * dy // 10**10
            return dy

        if self.address in (
            "0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F",
            "0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714",
            "0x93054188d876f558f4a66B2EF1d97d16eDf0895B",
            "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        ):
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = (xp[j] - y - 1) * self.PRECISION // rates[j]
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return dy - fee

        if self.address in (
            "0x0Ce6a5fF5217e38315f87032CF90686C96627CAA",
            "0x19b080FE1ffA0553469D20Ca36219F17Fcf03859",
            "0x1C5F80b6B68A9E1Ef25926EeE00b5255791b996B",
            "0x1F6bb2a7a2A84d08bb821B89E38cA651175aeDd4",
            "0x21B45B2c1C53fDFe378Ed1955E8Cc29aE8cE0132",
            "0x3CFAa1596777CAD9f5004F9a0c443d912E262243",
            "0x3F1B0278A9ee595635B61817630cC19DE792f506",
            "0x4424b4A37ba0088D8a718b8fc2aB7952C7e695F5",
            "0x602a9Abb10582768Fd8a9f13aD6316Ac2A5A2e2B",
            "0x8461A004b50d321CB22B7d034969cE6803911899",
            "0x857110B5f8eFD66CC3762abb935315630AC770B5",
            "0x8818a9bb44Fbf33502bE7c15c500d0C783B73067",
            "0x9c2C8910F113181783c249d8F6Aa41b51Cde0f0c",
            "0xa1F8A6807c402E4A15ef4EBa36528A3FED24E577",
            "0xaE34574AC03A15cd58A92DC79De7B1A0800F1CE3",
            "0xAf25fFe6bA5A8a29665adCfA6D30C5Ae56CA0Cd3",
            "0xBa3436Fd341F2C8A928452Db3C5A3670d1d5Cc73",
            "0xbB2dC673E1091abCA3eaDB622b18f6D4634b2CD9",
            "0xc5424B857f758E906013F3555Dad202e4bdB4567",
            "0xc8a7C1c4B748970F57cA59326BcD49F5c9dc43E3",
            "0xcbD5cC53C5b846671C6434Ab301AD4d210c21184",
            "0xD6Ac1CB9019137a896343Da59dDE6d097F710538",
            "0xD7C10449A6D134A9ed37e2922F8474EAc6E5c100",
            "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022",
            "0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2",
            "0xe7A3b38c39F97E977723bd1239C3470702568e7B",
            "0xf083FBa98dED0f9C970e5a418500bad08D8b9732",
            "0xF178C0b5Bb7e7aBF4e12A4838C7b7c5bA2C623c0",
            "0xf253f83AcA21aAbD2A20553AE0BF7F65C755A07F",
            "0xfC8c34a3B3CFE1F1Dd6DBCCEC4BC5d3103b80FF0",
            "0xFD5dB7463a3aB53fD211b4af195c5BCCC1A03890",
        ):
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return (dy - fee) * self.PRECISION // rates[j]

        if self.address in (
            "0x04c90C198b2eFF55716079bc06d7CCc4aa4d7512",
            "0x320B564Fb9CF36933eC507a846ce230008631fd3",
            "0x48fF31bBbD8Ab553Ebe7cBD84e1eA3dBa8f54957",
            "0x55A8a39bc9694714E2874c1ce77aa1E599461E18",
            "0x875DF0bA24ccD867f8217593ee27253280772A97",
            "0x9D0464996170c6B9e75eED71c68B99dDEDf279e8",
            "0xBaaa1F5DbA42C3389bDbc2c9D2dE134F5cD0Dc89",
            "0xDa5B670CcD418a187a3066674A8002Adc9356Ad1",
            "0xf03bD3cfE85f00bF5819AC20f0870cE8a8d1F0D8",
            "0xFB9a265b5a1f52d97838Ec7274A0b1442efAcC87",
        ):
            xp = tuple(pool_balances)
            x = xp[i] + dx
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return dy - fee

        if self.address in (
            "0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492",
            "0xBfAb6FA95E0091ed66058ad493189D2cB29385E6",
        ):
            live_balances = [
                token.get_balance(self.address, block_identifier=block_number)
                for token in self.tokens
            ]
            admin_balances = self._get_admin_balances(block_number=block_number)
            balances = [
                pool_balance - admin_balance
                for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
            ]
            rates = self._stored_rates_from_oracle(block_number=block_number)
            xp = self._xp(rates=rates, balances=balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return (dy - fee) * self.PRECISION // rates[j]

        if self.address in (
            "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C",
            "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56",
            "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD",
        ):
            rates = self._stored_rates_from_ctokens(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = (xp[j] - y) * self.PRECISION // rates[j]
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return dy - fee

        if self.address in ("0x2dded6Da1BF5DBdF597C45fcFaa3194e53EcfeAF",):
            rates = self._stored_rates_from_cytokens(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            return (dy - (self.fee * dy // self.FEE_DENOMINATOR)) * self.PRECISION // rates[j]

        if self.address in ("0x06364f10B501e868329afBc005b3492902d6C763",):
            rates = self._stored_rates_from_ytokens(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = (xp[j] - y - 1) * self.PRECISION // rates[j]
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return dy - fee

        if self.address in (
            "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
            "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
        ):
            rates = self._stored_rates_from_ytokens(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = (xp[j] - y) * self.PRECISION // rates[j]
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return dy - fee

        if self.address in ("0xA96A65c051bF88B4095Ee1f2451C2A9d43F53Ae2",):
            rates = self._stored_rates_from_aeth(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return (dy - fee) * self.PRECISION // rates[j]

        if self.address in ("0xF9440930043eb3997fc70e1339dBb11F341de7A8",):
            rates = self._stored_rates_from_reth(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return (dy - fee) * self.PRECISION // rates[j]

        if self.address in ("0xEB16Ae0052ed37f479f7fe63849198Df1765a733",):
            live_balances = [
                token.get_balance(self.address, block_identifier=block_number)
                for token in self.tokens
            ]
            admin_balances = self._get_admin_balances(block_number=block_number)

            _xp = [
                pool_balance - admin_balance
                for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
            ]
            x = _xp[i] + dx
            y = self._get_y(i, j, x, _xp)
            dy = _xp[j] - y
            _fee = (
                _dynamic_fee(
                    xpi=(_xp[i] + x) // 2,
                    xpj=(_xp[j] + y) // 2,
                    _fee=self.fee,
                    _feemul=self.offpeg_fee_multiplier,
                )
                * dy
                // self.FEE_DENOMINATOR
            )
            return dy - _fee

        if self.address in ("0xDeBF20617708857ebe4F679508E7b7863a8A8EeE",):
            live_balances = [
                token.get_balance(self.address, block_identifier=block_number)
                for token in self.tokens
            ]
            admin_balances = self._get_admin_balances(block_number=block_number)
            balances = [
                pool_balance - admin_balance
                for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
            ]

            _xp = [
                balance * rate
                for balance, rate in zip(balances, self.precision_multipliers, strict=True)
            ]

            x = _xp[i] + dx * self.precision_multipliers[i]
            y = self._get_y(i, j, x, _xp)
            dy = (_xp[j] - y) // self.precision_multipliers[j]

            _fee = (
                _dynamic_fee(
                    xpi=(_xp[i] + x) // 2,
                    xpj=(_xp[j] + y) // 2,
                    _fee=self.fee,
                    _feemul=self.offpeg_fee_multiplier,
                )
                * dy
                // self.FEE_DENOMINATOR
            )
            return dy - _fee

        # default pool behavior
        xp = self._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // self.PRECISION)
        y = self._get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = self.fee * dy // self.FEE_DENOMINATOR
        return (dy - fee) * self.PRECISION // rates[j]

    def _get_dy_underlying(
        self,
        i: int,
        j: int,
        dx: int,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        if TYPE_CHECKING:
            assert self.base_pool is not None

        pool_balances = override_state.balances if override_state is not None else self.balances

        block_number = (
            cast("BlockNumber", block_identifier)
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                identifier=block_identifier,
                w3=connection_manager.get_web3(self.chain_id),
            )
        )

        if self.address == "0x618788357D0EBd8A37e763ADab3bc575D54c2C7d":
            base_n_coins = len(self.base_pool.tokens)
            max_coin = len(self.tokens) - 1
            redemption_coin = 0

            # dx and dy in underlying units
            rates = (
                self._get_scaled_redemption_price(block_number=block_number),
                vp_rate := self._get_virtual_price(block_number=block_number),
            )
            xp = self._xp(rates=rates, balances=pool_balances)

            # Use base_i or base_j if they are >= 0
            base_i = i - max_coin
            base_j = j - max_coin
            meta_i = max_coin
            meta_j = max_coin
            if base_i < 0:
                meta_i = i
            if base_j < 0:
                meta_j = j

            if base_i < 0:
                x = xp[i] + (
                    dx
                    * self._get_scaled_redemption_price(block_number=block_number)
                    // self.PRECISION
                )
            elif base_j < 0:
                # i is from BasePool
                # At first, get the amount of pool tokens
                base_inputs = [0] * base_n_coins
                base_inputs[base_i] = dx
                # Token amount transformed to underlying "dollars"
                x = (
                    self.base_pool._calc_token_amount(
                        amounts=base_inputs,
                        deposit=True,
                        block_identifier=block_number,
                        override_state=(
                            override_state.base if override_state is not None else None
                        ),
                    )
                    * vp_rate
                    // self.PRECISION
                )
                # Accounting for deposit/withdraw fees approximately
                x -= x * self.base_pool.fee // (2 * self.FEE_DENOMINATOR)
                # Adding number of pool tokens
                x += xp[max_coin]
            else:
                # If both are from the base pool
                return self.base_pool._get_dy(
                    i=base_i,
                    j=base_j,
                    dx=dx,
                    override_state=(override_state.base if override_state is not None else None),
                )

            # This pool is involved only when in-pool assets are used
            y = self._get_y(meta_i, meta_j, x, xp)
            dy = xp[meta_j] - y - 1
            dy = dy - self.fee * dy // self.FEE_DENOMINATOR
            if j == redemption_coin:
                dy = (dy * self.PRECISION) // self._get_scaled_redemption_price(
                    block_number=block_number
                )

            # If output is going via the metapool
            if base_j >= 0:
                # j is from BasePool
                # The fee is already accounted for
                dy, *_ = self.base_pool._calc_withdraw_one_coin(
                    _token_amount=dy * self.PRECISION // vp_rate,
                    i=base_j,
                    block_identifier=block_number,
                )

            return dy

        if self.address in (
            "0xC61557C5d177bd7DC889A3b621eEC333e168f68A",
            "0x4606326b4Db89373F5377C316d3b0F6e55Bc6A20",
        ):
            base_n_coins = len(self.base_pool.tokens)
            max_coin = len(self.tokens) - 1

            rates = (self.PRECISION, self._get_virtual_price(block_number=block_number))
            xp = self._xp(rates=rates, balances=pool_balances)

            base_i = 0
            base_j = 0
            meta_i = 0
            meta_j = 0

            if i != 0:
                base_i = i - max_coin
                meta_i = 1
            if j != 0:
                base_j = j - max_coin
                meta_j = 1

            if i == 0:
                x = xp[i] + dx * (rates[0] // 10**18)
            elif j == 0:
                # i is from BasePool
                # At first, get the amount of pool tokens
                base_inputs = [0] * base_n_coins
                base_inputs[base_i] = dx
                # Token amount transformed to underlying "dollars"
                x = (
                    self.base_pool._calc_token_amount(
                        amounts=base_inputs,
                        deposit=True,
                        block_identifier=block_number,
                        override_state=(
                            override_state.base if override_state is not None else None
                        ),
                    )
                    * rates[1]
                    // self.PRECISION
                )
                # Accounting for deposit/withdraw fees approximately
                x -= x * self.base_pool.fee // (2 * self.FEE_DENOMINATOR)
                # Adding number of pool tokens
                x += xp[max_coin]
            else:
                # If both are from the base pool
                return self.base_pool._get_dy(
                    i=base_i,
                    j=base_j,
                    dx=dx,
                    block_identifier=block_number,
                    override_state=(override_state.base if override_state is not None else None),
                )

            # This pool is involved only when in-pool assets are used
            y = self._get_y(meta_i, meta_j, x, xp)
            dy = xp[meta_j] - y - 1
            dy = dy - self.fee * dy // self.FEE_DENOMINATOR

            # If output is going via the metapool
            if j == 0:
                dy //= rates[0] // 10**18
            else:
                # j is from BasePool
                # The fee is already accounted for
                dy, *_ = self.base_pool._calc_withdraw_one_coin(
                    _token_amount=dy * self.PRECISION // rates[1],
                    i=base_j,
                    block_identifier=block_number,
                )

            return dy

        _rates = list(self.rate_multipliers)

        vp_rate = self._get_virtual_price(block_number=block_number)
        _rates[-1] = vp_rate

        xp = self._xp(rates=tuple(_rates), balances=pool_balances)
        precisions = self.precision_multipliers

        base_n_coins = len(self.base_pool.tokens)
        max_coin = len(self.tokens) - 1

        # Use base_i or base_j if they are >= 0
        base_i = i - max_coin
        base_j = j - max_coin
        meta_i = max_coin
        meta_j = max_coin
        if base_i < 0:
            meta_i = i
        if base_j < 0:
            meta_j = j

        if base_i < 0:
            x = xp[i] + dx * precisions[i]
        elif base_j < 0:
            # i is from BasePool
            # At first, get the amount of pool tokens
            base_inputs = [0] * base_n_coins
            base_inputs[base_i] = dx
            # Token amount transformed to underlying "dollars"
            x = (
                self.base_pool._calc_token_amount(
                    amounts=base_inputs,
                    deposit=True,
                    block_identifier=block_number,
                    override_state=(override_state.base if override_state is not None else None),
                )
                * vp_rate
                // self.PRECISION
            )
            # Accounting for deposit/withdraw fees approximately
            x -= x * self.base_pool.fee // (2 * self.FEE_DENOMINATOR)
            # Adding number of pool tokens
            x += xp[max_coin]
        else:
            # If both are from the base pool
            return self.base_pool._get_dy(
                i=base_i,
                j=base_j,
                dx=dx,
                block_identifier=block_number,
                override_state=(override_state.base if override_state is not None else None),
            )

        # This pool is involved only when in-pool assets are used
        y = self._get_y(meta_i, meta_j, x, xp)
        dy = xp[meta_j] - y - 1
        dy = dy - self.fee * dy // self.FEE_DENOMINATOR

        # If output is going via the metapool
        if base_j < 0:
            dy //= precisions[meta_j]
        else:
            # j is from BasePool
            # The fee is already accounted for
            dy, *_ = self.base_pool._calc_withdraw_one_coin(
                _token_amount=dy * self.PRECISION // vp_rate,
                i=base_j,
                block_identifier=block_number,
            )

        return dy

    def _get_base_cache_updated(self, block_number: BlockNumber) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_base_cache_updated[block_number]

        w3 = connection_manager.get_web3(self.chain_id)

        base_cache_updated: int
        (base_cache_updated,) = eth_abi.abi.decode(
            types=["uint256"],
            data=w3.eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="base_cache_updated()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        self._cached_base_cache_updated[block_number] = base_cache_updated
        return base_cache_updated

    def _get_base_virtual_price(self, block_number: BlockNumber) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_base_virtual_price[block_number]

        w3 = connection_manager.get_web3(self.chain_id)

        base_virtual_price: int
        (base_virtual_price,) = eth_abi.abi.decode(
            types=["uint256"],
            data=w3.eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="base_virtual_price()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        self._cached_base_virtual_price[block_number] = base_virtual_price
        return base_virtual_price

    def _get_virtual_price(self, block_number: BlockNumber) -> int:
        if TYPE_CHECKING:
            assert self.base_pool is not None

        with contextlib.suppress(KeyError):
            return self._cached_virtual_price[block_number]

        base_cache_expires = 10 * 60  # 10 minutes

        w3 = connection_manager.get_web3(self.chain_id)
        self._block_timestamps[block_number] = w3.eth.get_block(block_identifier=block_number)[
            "timestamp"
        ]

        base_virtual_price: int
        if (
            self.base_cache_updated is None
            or self._block_timestamps[block_number] > self.base_cache_updated + base_cache_expires
        ):
            (base_virtual_price,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    transaction={
                        "to": self.base_pool.address,
                        "data": Web3.keccak(text="get_virtual_price()")[:4],
                    },
                    block_identifier=block_number,
                ),
            )
        else:
            base_virtual_price = self.base_virtual_price

        self._cached_virtual_price[block_number] = base_virtual_price
        self.base_virtual_price = base_virtual_price
        return base_virtual_price

    def _get_admin_balances(self, block_number: BlockNumber) -> tuple[int, ...]:
        with contextlib.suppress(KeyError):
            return self._cached_admin_balances[block_number]

        admin_balances: list[int] = []
        for token_index, _ in enumerate(self.tokens):
            admin_balance: int
            (admin_balance,) = raw_call(
                w3=connection_manager.get_web3(chain_id=self.chain_id),
                address=self.address,
                calldata=encode_function_calldata(
                    function_prototype="admin_balances(uint256)",
                    function_arguments=[token_index],
                ),
                return_types=["uint256"],
                block_identifier=block_number,
            )
            admin_balances.append(admin_balance)

        self._cached_admin_balances[block_number] = tuple(admin_balances)
        return tuple(admin_balances)

    def _get_d(self, _xp: Sequence[int], _amp: int) -> int:
        """
        Solve for the Curve stableswap invariant D, using a modified Newton's method.

        Mainnet V1 Curve pools have several calculation variants to calculate the D and D_prev
        values. The pool addresses using each variant are grouped and the appropriate function is
        set at runtime.
        """

        def calc_d(
            *,
            a_nn: int,
            s: int,
            d: int,
            d_p: int,
            n_coins: int,
            a_precision: int,
        ) -> int:
            return (
                (a_nn * s // a_precision + d_p * n_coins)
                * d
                // ((a_nn - a_precision) * d // a_precision + (n_coins + 1) * d_p)
            )

        def calc_d_variant_alpha(
            *,
            a_nn: int,
            s: int,
            d: int,
            d_p: int,
            n_coins: int,
            a_precision: int,  # noqa:ARG001
        ) -> int:
            return (a_nn * s + d_p * n_coins) * d // ((a_nn - 1) * d + (n_coins + 1) * d_p)

        def calc_dp(
            *,
            d: int,
            d_p: int,
            xp: Sequence[int],
        ) -> int:
            for x in xp:
                d_p = d_p * d // (x * n_coins)
            return d_p

        def calc_dp_variant_alpha(
            *,
            d: int,
            d_p: int,
            xp: Sequence[int],
        ) -> int:
            for x in xp:
                d_p = d_p * d // (x * n_coins + 1)
            return d_p

        def calc_dp_variant_beta(
            *,
            d: int,
            d_p: int,  # noqa:ARG001
            xp: Sequence[int],
        ) -> int:
            return d * d // xp[0] * d // xp[1] // n_coins**2

        def calc_dp_variant_gamma(
            *,
            d: int,
            d_p: int,  # noqa:ARG001
            xp: Sequence[int],
        ) -> int:
            return d * d // xp[0] * d // xp[1] // cast("int", n_coins**n_coins)

        d_func = calc_d
        dp_func = calc_dp
        if self.address in self.D_VARIANT_GROUP_0:
            d_func = calc_d_variant_alpha
        elif self.address in self.D_VARIANT_GROUP_1:
            d_func = calc_d_variant_alpha
            dp_func = calc_dp_variant_alpha
        elif self.address in self.D_VARIANT_GROUP_2:
            dp_func = calc_dp_variant_beta
        elif self.address in self.D_VARIANT_GROUP_3:
            dp_func = calc_dp_variant_alpha
        elif self.address in self.D_VARIANT_GROUP_4:
            dp_func = calc_dp_variant_gamma

        d = s = sum(_xp)
        if s == 0:
            return 0
        n_coins = len(self.tokens)
        a_nn = _amp * n_coins

        for _ in range(255):  # pragma: no branch
            d_p = d_prev = d
            d_p = dp_func(d=d, d_p=d_p, xp=_xp)
            d = d_func(a_nn=a_nn, s=s, d=d, d_p=d_p, n_coins=n_coins, a_precision=self.A_PRECISION)
            if d_prev < d:
                if d - d_prev <= 1:
                    return d
            elif d_prev - d <= 1:
                return d

        raise EVMRevertError(error="D calculation did not converge.")  # pragma: no cover

    def _get_y(self, i: int, j: int, x: int, xp: Sequence[int]) -> int:
        """
        Calculate x[j] if one makes x[i] = x

        Done by solving quadratic equation iteratively.
        x_1**2 + x_1 * (sum' - (A*n**n - 1) * D / (A * n**n)) = D ** (n + 1) / (
            n ** (2 * n) * prod' * A
        )
        x_1**2 + b*x_1 = c

        x_1 = (x_1**2 + c) / (2*x_1 + b)
        """

        # x in the input is converted to the same price/precision

        n_coins = len(self.tokens)

        assert i != j, "same coin"
        assert j >= 0, "j below zero"
        assert j < n_coins, "j above N_COINS"

        # should be unreachable, but good for safety
        assert i >= 0
        assert i < n_coins

        amp = (
            self._a(timestamp=self._block_timestamps[self.update_block]) // self.A_PRECISION
            if self.address in self.Y_VARIANT_GROUP_0
            else self._a(timestamp=self._block_timestamps[self.update_block])
        )
        c = y = d = self._get_d(xp, amp)

        s = 0
        for coin_index in range(n_coins):
            if coin_index == i:
                _x = x
            elif coin_index != j:
                _x = xp[coin_index]
            else:
                continue
            s += _x
            c = c * d // (_x * n_coins)

        a_nn = amp * n_coins
        if self.address in self.Y_VARIANT_GROUP_1:
            c = c * d // (a_nn * n_coins)
            b = s + d // a_nn
        else:
            c = c * d * self.A_PRECISION // (a_nn * n_coins)
            b = s + d * self.A_PRECISION // a_nn

        for _ in range(255):  # pragma: no branch
            y_prev = y
            y = (y * y + c) // (2 * y + b - d)
            if y > y_prev:
                if y - y_prev <= 1:
                    return y
            elif y_prev - y <= 1:
                return y

        raise EVMRevertError(error="y calculation did not converge.")  # pragma: no cover

    def _get_y_d(self, a: int, i: int, xp: Sequence[int], d: int) -> int:
        n_coins = len(self.tokens)

        assert i >= 0  # dev: i below zero
        assert i < n_coins  # dev: i above N_COINS

        c = y = d

        s = 0
        for coin_index in range(n_coins):
            if coin_index != i:
                x = xp[coin_index]
            else:
                continue
            s += x
            c = c * d // (x * n_coins)

        a_nn = a * n_coins
        if self.address in self.Y_D_VARIANT_GROUP_0:
            b = s + d * self.A_PRECISION // a_nn
            c = c * d * self.A_PRECISION // (a_nn * n_coins)
        else:
            b = s + d // a_nn
            c = c * d // (a_nn * n_coins)

        for _ in range(255):  # pragma: no branch
            y_prev = y
            y = (y * y + c) // (2 * y + b - d)
            if y > y_prev:
                if y - y_prev <= 1:
                    return y
            elif y_prev - y <= 1:
                return y

        raise EVMRevertError(error="y_d calculation did not converge.")  # pragma: no cover

    def _set_oracle_method(self, block_number: BlockNumber) -> None:
        w3 = connection_manager.get_web3(self.chain_id)
        (self.oracle_method,) = eth_abi.abi.decode(
            types=["uint256"],
            data=w3.eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="oracle_method()")[:4],
                },
                block_identifier=block_number,
            ),
        )

    def _stored_rates_from_ctokens(self, block_number: BlockNumber) -> tuple[int, ...]:
        with contextlib.suppress(KeyError):
            return self._cached_rates_from_ctokens[block_number]

        result: list[int] = []
        rate: int
        for token, use_lending, multiplier in zip(
            self.tokens,
            self.use_lending,
            self.precision_multipliers,
            strict=True,
        ):
            if not use_lending:
                rate = self.PRECISION
            else:
                w3 = connection_manager.get_web3(self.chain_id)
                (rate,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        transaction={
                            "to": token.address,
                            "data": Web3.keccak(text="exchangeRateStored()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )
                supply_rate: int
                (supply_rate,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        transaction={
                            "to": token.address,
                            "data": Web3.keccak(text="supplyRatePerBlock()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )
                old_block: int
                (old_block,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        transaction={
                            "to": token.address,
                            "data": Web3.keccak(text="accrualBlockNumber()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )

                rate += rate * supply_rate * (block_number - old_block) // self.PRECISION

            result.append(multiplier * rate)

        self._cached_rates_from_ctokens[block_number] = tuple(result)
        return tuple(result)

    def _stored_rates_from_ytokens(self, block_number: BlockNumber) -> tuple[int, ...]:
        with contextlib.suppress(KeyError):
            return self._cached_rates_from_ytokens[block_number]

        # ref: https://etherscan.io/address/0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27#code

        result: list[int] = []
        for token, multiplier, use_lending in zip(
            self.tokens,
            self.precision_multipliers,
            self.use_lending,
            strict=True,
        ):
            if use_lending:
                w3 = connection_manager.get_web3(self.chain_id)
                rate: int
                (rate,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        transaction={
                            "to": token.address,
                            "data": Web3.keccak(text="getPricePerFullShare()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )
            else:
                rate = self.LENDING_PRECISION

            result.append(rate * multiplier)

        self._cached_rates_from_ytokens[block_number] = tuple(result)
        return tuple(result)

    def _stored_rates_from_cytokens(self, block_number: BlockNumber) -> tuple[int, ...]:
        with contextlib.suppress(KeyError):
            return self._cached_rates_from_cytokens[block_number]

        w3 = connection_manager.get_web3(self.chain_id)

        result: list[int] = []
        for token, precision_multiplier in zip(
            self.tokens, self.precision_multipliers, strict=True
        ):
            rate: int
            (rate,) = eth_abi.abi.decode(
                types=["uint256"],
                data=(
                    w3.eth.call(
                        transaction={
                            "to": token.address,
                            "data": Web3.keccak(text="exchangeRateStored()")[:4],
                        },
                        block_identifier=block_number,
                    )
                ),
            )
            supply_rate: int
            (supply_rate,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    transaction={
                        "to": token.address,
                        "data": Web3.keccak(text="supplyRatePerBlock()")[:4],
                    },
                    block_identifier=block_number,
                ),
            )
            old_block: int
            (old_block,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    transaction={
                        "to": token.address,
                        "data": Web3.keccak(text="accrualBlockNumber()")[:4],
                    },
                    block_identifier=block_number,
                ),
            )

            rate += rate * supply_rate * (block_number - old_block) // self.PRECISION
            result.append(precision_multiplier * rate)

        self._cached_rates_from_cytokens[block_number] = tuple(result)
        return tuple(result)

    def _stored_rates_from_reth(self, block_number: BlockNumber) -> tuple[int, ...]:
        with contextlib.suppress(KeyError):
            return self.PRECISION, self._cached_rates_from_reth[block_number]

        w3 = connection_manager.get_web3(self.chain_id)

        # ref: https://etherscan.io/address/0xF9440930043eb3997fc70e1339dBb11F341de7A8#code
        ratio: int
        (ratio,) = eth_abi.abi.decode(
            types=["uint256"],
            data=w3.eth.call(
                transaction={
                    "to": self.tokens[1].address,
                    "data": Web3.keccak(text="getExchangeRate()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        self._cached_rates_from_reth[block_number] = ratio
        return self.PRECISION, ratio

    def _stored_rates_from_aeth(self, block_number: BlockNumber) -> tuple[int, ...]:
        with contextlib.suppress(KeyError):
            return (
                self.PRECISION,
                self.PRECISION
                * self.LENDING_PRECISION
                // self._cached_rates_from_aeth[block_number],
            )

        w3 = connection_manager.get_web3(self.chain_id)

        # ref: https://etherscan.io/address/0xA96A65c051bF88B4095Ee1f2451C2A9d43F53Ae2#code
        ratio: int
        (ratio,) = eth_abi.abi.decode(
            types=["uint256"],
            data=w3.eth.call(
                transaction={
                    "to": self.tokens[1].address,
                    "data": Web3.keccak(text="ratio()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        self._cached_rates_from_aeth[block_number] = ratio
        return self.PRECISION, self.PRECISION * self.LENDING_PRECISION // ratio

    def _stored_rates_from_oracle(self, block_number: BlockNumber) -> tuple[int, ...]:
        """
        Get rates from on-chain oracle

        Ref: https://etherscan.io/address/0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492#code
        """

        with contextlib.suppress(KeyError):
            return self._cached_rates_from_oracle[block_number]

        self._set_oracle_method(block_number=block_number)
        if TYPE_CHECKING:
            assert self.oracle_method is not None

        if self.oracle_method == 0:
            rates = self.rate_multipliers
        else:
            oracle_bit_mask = (2**32 - 1) * 256**28
            oracle_rate: int
            (oracle_rate,) = eth_abi.abi.decode(
                types=["uint256"],
                data=connection_manager.get_web3(self.chain_id).eth.call(
                    transaction={
                        "to": get_checksum_address(HexBytes(self.oracle_method % 2**160)),
                        "data": HexBytes(self.oracle_method & oracle_bit_mask),
                    },
                    block_identifier=block_number,
                ),
            )
            rates = (
                self.rate_multipliers[0],
                self.rate_multipliers[1] * oracle_rate // self.PRECISION,
            )

        self._cached_rates_from_oracle[block_number] = rates
        return rates

    def _xp(self, rates: Iterable[int], balances: Iterable[int]) -> tuple[int, ...]:
        return tuple(
            rate * balance // self.PRECISION for rate, balance in zip(rates, balances, strict=True)
        )

    def auto_update(self, block_number: BlockNumber | None = None) -> bool:
        """
        Retrieve and set updated balances from the contract
        """

        with self._state_lock:
            w3 = connection_manager.get_web3(self.chain_id)

            state_block = w3.eth.get_block("latest" if block_number is None else block_number)
            block_number = state_block["number"]

            token_balances = []
            token_balance: int
            for token_id, _ in enumerate(self.tokens):
                (token_balance,) = eth_abi.abi.decode(
                    types=[self._coin_index_type],
                    data=w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": Web3.keccak(text=f"balances({self._coin_index_type})")[:4]
                            + eth_abi.abi.encode(types=[self._coin_index_type], args=[token_id]),
                        },
                        block_identifier=block_number,
                    ),
                )
                token_balances.append(token_balance)

            if self.base_pool is not None:
                self.base_pool.auto_update(block_number=block_number)
                if self.base_cache_updated is not None:
                    self.base_cache_updated = self._get_base_cache_updated(
                        block_number=block_number
                    )

            found_updates = tuple(token_balances) != self.balances

            state = (
                CurveStableswapPoolState(
                    address=self.address,
                    balances=tuple(token_balances),
                    base=self.base_pool.state,
                    block=block_number,
                )
                if self.base_pool is not None
                else CurveStableswapPoolState(
                    address=self.address,
                    balances=tuple(token_balances),
                    block=block_number,
                )
            )
            self._state_cache[block_number] = state
            self._state = state
            self._block_timestamps[block_number] = state_block["timestamp"]

            if found_updates:
                self._notify_subscribers(
                    message=CurveStableSwapPoolStateUpdated(state),
                )

            return found_updates

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: CurveStableswapPoolState | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        """

        block_number = (
            cast("BlockNumber", block_identifier)
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                connection_manager.get_web3(self.chain_id),
            )
        )

        if token_in_quantity <= 0:
            raise InvalidSwapInputAmount

        if override_state:
            logger.debug("Overrides applied:")
            logger.debug(f"Balances: {override_state.balances}")

        tokens_used_this_pool = [
            token_in in self.tokens,
            token_out in self.tokens,
        ]

        tokens_used_in_base_pool = []
        if self.base_pool is not None:
            tokens_used_in_base_pool = [
                token_in in self.base_pool.tokens,
                token_out in self.base_pool.tokens,
            ]

        if all(tokens_used_this_pool):
            if any(balance == 0 for balance in self.balances):
                raise NoLiquidity(message="One or more of the tokens has a zero balance.")

            return self._get_dy(
                i=self.tokens.index(token_in),
                j=self.tokens.index(token_out),
                dx=token_in_quantity,
                block_identifier=block_number,
                override_state=override_state,
            )
        if any(tokens_used_this_pool) and any(tokens_used_in_base_pool):
            if TYPE_CHECKING:
                assert self.base_pool is not None

            # TODO: see if any of these checks are unnecessary (partial zero balanece OK?)
            if any(balance == 0 for balance in self.base_pool.balances):
                raise NoLiquidity(message="One or more of the base pool tokens has a zero balance.")
            if any(balance == 0 for balance in self.balances):
                raise NoLiquidity(message="One or more of the tokens has a zero balance.")

            token_in_from_metapool = token_in in self.tokens
            token_out_from_metapool = token_out in self.tokens
            assert token_in_from_metapool or token_out_from_metapool

            if token_in_from_metapool and self.balances[self.tokens.index(token_in)] == 0:
                raise NoLiquidity(message=f"{token_in} has a zero balance.")
            if token_out_from_metapool and self.balances[self.tokens.index(token_out)] == 0:
                raise NoLiquidity(message=f"{token_out} has a zero balance.")

            token_in_from_basepool = token_in in self.base_pool.tokens
            token_out_from_basepool = token_out in self.base_pool.tokens
            assert token_in_from_basepool or token_out_from_basepool

            if (
                token_in_from_basepool
                and self.base_pool.balances[self.base_pool.tokens.index(token_in)] == 0
            ):
                raise NoLiquidity(message=f"{token_in} has a zero balance.")
            if (
                token_out_from_basepool
                and self.base_pool.balances[self.base_pool.tokens.index(token_out)] == 0
            ):
                raise NoLiquidity(message=f"{token_out} has a zero balance.")

            return self._get_dy_underlying(
                i=(
                    self.tokens.index(token_in)
                    if token_in_from_metapool
                    else self.tokens_underlying.index(token_in)
                ),
                j=(
                    self.tokens.index(token_out)
                    if token_out_from_metapool
                    else self.tokens_underlying.index(token_out)
                ),
                dx=token_in_quantity,
                block_identifier=block_number,
                override_state=override_state,
            )
        if all(tokens_used_in_base_pool):
            token_in_from_basepool = token_in in self.tokens_underlying
            token_out_from_basepool = token_out in self.tokens_underlying
            assert token_in_from_basepool or token_out_from_basepool

            return self._get_dy_underlying(
                i=self.tokens_underlying.index(token_in),
                j=self.tokens_underlying.index(token_out),
                dx=token_in_quantity,
                block_identifier=block_number,
                override_state=override_state,
            )

        raise DegenbotValueError(
            message="Tokens not held by pool or in underlying base pool"
        )  # pragma: no cover

    def get_arbitrage_helpers(self) -> tuple[AbstractArbitrage, ...]:
        return tuple(
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, AbstractArbitrage)
        )
