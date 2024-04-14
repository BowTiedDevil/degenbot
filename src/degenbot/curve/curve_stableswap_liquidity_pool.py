# TODO
# ----------------------------------------------------
# PRIORITY      TASK
# high          write state_update method
# high          create a manager for Curve pools
# medium        add liquidity modifying mode for external_update
# medium        investigate differences in get_dy_underlying vs exchange_underlying at GUSD-3Crv


from threading import Lock
from typing import TYPE_CHECKING, Any, Dict, List, Tuple

import eth_abi.abi
import web3.exceptions
from eth_abi.exceptions import InsufficientDataBytes
from eth_typing import AnyAddress, ChecksumAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from web3 import Web3
from web3.contract.contract import Contract
from web3.types import BlockIdentifier

from .. import config
from ..baseclasses import BaseLiquidityPool
from ..constants import ZERO_ADDRESS
from ..dex.curve import (
    BROKEN_CURVE_V1_POOLS,
    CACHED_CURVE_V1_POOL_ATTRIBUTES,
    CURVE_V1_FACTORY_ADDRESS,
    CURVE_V1_REGISTRY_ADDRESS,
)
from ..erc20_token import Erc20Token
from ..exceptions import BrokenPool, EVMRevertError, ZeroLiquidityError, ZeroSwapError
from ..functions import get_number_for_block_identifier
from ..logging import logger
from ..manager.token_manager import Erc20TokenHelperManager
from ..registry.all_pools import AllPools
from .abi import CURVE_V1_FACTORY_ABI, CURVE_V1_POOL_ABI, CURVE_V1_REGISTRY_ABI
from .curve_stableswap_dataclasses import (
    CurveStableSwapPoolAttributes,
    CurveStableswapPoolState,
    CurveStableSwapPoolStateUpdated,
)


class CurveStableswapPool(BaseLiquidityPool):
    # Constants from contract
    # ref: https://github.com/curvefi/curve-contract/blob/master/contracts/pool-templates/base/SwapTemplateBase.vy
    PRECISION_DECIMALS = 18
    PRECISION = 10**PRECISION_DECIMALS
    LENDING_PRECISION = PRECISION
    FEE_DENOMINATOR = 10**10
    A_PRECISION = 100
    MAX_COINS = 8

    def __init__(
        self,
        address: ChecksumAddress | str,
        abi: List[Any] | None = None,
        silent: bool = False,
        state_block: int | None = None,
    ) -> None:
        """
        Create a `CurveStableswapPool` object for interaction with a Curve V1
        (StableSwap) pool.

        Arguments
        ---------
        address : str
            Address for the deployed pool contract.
        tokens : List[Erc20Token], optional
            "Erc20Token" objects for the tokens held by the deployed pool.
        abi : list, optional
            Contract ABI.
        silent : bool
            Suppress status output.
        state_block: int, optional
            Fetch initial state values from the chain at a particular block
            height. Defaults to the latest block if omitted.
        """

        self.fee_gamma: int
        self.mid_fee: int
        self.offpeg_fee_multiplier: int
        self.out_fee: int
        self.precision_multipliers: List[int]
        self.rate_multipliers: List[int]
        self.use_lending: List[bool]

        def _get_coin_index_type() -> str:
            # Identify the coins input format (int128 or uint256)
            # Some contracts accept token_id as an int128, some accept uint256
            for _type in ["int128", "uint256"]:
                try:
                    eth_abi.abi.decode(
                        types=["address"],
                        data=_w3.eth.call(
                            transaction={
                                "to": self.address,
                                "data": Web3.keccak(text=f"coins({_type})")[:4]
                                + eth_abi.abi.encode(types=[_type], args=[0]),
                            },
                            block_identifier=state_block,
                        ),
                    )
                except (
                    InsufficientDataBytes,
                    web3.exceptions.ContractLogicError,
                ):
                    continue
                else:
                    return _type

            raise ValueError("Could not determine input type for pool")  # pragma: no cover

        def _get_token_addresses() -> List[ChecksumAddress]:
            token_addresses = []
            for token_id in range(self.MAX_COINS):
                try:
                    token_address, *_ = eth_abi.abi.decode(
                        types=["address"],
                        data=_w3.eth.call(
                            transaction={
                                "to": self.address,
                                "data": Web3.keccak(text=f"coins({self._coin_index_type})")[:4]
                                + eth_abi.abi.encode(
                                    types=[self._coin_index_type], args=[token_id]
                                ),
                            },
                            block_identifier=state_block,
                        ),
                    )
                except web3.exceptions.ContractLogicError:
                    break
                else:
                    token_addresses.append(to_checksum_address(token_address))
            return token_addresses

        def _get_lp_token_address() -> ChecksumAddress:
            for contract in [_w3_registry_contract, _w3_factory_contract]:
                lp_token_address, *_ = eth_abi.abi.decode(
                    types=["address"],
                    data=_w3.eth.call(
                        transaction={
                            "to": contract.address,
                            "data": Web3.keccak(text="get_lp_token(address)")[:4]
                            + eth_abi.abi.encode(types=["address"], args=[self.address]),
                        },
                        block_identifier=state_block,
                    ),
                )
                if lp_token_address != ZERO_ADDRESS:
                    return to_checksum_address(lp_token_address)

            raise ValueError(
                f"Could not identify LP token for pool {self.address}"
            )  # pragma: no cover

        def _get_pool_from_lp_token(token: AnyAddress) -> ChecksumAddress:
            pool_address, *_ = eth_abi.abi.decode(
                types=["address"],
                data=_w3.eth.call(
                    transaction={
                        "to": _w3_registry_contract.address,
                        "data": Web3.keccak(text="get_pool_from_lp_token(address)")[:4]
                        + eth_abi.abi.encode(
                            types=["address"],
                            args=[to_checksum_address(token)],
                        ),
                    },
                    block_identifier=state_block,
                ),
            )
            return to_checksum_address(pool_address)

        def _is_metapool() -> bool:
            for contract in [_w3_factory_contract, _w3_registry_contract]:
                try:
                    is_meta, *_ = eth_abi.abi.decode(
                        types=["bool"],
                        data=_w3.eth.call(
                            transaction={
                                "to": contract.address,
                                "data": Web3.keccak(text="is_meta(address)")[:4]
                                + eth_abi.abi.encode(types=["address"], args=[self.address]),
                            },
                            block_identifier=state_block,
                        ),
                    )
                except Exception:
                    continue
                else:
                    if is_meta:
                        return True
            return False

        def _set_pool_specific_attributes() -> None:
            match self.address:
                case "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56":
                    self.use_lending = [True, True]
                    self.precision_multipliers = [1, 10**12]
                case "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5":
                    self.fee_gamma = 10000000000000000
                    self.mid_fee = 4000000
                    self.out_fee = 40000000
                case "0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2":
                    self.precision_multipliers = [1, 1000000000000]
                case "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C":
                    self.use_lending = [True, True, False]
                    self.precision_multipliers = [1, 10**12, 10**12]
                case "0x06364f10B501e868329afBc005b3492902d6C763":
                    self.use_lending = [True, True, True, False]
                case "0xDeBF20617708857ebe4F679508E7b7863a8A8EeE":
                    self.precision_multipliers = [1, 10**12, 10**12]
                    self.offpeg_fee_multiplier, *_ = eth_abi.abi.decode(
                        types=["uint256"],
                        data=_w3.eth.call(
                            transaction={
                                "to": self.address,
                                "data": Web3.keccak(text="offpeg_fee_multiplier()")[:4],
                            },
                            block_identifier=state_block,
                        ),
                    )
                case "0x2dded6Da1BF5DBdF597C45fcFaa3194e53EcfeAF":
                    self.precision_multipliers = [1, 10**12, 10**12]
                case "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27":
                    self.precision_multipliers = [1, 10**12, 10**12, 1]
                    self.use_lending = [True] * len(self.tokens)
                case "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51":
                    self.precision_multipliers = [1, 10**12, 10**12, 1]
                    self.use_lending = [True] * len(self.tokens)
                case "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD":
                    self.use_lending = [False] * len(self.tokens)
                case (
                    "0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492"
                    | "0xBfAb6FA95E0091ed66058ad493189D2cB29385E6"
                ):
                    self.oracle_method, *_ = eth_abi.abi.decode(
                        types=["uint256"],
                        data=_w3.eth.call(
                            transaction={
                                "to": self.address,
                                "data": Web3.keccak(text="oracle_method()")[:4],
                            },
                            block_identifier=state_block,
                        ),
                    )
                case "0xEB16Ae0052ed37f479f7fe63849198Df1765a733":
                    self.offpeg_fee_multiplier, *_ = eth_abi.abi.decode(
                        types=["uint256"],
                        data=_w3.eth.call(
                            transaction={
                                "to": self.address,
                                "data": Web3.keccak(text="offpeg_fee_multiplier()")[:4],
                            },
                            block_identifier=state_block,
                        ),
                    )

        self.address = to_checksum_address(address)
        if self.address in BROKEN_CURVE_V1_POOLS:
            raise BrokenPool(f"Pool {self.address} is broken")

        self._state_lock = Lock()
        self.abi = abi if abi is not None else CURVE_V1_POOL_ABI

        _w3 = config.get_web3()
        if state_block is None:
            state_block = _w3.eth.get_block_number()
        self.update_block = state_block
        self._create_timestamp = _w3.eth.get_block("latest")["timestamp"]
        chain_id = _w3.eth.chain_id

        cached_pool_attributes: CurveStableSwapPoolAttributes | None = None
        try:
            # if chain_id in POOL_ATTRIBUTES and self.address in POOL_ATTRIBUTES[chain_id]:
            cached_pool_attributes = CurveStableSwapPoolAttributes(
                **CACHED_CURVE_V1_POOL_ATTRIBUTES[chain_id][self.address]
            )
        except KeyError:
            pass

        _w3_contract = self._w3_contract
        _w3_registry_contract = _w3.eth.contract(
            address=CURVE_V1_REGISTRY_ADDRESS, abi=CURVE_V1_REGISTRY_ABI
        )
        _w3_factory_contract = _w3.eth.contract(
            address=CURVE_V1_FACTORY_ADDRESS,
            abi=CURVE_V1_FACTORY_ABI,
        )

        # @dev These dicts are simple caches to hold retrieved values from on-chain calls.
        # @dev They have no functionality to evict old values and will grow without bound
        self._cached_admin_balance: Dict[Tuple[int, int], int] = {}
        self._cached_base_cache_updated: Dict[int, int] = {}
        self._cached_base_virtual_price: Dict[int, int] = {}
        self._cached_contract_D: Dict[int, int] = {}
        self._cached_gamma: Dict[int, int] = {}
        self._cached_price_scale: Dict[int, List[int]] = {}
        self._cached_rates_from_aeth: Dict[int, List[int]] = {}
        self._cached_rates_from_ctokens: Dict[int, List[int]] = {}
        self._cached_rates_from_cytokens: Dict[int, List[int]] = {}
        self._cached_rates_from_oracle: Dict[int, List[int]] = {}
        self._cached_rates_from_reth: Dict[int, int] = {}
        self._cached_rates_from_ytokens: Dict[int, List[int]] = {}
        self._cached_scaled_redemption_price: Dict[int, int] = {}
        self._cached_virtual_price: Dict[int, int] = {}

        self.a_coefficient: int = _w3_contract.functions.A().call(block_identifier=state_block)
        self.initial_a_coefficient: int | None = None
        self.initial_a_coefficient_time: int | None = None
        self.future_a_coefficient: int | None = None
        self.future_a_coefficient_time: int | None = None

        try:
            self.initial_a_coefficient = _w3_contract.functions.initial_A().call(
                block_identifier=state_block
            )
            self.initial_a_coefficient_time = _w3_contract.functions.initial_A_time().call(
                block_identifier=state_block
            )
            self.future_a_coefficient = _w3_contract.functions.future_A().call(
                block_identifier=state_block
            )
            self.future_a_coefficient_time = _w3_contract.functions.future_A_time().call(
                block_identifier=state_block
            )
        except Exception:
            pass

        # fee setup
        self.fee: int = (
            cached_pool_attributes.fee
            if cached_pool_attributes is not None
            else _w3_contract.functions.fee().call(block_identifier=state_block)
        )
        self.admin_fee: int = (
            cached_pool_attributes.admin_fee
            if cached_pool_attributes is not None
            else _w3_contract.functions.admin_fee().call(block_identifier=state_block)
        )

        # token setup
        self._coin_index_type: str = (
            cached_pool_attributes.coin_index_type
            if cached_pool_attributes is not None
            else _get_coin_index_type()
        )
        token_addresses = (
            [to_checksum_address(coin) for coin in cached_pool_attributes.coin_addresses]
            if cached_pool_attributes is not None
            else _get_token_addresses()
        )
        lp_token_address = (
            to_checksum_address(cached_pool_attributes.lp_token_address)
            if cached_pool_attributes is not None
            else _get_lp_token_address()
        )

        # metapool setup
        self.is_metapool = (
            cached_pool_attributes.is_metapool
            if cached_pool_attributes is not None
            else _is_metapool()
        )
        if self.is_metapool is True:
            # Curve metapools hold the LP token for the base pool at index 1
            base_pool_lp_token_address = token_addresses[1]
            base_pool_address = (
                to_checksum_address(cached_pool_attributes.base_pool_address)
                if cached_pool_attributes is not None
                and cached_pool_attributes.base_pool_address is not None
                else _get_pool_from_lp_token(base_pool_lp_token_address)
            )

        _token_manager = Erc20TokenHelperManager(chain_id)
        self.tokens = tuple(
            [
                _token_manager.get_erc20token(
                    address=token_address,
                    silent=silent,
                )
                for token_address in token_addresses
            ]
        )
        self.lp_token = _token_manager.get_erc20token(
            address=lp_token_address,
            silent=silent,
        )

        self.balances = []
        for token_id, _ in enumerate(self.tokens):
            token_balance, *_ = eth_abi.abi.decode(
                types=[self._coin_index_type],
                data=_w3.eth.call(
                    transaction={
                        "to": self.address,
                        "data": Web3.keccak(text=f"balances({self._coin_index_type})")[:4]
                        + eth_abi.abi.encode(types=[self._coin_index_type], args=[token_id]),
                    },
                    block_identifier=state_block,
                ),
            )
            self.balances.append(token_balance)

        if self.is_metapool is True:
            self.base_pool: CurveStableswapPool
            try:
                base_pool = AllPools(chain_id)[base_pool_address]
                if TYPE_CHECKING:
                    assert isinstance(base_pool, CurveStableswapPool)
                self.base_pool = base_pool
            except KeyError:
                self.base_pool = CurveStableswapPool(
                    base_pool_address, state_block=state_block, silent=silent
                )

            base_pool_tokens = self.base_pool.tokens

            if TYPE_CHECKING:
                assert base_pool_tokens is not None

            self.tokens_underlying = tuple([self.tokens[0]] + list(base_pool_tokens))

            self.base_cache_updated: int | None = None
            try:
                self.base_cache_updated = self._get_base_cache_updated(block_number=state_block)
            except web3.exceptions.ContractLogicError:
                pass

            self.base_virtual_price: int
            try:
                self.base_virtual_price = self._get_base_virtual_price(block_number=state_block)
            except web3.exceptions.ContractLogicError:
                pass

        # 3pool example
        # rate_multipliers = [
        #   10**12000000,             <------ 10**18 == 10**(18 + 18 - 18)
        #   10**12000000000000000000, <------ 10**30 == 10**(18 + 18 - 6)
        #   10**12000000000000000000, <------ 10**30 == 10**(18 + 18 - 6)
        # ]

        self.rate_multipliers = [
            10 ** (2 * self.PRECISION_DECIMALS - token.decimals) for token in self.tokens
        ]
        self.precision_multipliers = [
            10 ** (self.PRECISION_DECIMALS - token.decimals) for token in self.tokens
        ]

        _set_pool_specific_attributes()

        fee_string = f"{100*self.fee/self.FEE_DENOMINATOR:.2f}"
        token_string = "-".join([token.symbol for token in self.tokens])
        self.name = f"{token_string} (CurveStable, {fee_string}%)"

        self._subscribers = set()

        self.state: CurveStableswapPoolState
        self._update_pool_state()
        self._pool_state_archive: Dict[int, CurveStableswapPoolState] = {
            0: CurveStableswapPoolState(
                pool=self.address,
                balances=self.balances,
                base=getattr(self, "base_pool", None),
            ),
            state_block: self.state,
        }

        AllPools(chain_id)[self.address] = self

        if not silent:
            logger.info(
                f"{self.name} @ {self.address}, A={self.a_coefficient}, fee={100*self.fee/self.FEE_DENOMINATOR:.2f}%"
            )
            for token_id, (token, balance) in enumerate(zip(self.tokens, self.balances)):
                logger.info(f"• Token {token_id}: {token} - Reserves: {balance}")

        # print(
        #     f'"{self.address}" :',
        #     f"dict(\
        #         address='{self.address}',\
        #         lp_token_address='{self.lp_token.address}',\
        #         coin_addresses={[token.address for token in self.tokens]},\
        #         coin_index_type='{self._coin_index_type}',\
        #         underlying_coin_addresses={([token.address for token in self.tokens_underlying] if self.is_metapool else None)},\
        #         fee={self.fee},\
        #         admin_fee={self.admin_fee},\
        #         is_metapool={self.is_metapool},\
        #         base_pool_address={self.base_pool.address if self.is_metapool else None},\
        #     ),",
        # )

    def __getstate__(self) -> Dict[str, Any]:
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
        return f"CurveStableswapPool(address={self.address}, tokens={token_string}, fee={100*self.fee/self.FEE_DENOMINATOR:.2f}%, A={self.a_coefficient})"

    def _update_pool_state(self) -> None:
        self.state = CurveStableswapPoolState(pool=self.address, balances=self.balances)
        self._notify_subscribers(
            message=CurveStableSwapPoolStateUpdated(self.state),
        )

    @property
    def _w3_contract(self) -> Contract:
        return config.get_web3().eth.contract(
            address=self.address,
            abi=self.abi,
        )

    def _A(self, timestamp: int | None = None) -> int:
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
            assert self.future_a_coefficient_time is not None
            assert self.future_a_coefficient is not None

        if self._create_timestamp >= self.future_a_coefficient_time:
            return self.future_a_coefficient

        if timestamp is None:
            timestamp = config.get_web3().eth.get_block("latest")["timestamp"]

        A1 = self.future_a_coefficient
        t1 = self.future_a_coefficient_time

        if TYPE_CHECKING:
            assert A1 is not None
            assert t1 is not None

        # Modified from contract template to check timestamp argument instead
        # of block.timestamp
        if timestamp < t1:
            A0 = self.initial_a_coefficient
            t0 = self.initial_a_coefficient_time
            if TYPE_CHECKING:
                assert A0 is not None
                assert t0 is not None
            if A1 > A0:
                scaled_A = A0 + (A1 - A0) * (timestamp - t0) // (t1 - t0)
            else:
                scaled_A = A0 - (A0 - A1) * (timestamp - t0) // (t1 - t0)
        else:
            scaled_A = A1

        return scaled_A

    def _get_scaled_redemption_price(self, block_number: int) -> int:
        try:
            return self._cached_scaled_redemption_price[block_number]
        except KeyError:
            pass

        REDEMPTION_PRICE_SCALE = 10**9

        _w3 = config.get_web3()

        snap_contract_address: str
        snap_contract_address, *_ = eth_abi.abi.decode(
            types=["address"],
            data=_w3.eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="redemption_price_snap()")[:4],
                },
                block_identifier=block_number,
            ),
        )

        rate: int
        rate, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=_w3.eth.call(
                transaction={
                    "to": to_checksum_address(snap_contract_address),
                    "data": Web3.keccak(text="snappedRedemptionPrice()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        result = rate // REDEMPTION_PRICE_SCALE
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
        """
        # ref: https://github.com/curveresearch/notes/blob/main/stableswap.pdf

        result: int

        if override_state is not None:
            pool_balances = override_state.balances.copy()
        else:
            pool_balances = self.balances.copy()

        block_number = (
            config.get_web3().eth.get_block_number()
            if block_identifier is None
            else get_number_for_block_identifier(block_identifier)
        )

        def _dynamic_fee(xpi: int, xpj: int, _fee: int, _feemul: int) -> int:
            if _feemul <= self.FEE_DENOMINATOR:
                return _fee
            else:
                xps2 = (xpi + xpj) ** 2
                return (_feemul * _fee) // (
                    (_feemul - self.FEE_DENOMINATOR) * 4 * xpi * xpj // xps2 + self.FEE_DENOMINATOR
                )

        if self.address in (
            "0x4e0915C88bC70750D68C481540F081fEFaF22273",
            "0x1005F7406f32a61BD760CfA14aCCd2737913d546",
            "0x6A274dE3e2462c7614702474D64d376729831dCa",
            "0xb9446c4Ef5EBE66268dA6700D26f96273DE3d571",
            "0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a",
        ):
            # TODO: investigate providing overrides for live-lookup contracts
            live_balances = [
                token.get_balance(self.address, block_identifier=block_number)
                for token in self.tokens
            ]
            admin_balances = [
                self._get_admin_balance(token_index=token_index, block_number=block_number)
                for token_index, _ in enumerate(self.tokens)
            ]
            balances = [
                pool_balance - admin_balance
                for pool_balance, admin_balance in zip(live_balances, admin_balances)
            ]
            rates = self.rate_multipliers
            xp = self._xp(rates=rates, balances=balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = (dy - fee) * self.PRECISION // rates[j]
            return result

        elif self.address == "0x618788357D0EBd8A37e763ADab3bc575D54c2C7d":
            rates = [
                self._get_scaled_redemption_price(block_number=block_number),
                self._get_virtual_price(block_number=block_number),
            ]
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = (dy - fee) * self.PRECISION // rates[j]
            return result

        elif self.is_metapool:
            rates = self.rate_multipliers.copy()
            if self.address in (
                "0xC61557C5d177bd7DC889A3b621eEC333e168f68A",
                "0x8038C01A0390a8c547446a0b2c18fc9aEFEcc10c",
            ):
                rates[0] = 10**self.PRECISION_DECIMALS

            rates[1] = self._get_virtual_price(block_number=block_number)

            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            _fee = self.fee * dy // self.FEE_DENOMINATOR
            result = (dy - _fee) * self.PRECISION // rates[j]
            return result

        elif self.address == "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5":
            # TODO: check if any functions (price_scale, gamma, D, fee_calc) can be calculated off-chain

            def _D(block_number: int) -> int:
                try:
                    return self._cached_contract_D[block_number]
                except KeyError:
                    pass

                _w3 = config.get_web3()

                D: int
                D, *_ = eth_abi.abi.decode(
                    types=["uint256"],
                    data=_w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": Web3.keccak(text="D()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )
                self._cached_contract_D[block_number] = D
                return D

            def _gamma(block_number: int) -> int:
                try:
                    return self._cached_gamma[block_number]
                except KeyError:
                    pass

                _w3 = config.get_web3()

                gamma: int
                gamma, *_ = eth_abi.abi.decode(
                    types=["uint256"],
                    data=_w3.eth.call(
                        transaction={
                            "to": self.address,
                            "data": Web3.keccak(text="gamma()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )
                self._cached_gamma[block_number] = gamma
                return gamma

            def _price_scale(block_number: int) -> List[int]:
                try:
                    return self._cached_price_scale[block_number]
                except KeyError:
                    pass

                N_COINS = len(self.tokens)
                _w3 = config.get_web3()

                price_scale = [0] * (N_COINS - 1)
                for token_index in range(N_COINS - 1):
                    price_scale[token_index], *_ = eth_abi.abi.decode(
                        types=["uint256"],
                        data=_w3.eth.call(
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
                self._cached_price_scale[block_number] = price_scale
                return price_scale

            def _newton_y(ann: int, gamma: int, xp: List[int], D: int, token_index: int) -> int:
                """
                Calculating xp[i] given other balances xp[0..N_COINS-1] and invariant D
                _ann = A * N**N
                """

                N_COINS = len(self.tokens)
                A_MULTIPLIER = self.A_PRECISION

                # Safety checks
                assert (
                    ann > N_COINS**N_COINS * A_MULTIPLIER - 1
                    and ann < 10000 * N_COINS**N_COINS * A_MULTIPLIER + 1
                ), "unsafe value for A"
                assert gamma > 10**10 - 1 and gamma < 10**16 + 1, "unsafe values for gamma"
                assert D > 10**17 - 1 and D < 10**15 * 10**18 + 1, "unsafe values for D"

                for k in range(3):
                    if k != token_index:
                        frac = xp[k] * 10**18 // D
                        assert (frac > 10**16 - 1) and (
                            frac < 10**20 + 1
                        ), f"{frac=} out of range"  # dev: unsafe values x[i]

                y = D // N_COINS
                K0_i = 10**18
                S_i = 0

                x_sorted = xp.copy()
                x_sorted[token_index] = 0
                x_sorted = sorted(x_sorted, reverse=True)  # From high to low

                convergence_limit = max(max(x_sorted[0] // 10**14, D // 10**14), 100)
                for j in range(2, N_COINS + 1):
                    _x = x_sorted[N_COINS - j]
                    y = y * D // (_x * N_COINS)  # Small _x first
                    S_i += _x
                for j in range(N_COINS - 1):
                    K0_i = K0_i * x_sorted[j] * N_COINS // D  # Large _x first

                for j in range(255):
                    y_prev = y

                    K0 = K0_i * y * N_COINS // D
                    S = S_i + y

                    _g1k0 = gamma + 10**18
                    if _g1k0 > K0:
                        _g1k0 = _g1k0 - K0 + 1
                    else:
                        _g1k0 = K0 - _g1k0 + 1

                    # D // (A * N**N) * _g1k0**2 // gamma**2
                    mul1 = 10**18 * D // gamma * _g1k0 // gamma * _g1k0 * A_MULTIPLIER // ann

                    # 2*K0 // _g1k0
                    mul2 = 10**18 + (2 * 10**18) * K0 // _g1k0

                    yfprime = 10**18 * y + S * mul2 + mul1
                    _dyfprime = D * mul2
                    if yfprime < _dyfprime:
                        y = y_prev // 2
                        continue
                    else:
                        yfprime -= _dyfprime
                    fprime = yfprime // y

                    # y -= f // f_prime;  y = (y * fprime - f) // fprime
                    # y = (yfprime + 10**18 * D - 10**18 * S) // fprime + mul1 // fprime * (10**18 - K0) // K0
                    y_minus = mul1 // fprime
                    y_plus = (yfprime + 10**18 * D) // fprime + y_minus * 10**18 // K0
                    y_minus += 10**18 * S // fprime

                    if y_plus < y_minus:
                        y = y_prev // 2
                    else:
                        y = y_plus - y_minus

                    diff = 0
                    if y > y_prev:
                        diff = y - y_prev
                    else:
                        diff = y_prev - y
                    if diff < max(convergence_limit, y // 10**14):
                        frac = y * 10**18 // D
                        assert (frac > 10**16 - 1) and (frac < 10**20 + 1), "unsafe value for y"
                        return y

                raise EVMRevertError(
                    f"_newton_y() did not converge for pool {self.address}"
                )  # pragma: no cover

            def _reduction_coefficient(x: List[int], fee_gamma: int) -> int:
                """
                fee_gamma / (fee_gamma + (1 - K))
                where
                K = prod(x) / (sum(x) / N)**N
                (all normalized to 1e18)
                """
                K = 10**18
                S = 0
                for x_i in x:
                    S += x_i
                # Could be good to pre-sort x, but it is used only for dynamic fee,
                # so that is not so important
                for x_i in x:
                    K = K * N_COINS * x_i // S
                if fee_gamma > 0:
                    K = fee_gamma * 10**18 // (fee_gamma + 10**18 - K)
                return K

            N_COINS = len(self.tokens)

            assert i != j and i < N_COINS and j < N_COINS, "coin index out of range"
            assert dx > 0, "do not exchange 0 coins"

            precisions = [
                10**12,  # USDT
                10**10,  # WBTC
                1,  # WETH
            ]

            price_scale = _price_scale(block_number=block_number)
            xp = pool_balances

            xp[i] += dx
            xp[0] *= precisions[0]

            for k in range(N_COINS - 1):
                xp[k + 1] = xp[k + 1] * price_scale[k] * precisions[k + 1] // self.PRECISION

            A = self._A()
            gamma = _gamma(block_number=block_number)
            D = _D(block_number=block_number)
            y = _newton_y(A, gamma, xp, D, j)
            dy = xp[j] - y - 1

            xp[j] = y
            if j > 0:
                dy = dy * self.PRECISION // price_scale[j - 1]
            dy //= precisions[j]

            f = _reduction_coefficient(xp, self.fee_gamma)
            fee_calc = (self.mid_fee * f + self.out_fee * (10**18 - f)) // 10**18

            dy -= fee_calc * dy // 10**10
            return dy

        elif self.address in (
            "0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F",
            "0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714",
            "0x93054188d876f558f4a66B2EF1d97d16eDf0895B",
            "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        ):
            rates = self.rate_multipliers
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = (xp[j] - y - 1) * self.PRECISION // rates[j]
            _fee = self.fee * dy // self.FEE_DENOMINATOR
            return dy - _fee

        elif self.address in (
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
            rates = self.rate_multipliers
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = (dy - fee) * self.PRECISION // rates[j]
            return result

        elif self.address in (
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
            xp = pool_balances
            x = xp[i] + dx
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = dy - fee
            return result

        elif self.address in (
            "0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492",
            "0xBfAb6FA95E0091ed66058ad493189D2cB29385E6",
        ):
            ETH_COIN_INDEX = 0
            DERIVATIVE_ETH_COIN_INDEX = 1

            balances = [
                self.tokens[ETH_COIN_INDEX].get_balance(
                    address=self.address, block_identifier=block_number
                )
                - self._get_admin_balance(token_index=ETH_COIN_INDEX, block_number=block_number),
                self.tokens[DERIVATIVE_ETH_COIN_INDEX].get_balance(
                    address=self.address, block_identifier=block_number
                )
                - self._get_admin_balance(
                    token_index=DERIVATIVE_ETH_COIN_INDEX, block_number=block_number
                ),
            ]
            rates = self._stored_rates_from_oracle(block_number=block_number)
            xp = self._xp(rates=rates, balances=balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = (dy - fee) * self.PRECISION // rates[j]
            return result

        elif self.address in (
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

        elif self.address in ("0x2dded6Da1BF5DBdF597C45fcFaa3194e53EcfeAF",):
            assert self.precision_multipliers == [1, 10**12, 10**12]
            rates = self._stored_rates_from_cytokens(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            result = (dy - (self.fee * dy // self.FEE_DENOMINATOR)) * self.PRECISION // rates[j]
            return result

        elif self.address in ("0x06364f10B501e868329afBc005b3492902d6C763",):
            rates = self._stored_rates_from_ytokens(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = (xp[j] - y - 1) * self.PRECISION // rates[j]
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = dy - fee
            return result

        elif self.address in (
            "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
            "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
        ):
            rates = self._stored_rates_from_ytokens(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = (xp[j] - y) * self.PRECISION // rates[j]
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = dy - fee
            return result

        elif self.address in ("0xA96A65c051bF88B4095Ee1f2451C2A9d43F53Ae2",):
            rates = self._stored_rates_from_aeth(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = (dy - fee) * self.PRECISION // rates[j]
            return result

        elif self.address in ("0xF9440930043eb3997fc70e1339dBb11F341de7A8",):
            rates = self._stored_rates_from_reth(block_number=block_number)
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = (dy - fee) * self.PRECISION // rates[j]
            return result

        elif self.address in ("0xEB16Ae0052ed37f479f7fe63849198Df1765a733",):
            live_balances = [
                token.get_balance(self.address, block_identifier=block_number)
                for token in self.tokens
            ]
            admin_balances = [
                self._get_admin_balance(token_index=token_index, block_number=block_number)
                for token_index, _ in enumerate(self.tokens)
            ]
            balances = [
                pool_balance - admin_balance
                for pool_balance, admin_balance in zip(live_balances, admin_balances)
            ]
            xp = balances

            x = xp[i] + dx
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y
            _fee = (
                _dynamic_fee(
                    xpi=(xp[i] + x) // 2,
                    xpj=(xp[j] + y) // 2,
                    _fee=self.fee,
                    _feemul=self.offpeg_fee_multiplier,
                )
                * dy
                // self.FEE_DENOMINATOR
            )
            return dy - _fee

        elif self.address in ("0xDeBF20617708857ebe4F679508E7b7863a8A8EeE",):
            live_balances = [
                token.get_balance(self.address, block_identifier=block_number)
                for token in self.tokens
            ]
            admin_balances = [
                self._get_admin_balance(token_index=token_index, block_number=block_number)
                for token_index, _ in enumerate(self.tokens)
            ]
            balances = [
                pool_balance - admin_balance
                for pool_balance, admin_balance in zip(live_balances, admin_balances)
            ]

            precisions = self.precision_multipliers
            xp = [balance * rate for balance, rate in zip(balances, precisions)]

            x = xp[i] + dx * precisions[i]
            y = self._get_y(i, j, x, xp)
            dy = (xp[j] - y) // precisions[j]

            _fee = (
                _dynamic_fee(
                    xpi=(xp[i] + x) // 2,
                    xpj=(xp[j] + y) // 2,
                    _fee=self.fee,
                    _feemul=self.offpeg_fee_multiplier,
                )
                * dy
                // self.FEE_DENOMINATOR
            )
            result = dy - _fee
            return result
        else:
            rates = self.rate_multipliers
            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            result = (dy - fee) * self.PRECISION // rates[j]
            return result

    def _get_dy_underlying(
        self,
        i: int,
        j: int,
        dx: int,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        if override_state is not None:
            pool_balances = override_state.balances.copy()
        else:
            pool_balances = self.balances.copy()

        block_number = (
            config.get_web3().eth.get_block_number()
            if block_identifier is None
            else get_number_for_block_identifier(block_identifier)
        )

        if self.address == "0x618788357D0EBd8A37e763ADab3bc575D54c2C7d":
            BASE_N_COINS = len(self.base_pool.tokens)
            MAX_COIN = len(self.tokens) - 1
            REDEMPTION_COIN = 0

            # dx and dy in underlying units
            rates = [
                self._get_scaled_redemption_price(block_number=block_number),
                vp_rate := self._get_virtual_price(block_number=block_number),
            ]
            xp = self._xp(rates=rates, balances=pool_balances)

            # Use base_i or base_j if they are >= 0
            base_i = i - MAX_COIN
            base_j = j - MAX_COIN
            meta_i = MAX_COIN
            meta_j = MAX_COIN
            if base_i < 0:
                meta_i = i
            if base_j < 0:
                meta_j = j

            x = 0
            if base_i < 0:
                x = xp[i] + (
                    dx
                    * self._get_scaled_redemption_price(block_number=block_number)
                    // self.PRECISION
                )
            else:
                if base_j < 0:
                    # i is from BasePool
                    # At first, get the amount of pool tokens
                    base_inputs = [0] * BASE_N_COINS
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
                    x += xp[MAX_COIN]
                else:
                    # If both are from the base pool
                    return self.base_pool._get_dy(
                        i=base_i,
                        j=base_j,
                        dx=dx,
                        override_state=(
                            override_state.base if override_state is not None else None
                        ),
                    )

            # This pool is involved only when in-pool assets are used
            y = self._get_y(meta_i, meta_j, x, xp)
            dy = xp[meta_j] - y - 1
            dy = dy - self.fee * dy // self.FEE_DENOMINATOR
            if j == REDEMPTION_COIN:
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

        elif self.address == "0xC61557C5d177bd7DC889A3b621eEC333e168f68A":
            BASE_N_COINS = len(self.base_pool.tokens)
            MAX_COIN = len(self.tokens) - 1

            rates = [self.PRECISION, self._get_virtual_price(block_number=block_number)]
            xp = self._xp(rates=rates, balances=pool_balances)

            x = 0
            base_i = 0
            base_j = 0
            meta_i = 0
            meta_j = 0

            if i != 0:
                base_i = i - MAX_COIN
                meta_i = 1
            if j != 0:
                base_j = j - MAX_COIN
                meta_j = 1

            if i == 0:
                x = xp[i] + dx * (rates[0] // 10**18)
            else:
                if j == 0:
                    # i is from BasePool
                    # At first, get the amount of pool tokens
                    base_inputs = [0] * BASE_N_COINS
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
                    x += xp[MAX_COIN]
                else:
                    # If both are from the base pool
                    return self.base_pool._get_dy(
                        i=base_i,
                        j=base_j,
                        dx=dx,
                        block_identifier=block_number,
                        override_state=(
                            override_state.base if override_state is not None else None
                        ),
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

        elif self.address == "0x4606326b4Db89373F5377C316d3b0F6e55Bc6A20":
            BASE_N_COINS = len(self.base_pool.tokens)
            MAX_COIN = len(self.tokens) - 1

            rates = [self.PRECISION, self._get_virtual_price(block_number=block_number)]
            xp = self._xp(rates=rates, balances=pool_balances)

            x = 0
            base_i = 0
            base_j = 0
            meta_i = 0
            meta_j = 0

            if i != 0:
                base_i = i - MAX_COIN
                meta_i = 1
            if j != 0:
                base_j = j - MAX_COIN
                meta_j = 1

            if i == 0:
                x = xp[i] + dx * (rates[0] // 10**18)
            else:
                if j == 0:
                    # i is from BasePool
                    # At first, get the amount of pool tokens
                    base_inputs = [0] * BASE_N_COINS
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
                    x += xp[MAX_COIN]
                else:
                    # If both are from the base pool
                    return self.base_pool._get_dy(
                        i=base_i,
                        j=base_j,
                        dx=dx,
                        block_identifier=block_number,
                        override_state=(
                            override_state.base if override_state is not None else None
                        ),
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

        else:
            rates = self.rate_multipliers.copy()

            vp_rate = self._get_virtual_price(block_number=block_number)
            rates[-1] = vp_rate

            xp = self._xp(rates=rates, balances=pool_balances)
            precisions = self.precision_multipliers

            BASE_N_COINS = len(self.base_pool.tokens)
            MAX_COIN = len(self.tokens) - 1

            # Use base_i or base_j if they are >= 0
            base_i = i - MAX_COIN
            base_j = j - MAX_COIN
            meta_i = MAX_COIN
            meta_j = MAX_COIN
            if base_i < 0:
                meta_i = i
            if base_j < 0:
                meta_j = j

            if base_i < 0:
                x = xp[i] + dx * precisions[i]
            else:
                if base_j < 0:
                    # i is from BasePool
                    # At first, get the amount of pool tokens
                    base_inputs = [0] * BASE_N_COINS
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
                    x += xp[MAX_COIN]
                else:
                    # If both are from the base pool
                    return self.base_pool._get_dy(
                        i=base_i,
                        j=base_j,
                        dx=dx,
                        block_identifier=block_number,
                        override_state=(
                            override_state.base if override_state is not None else None
                        ),
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

    def _get_base_cache_updated(self, block_number: int) -> int:
        try:
            return self._cached_base_cache_updated[block_number]
        except KeyError:
            pass

        base_cache_updated: int
        base_cache_updated, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=config.get_web3().eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="base_cache_updated()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        self._cached_base_cache_updated[block_number] = base_cache_updated
        return base_cache_updated

    def _get_base_virtual_price(self, block_number: int) -> int:
        try:
            return self._cached_base_virtual_price[block_number]
        except KeyError:
            pass

        base_virtual_price: int
        base_virtual_price, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=config.get_web3().eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="base_virtual_price()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        self._cached_base_virtual_price[block_number] = base_virtual_price
        return base_virtual_price

    def _get_virtual_price(self, block_number: int) -> int:
        try:
            return self._cached_virtual_price[block_number]
        except KeyError:
            pass

        BASE_CACHE_EXPIRES = 10 * 60  # 10 minutes

        _w3 = config.get_web3()
        timestamp = _w3.eth.get_block(block_identifier=block_number)["timestamp"]

        base_virtual_price: int
        if (
            self.base_cache_updated is None
            or timestamp > self.base_cache_updated + BASE_CACHE_EXPIRES
        ):
            base_virtual_price, *_ = eth_abi.abi.decode(
                types=["uint256"],
                data=_w3.eth.call(
                    transaction={
                        "to": self.base_pool.address,
                        "data": Web3.keccak(text="get_virtual_price()")[:4],
                    },
                    block_identifier=block_number,
                ),
            )
        else:
            base_virtual_price = self.base_virtual_price

        if TYPE_CHECKING:
            assert isinstance(base_virtual_price, int)

        self._cached_virtual_price[block_number] = base_virtual_price
        self.base_virtual_price = base_virtual_price
        return base_virtual_price

    def _calc_token_amount(
        self,
        amounts: List[int],
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

        N_COINS = len(self.tokens)

        if override_state is not None:
            pool_balances = override_state.balances.copy()
        else:
            pool_balances = self.balances.copy()

        block_number = (
            config.get_web3().eth.get_block_number()
            if block_identifier is None
            else get_number_for_block_identifier(block_identifier)
        )

        xp = self._xp(rates=self.rate_multipliers, balances=pool_balances)
        amp = self._A()
        D0 = self._get_D(_xp=xp, _amp=amp)

        for i in range(N_COINS):
            if deposit:
                pool_balances[i] += amounts[i]
            else:
                pool_balances[i] -= amounts[i]

        xp = self._xp(rates=self.rate_multipliers, balances=pool_balances)
        D1: int = self._get_D(xp, amp)
        token_amount: int = self.lp_token.get_total_supply(block_identifier=block_number)

        if deposit:
            diff = D1 - D0
        else:
            diff = D0 - D1

        return diff * token_amount // D0

    def _calc_withdraw_one_coin(
        self, _token_amount: int, i: int, block_identifier: BlockIdentifier | None = None
    ) -> Tuple[int, ...]:
        block_number = (
            config.get_web3().eth.get_block_number()
            if block_identifier is None
            else get_number_for_block_identifier(block_identifier)
        )

        N_COINS = len(self.tokens)

        amp = self._A()
        total_supply = self.lp_token.get_total_supply(block_identifier=block_number)
        precisions = self.precision_multipliers
        xp = self._xp(rates=self.rate_multipliers, balances=self.balances)
        D0 = self._get_D(xp, amp)
        D1 = D0 - _token_amount * D0 // total_supply
        new_y = self._get_y_D(amp, i, xp, D1)
        dy_0 = (xp[i] - new_y) // precisions[i]

        xp_reduced = xp.copy()
        _fee = self.fee * N_COINS // (4 * (N_COINS - 1))
        for j in range(N_COINS):
            dx_expected = 0
            if j == i:
                dx_expected = xp[j] * D1 // D0 - new_y
            else:
                dx_expected = xp[j] - xp[j] * D1 // D0
            xp_reduced[j] -= _fee * dx_expected // self.FEE_DENOMINATOR

        dy = xp_reduced[i] - self._get_y_D(amp, i, xp_reduced, D1)
        dy = (dy - 1) // precisions[i]

        return dy, dy_0 - dy, total_supply

    def _get_admin_balance(self, token_index: int, block_number: int) -> int:
        try:
            return self._cached_admin_balance[block_number, token_index]
        except KeyError:
            pass

        admin_balance: int
        admin_balance = self._w3_contract.functions.admin_balances(token_index).call(
            block_identifier=block_number
        )
        self._cached_admin_balance[block_number, token_index] = admin_balance
        return admin_balance

    def _get_D(self, _xp: List[int], _amp: int) -> int:
        N_COINS = len(self.tokens)

        S = sum(_xp)
        if S == 0:
            return 0

        D = S
        Ann = _amp * N_COINS

        if self.address in (
            "0x06364f10B501e868329afBc005b3492902d6C763",
            "0x4CA9b3063Ec5866A4B82E437059D2C43d1be596F",
            "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C",
            "0x7fC77b5c7614E1533320Ea6DDc2Eb61fa00A9714",
            "0x93054188d876f558f4a66B2EF1d97d16eDf0895B",
            "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        ):
            for _ in range(255):
                D_P = D
                for _x in _xp:
                    D_P = D_P * D // (_x * N_COINS)
                Dprev = D
                D = (Ann * S + D_P * N_COINS) * D // ((Ann - 1) * D + (N_COINS + 1) * D_P)
                if D > Dprev:
                    if D - Dprev <= 1:
                        return D
                else:
                    if Dprev - D <= 1:
                        return D

        elif self.address in (
            "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
            "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
            "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56",
            "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD",
        ):
            for _ in range(255):
                D_P = D
                for _x in _xp:
                    D_P = D_P * D // (_x * N_COINS + 1)
                Dprev = D
                D = (Ann * S + D_P * N_COINS) * D // ((Ann - 1) * D + (N_COINS + 1) * D_P)
                if D > Dprev:
                    if D - Dprev <= 1:
                        return D
                else:
                    if Dprev - D <= 1:
                        return D

        elif self.address in (
            "0x0AD66FeC8dB84F8A3365ADA04aB23ce607ac6E24",
            "0x1C5F80b6B68A9E1Ef25926EeE00b5255791b996B",
            "0x1c899dED01954d0959E034b62a728e7fEbE593b0",
            "0x320B564Fb9CF36933eC507a846ce230008631fd3",
            "0x3b21C2868B6028CfB38Ff86127eF22E68d16d53B",
            "0x3F1B0278A9ee595635B61817630cC19DE792f506",
            "0x3Fb78e61784C9c637D560eDE23Ad57CA1294c14a",
            "0x453D92C7d4263201C69aACfaf589Ed14202d83a4",
            "0x69ACcb968B19a53790f43e57558F5E443A91aF22",
            "0x875DF0bA24ccD867f8217593ee27253280772A97",
            "0x9D0464996170c6B9e75eED71c68B99dDEDf279e8",
            "0xB657B895B265C38c53FFF00166cF7F6A3C70587d",
            "0xD6Ac1CB9019137a896343Da59dDE6d097F710538",
        ):
            for _ in range(255):
                D_P = D * D // _xp[0] * D // _xp[1] // N_COINS**2
                Dprev = D
                D = (
                    (Ann * S // self.A_PRECISION + D_P * N_COINS)
                    * D
                    // ((Ann - self.A_PRECISION) * D // self.A_PRECISION + (N_COINS + 1) * D_P)
                )
                if D > Dprev:
                    if D - Dprev <= 1:
                        return D
                else:
                    if Dprev - D <= 1:
                        return D

        elif self.address in (
            "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022",
            "0xDeBF20617708857ebe4F679508E7b7863a8A8EeE",
            "0xEB16Ae0052ed37f479f7fe63849198Df1765a733",
        ):
            for _ in range(255):
                D_P = D
                for _x in _xp:
                    D_P = D_P * D // (_x * N_COINS + 1)
                Dprev = D
                D = (
                    (Ann * S // self.A_PRECISION + D_P * N_COINS)
                    * D
                    // ((Ann - self.A_PRECISION) * D // self.A_PRECISION + (N_COINS + 1) * D_P)
                )
                if D > Dprev:
                    if D - Dprev <= 1:
                        return D
                else:
                    if Dprev - D <= 1:
                        return D

        else:
            for _ in range(255):
                D_P = D
                for _x in _xp:
                    D_P = D_P * D // (_x * N_COINS)
                Dprev = D
                D = (
                    (Ann * S // self.A_PRECISION + D_P * N_COINS)
                    * D
                    // ((Ann - self.A_PRECISION) * D // self.A_PRECISION + (N_COINS + 1) * D_P)
                )
                if D > Dprev:
                    if D - Dprev <= 1:
                        return D
                else:
                    if Dprev - D <= 1:
                        return D

        raise EVMRevertError(
            f"_get_D() did not converge for pool {self.address}"
        )  # pragma: no cover

    def _get_y(self, i: int, j: int, x: int, xp: List[int]) -> int:
        """
        Calculate x[j] if one makes x[i] = x

        Done by solving quadratic equation iteratively.
        x_1**2 + x_1 * (sum' - (A*n**n - 1) * D / (A * n**n)) = D ** (n + 1) / (n ** (2 * n) * prod' * A)
        x_1**2 + b*x_1 = c

        x_1 = (x_1**2 + c) / (2*x_1 + b)
        """

        # x in the input is converted to the same price/precision

        N_COINS = len(self.tokens)

        assert i != j, "same coin"
        assert j >= 0, "j below zero"
        assert j < N_COINS, "j above N_COINS"

        # should be unreachable, but good for safety
        assert i >= 0
        assert i < N_COINS

        if self.address in (
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
        ):
            if self.address in (
                "0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51",
                "0x52EA46506B9CC5Ef470C5bf89f17Dc28bB35D85C",
                "0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27",
                "0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56",
                "0xA5407eAE9Ba41422680e2e00537571bcC53efBfD",
            ):
                amp = self._A() // self.A_PRECISION
            else:
                amp = self._A()

            D = self._get_D(xp, amp)
            c = D
            S_ = 0
            Ann = amp * N_COINS

            for _i in range(N_COINS):
                if _i == i:
                    _x = x
                elif _i != j:
                    _x = xp[_i]
                else:
                    continue
                S_ += _x
                c = c * D // (_x * N_COINS)

            c = c * D // (Ann * N_COINS)
            b = S_ + D // Ann
            y = D
            for _ in range(255):
                y_prev = y
                y = (y * y + c) // (2 * y + b - D)
                if y > y_prev:
                    if y - y_prev <= 1:
                        return y
                else:
                    if y_prev - y <= 1:
                        return y

        else:
            amp = self._A()
            D = self._get_D(xp, amp)

            S_ = 0
            c = D
            Ann = amp * N_COINS

            for _i in range(N_COINS):
                if _i == i:
                    _x = x
                elif _i != j:
                    _x = xp[_i]
                else:
                    continue
                S_ += _x
                c = c * D // (_x * N_COINS)

            c = c * D * self.A_PRECISION // (Ann * N_COINS)
            b = S_ + D * self.A_PRECISION // Ann
            y = D
            for _ in range(255):
                y_prev = y
                y = (y * y + c) // (2 * y + b - D)
                if y > y_prev:
                    if y - y_prev <= 1:
                        return y
                else:
                    if y_prev - y <= 1:
                        return y

        raise EVMRevertError(
            f"_get_y() did not converge for pool {self.address}"
        )  # pragma: no cover

    def _get_y_D(self, A_: int, i: int, xp: List[int], D: int) -> int:
        """
        Calculate x[i] if one reduces D from being calculated for xp to D

        Done by solving quadratic equation iteratively.
        x_1**2 + x1 * (sum' - (A*n**n - 1) * D / (A * n**n)) = D ** (n + 1) / (n ** (2 * n) * prod' * A)
        x_1**2 + b*x_1 = c

        x_1 = (x_1**2 + c) / (2*x_1 + b)
        """

        if self.address in (
            "0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2",
            "0xf253f83AcA21aAbD2A20553AE0BF7F65C755A07F",
        ):
            """
            Calculate x[i] if one reduces D from being calculated for xp to D

            Done by solving quadratic equation iteratively.
            x_1**2 + x_1 * (sum' - (A*n**n - 1) * D / (A * n**n)) = D ** (n + 1) / (n ** (2 * n) * prod' * A)
            x_1**2 + b*x_1 = c

            x_1 = (x_1**2 + c) / (2*x_1 + b)
            """

            N_COINS = len(self.tokens)

            # x in the input is converted to the same price/precision

            assert i >= 0  # dev: i below zero
            assert i < N_COINS  # dev: i above N_COINS

            Ann = A_ * N_COINS
            c = D
            S = 0
            _x = 0
            y_prev = 0

            for _i in range(N_COINS):
                if _i != i:
                    _x = xp[_i]
                else:
                    continue
                S += _x
                c = c * D // (_x * N_COINS)
            b = S + D * self.A_PRECISION // Ann
            c = c * D * self.A_PRECISION // (Ann * N_COINS)
            y = D

            for _i in range(255):
                y_prev = y
                y = (y * y + c) // (2 * y + b - D)
                if y > y_prev:
                    if y - y_prev <= 1:
                        return y
                else:
                    if y_prev - y <= 1:
                        return y
            raise EVMRevertError(f"_get_y_D() failed to converge for pool {self.address}")

        else:
            # x in the input is converted to the same price/precision

            N_COINS = len(self.tokens)

            assert i >= 0  # dev: i below zero
            assert i < N_COINS  # dev: i above N_COINS

            c = D
            S_ = 0
            Ann = A_ * N_COINS

            _x = 0
            for _i in range(N_COINS):
                if _i != i:
                    _x = xp[_i]
                else:
                    continue
                S_ += _x
                c = c * D // (_x * N_COINS)
            c = c * D // (Ann * N_COINS)
            b = S_ + D // Ann
            y = D
            for _i in range(255):
                y_prev = y
                y = (y * y + c) // (2 * y + b - D)
                if y > y_prev:
                    if y - y_prev <= 1:
                        break
                else:
                    if y_prev - y <= 1:
                        break
            return y

    def _stored_rates_from_ctokens(self, block_number: int) -> List[int]:
        try:
            return self._cached_rates_from_ctokens[block_number]
        except KeyError:
            pass

        _w3 = config.get_web3()

        result = []
        for token, use_lending, multiplier in zip(
            self.tokens,
            self.use_lending,
            self.precision_multipliers,
        ):
            if not use_lending:
                rate = self.PRECISION
            else:
                rate, *_ = eth_abi.abi.decode(
                    types=["uint256"],
                    data=_w3.eth.call(
                        transaction={
                            "to": token.address,
                            "data": Web3.keccak(text="exchangeRateStored()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )
                supply_rate, *_ = eth_abi.abi.decode(
                    types=["uint256"],
                    data=_w3.eth.call(
                        transaction={
                            "to": token.address,
                            "data": Web3.keccak(text="supplyRatePerBlock()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )
                old_block, *_ = eth_abi.abi.decode(
                    types=["uint256"],
                    data=_w3.eth.call(
                        transaction={
                            "to": token.address,
                            "data": Web3.keccak(text="accrualBlockNumber()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                )

                rate += rate * supply_rate * (block_number - old_block) // self.PRECISION

            result.append(multiplier * rate)

        self._cached_rates_from_ctokens[block_number] = result
        return result

    def _stored_rates_from_ytokens(self, block_number: int) -> List[int]:
        try:
            return self._cached_rates_from_ytokens[block_number]
        except KeyError:
            pass

        _w3 = config.get_web3()

        # ref: https://etherscan.io/address/0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27#code

        result = []
        for token, multiplier, use_lending in zip(
            self.tokens,
            self.precision_multipliers,
            self.use_lending,
        ):
            if use_lending:
                rate, *_ = eth_abi.abi.decode(
                    types=["uint256"],
                    data=_w3.eth.call(
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

        self._cached_rates_from_ytokens[block_number] = result
        return result

    def _stored_rates_from_cytokens(self, block_number: int) -> List[int]:
        try:
            return self._cached_rates_from_cytokens[block_number]
        except KeyError:
            pass

        _w3 = config.get_web3()

        result = []
        for token, precision_multiplier in zip(self.tokens, self.precision_multipliers):
            rate, *_ = eth_abi.abi.decode(
                types=["uint256"],
                data=(
                    _w3.eth.call(
                        transaction={
                            "to": token.address,
                            "data": Web3.keccak(text="exchangeRateStored()")[:4],
                        },
                        block_identifier=block_number,
                    )
                ),
            )
            supply_rate, *_ = eth_abi.abi.decode(
                types=["uint256"],
                data=_w3.eth.call(
                    transaction={
                        "to": token.address,
                        "data": Web3.keccak(text="supplyRatePerBlock()")[:4],
                    },
                    block_identifier=block_number,
                ),
            )
            old_block, *_ = eth_abi.abi.decode(
                types=["uint256"],
                data=_w3.eth.call(
                    transaction={
                        "to": token.address,
                        "data": Web3.keccak(text="accrualBlockNumber()")[:4],
                    },
                    block_identifier=block_number,
                ),
            )

            rate += rate * supply_rate * (block_number - old_block) // self.PRECISION
            result.append(precision_multiplier * rate)

        self._cached_rates_from_cytokens[block_number] = result
        return result

    def _stored_rates_from_reth(self, block_number: int) -> List[int]:
        try:
            return [self.PRECISION, self._cached_rates_from_reth[block_number]]
        except KeyError:
            pass

        _w3 = config.get_web3()

        # ref: https://etherscan.io/address/0xF9440930043eb3997fc70e1339dBb11F341de7A8#code
        ratio, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=_w3.eth.call(
                transaction={
                    "to": self.tokens[1].address,
                    "data": Web3.keccak(text="getExchangeRate()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        self._cached_rates_from_reth[block_number] = ratio
        return [self.PRECISION, ratio]

    def _stored_rates_from_aeth(self, block_number: int) -> List[int]:
        try:
            return [
                self.PRECISION,
                self.PRECISION
                * self.LENDING_PRECISION
                // self._cached_rates_from_aeth[block_number],
            ]
        except KeyError:
            pass

        _w3 = config.get_web3()

        # ref: https://etherscan.io/address/0xA96A65c051bF88B4095Ee1f2451C2A9d43F53Ae2#code
        ratio, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=_w3.eth.call(
                transaction={
                    "to": self.tokens[1].address,
                    "data": Web3.keccak(text="ratio()")[:4],
                },
                block_identifier=block_number,
            ),
        )
        self._cached_rates_from_aeth[block_number] = ratio
        return [
            self.PRECISION,
            self.PRECISION * self.LENDING_PRECISION // ratio,
        ]

    def _stored_rates_from_oracle(self, block_number: int) -> List[int]:
        try:
            return self._cached_rates_from_oracle[block_number]
        except KeyError:
            pass

        _w3 = config.get_web3()

        # ref: https://etherscan.io/address/0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492#code
        ORACLE_BIT_MASK = (2**32 - 1) * 256**28

        rates = self.rate_multipliers
        oracle = self.oracle_method

        if oracle != 0:
            oracle_rate, *_ = eth_abi.abi.decode(
                types=["uint256"],
                data=_w3.eth.call(
                    transaction={
                        "to": to_checksum_address(HexBytes(oracle % 2**160)),
                        "data": HexBytes(oracle & ORACLE_BIT_MASK),
                    },
                    block_identifier=block_number,
                ),
            )
            rates = [rates[0], rates[1] * oracle_rate // self.PRECISION]

        self._cached_rates_from_oracle[block_number] = rates
        return rates

    def _xp(self, rates: List[int], balances: List[int]) -> List[int]:
        return [rate * balance // self.PRECISION for rate, balance in zip(rates, balances)]

    def auto_update(self, block_number: int | None = None) -> Tuple[bool, CurveStableswapPoolState]:
        """
        Retrieve updated balances from the contract
        """

        _w3 = config.get_web3()
        if block_number is None:
            block_number = _w3.eth.get_block_number()

        found_updates = False
        token_balances = []
        coin_index_type = self._coin_index_type
        for token_id, _ in enumerate(self.tokens):
            token_balance, *_ = eth_abi.abi.decode(
                types=[coin_index_type],
                data=_w3.eth.call(
                    transaction={
                        "to": self.address,
                        "data": Web3.keccak(text=f"balances({coin_index_type})")[:4]
                        + eth_abi.abi.encode(types=[coin_index_type], args=[token_id]),
                    },
                    block_identifier=block_number,
                ),
            )
            token_balances.append(token_balance)

        if self.is_metapool:
            self.base_pool.auto_update(block_number=block_number)
            if self.base_cache_updated is not None:
                self.base_cache_updated = self._get_base_cache_updated(block_number=block_number)

        if token_balances != self.balances:
            found_updates = True
            self.balances = token_balances

        self.update_block = block_number

        return found_updates, CurveStableswapPoolState(pool=self.address, balances=self.balances)

    # def external_update(self, update: CurveStableswapPoolExternalUpdate) -> bool:
    #     with self._state_lock:
    #         i = update.sold_id
    #         j = update.bought_id
    #         dx = update.tokens_sold
    #         dy_out = update.tokens_bought

    #         _xp = self._xp(rates=self.rate_multipliers, balances=self.balances)
    #         x = _xp[i] + dx * self.rate_multipliers[i] // self.PRECISION
    #         y = self._get_y(i, j, x, _xp)

    #         dy = _xp[j] - y - 1
    #         dy_fee = dy * self.fee // self.FEE_DENOMINATOR

    #         dy = (dy - dy_fee) * self.PRECISION // self.rate_multipliers[j]

    #         dy_admin_fee = dy_fee * self.admin_fee // self.FEE_DENOMINATOR
    #         dy_admin_fee = dy_admin_fee * self.PRECISION // self.rate_multipliers[j]

    #         assert dy == dy_out, f"Predicted output {dy} does not match update {dy_out}"

    #         self.balances[i] += dx
    #         self.balances[j] -= dy_out + dy_admin_fee

    #         if update.block_number:
    #             self.update_block = update.block_number

    #         self._update_pool_state()
    #         return True

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
            config.get_web3().eth.get_block_number()
            if block_identifier is None
            else get_number_for_block_identifier(block_identifier)
        )

        if token_in_quantity <= 0:
            raise ZeroSwapError("token_in_quantity must be positive")

        if override_state:
            logger.debug("Overrides applied:")
            logger.debug(f"Balances: {override_state.balances}")

        tokens_used_this_pool = [
            token_in in self.tokens,
            token_out in self.tokens,
        ]
        if self.is_metapool:
            tokens_used_in_base_pool = [
                token_in in self.base_pool.tokens,
                token_out in self.base_pool.tokens,
            ]

        if all(tokens_used_this_pool):
            if any([balance == 0 for balance in self.balances]):
                raise ZeroLiquidityError("One or more of the tokens has a zero balance.")

            return self._get_dy(
                i=self.tokens.index(token_in),
                j=self.tokens.index(token_out),
                dx=token_in_quantity,
                block_identifier=block_number,
                override_state=override_state,
            )
        elif any(tokens_used_this_pool) and self.is_metapool and any(tokens_used_in_base_pool):
            # TODO: see if any of these checks are unnecessary (partial zero balanece OK?)
            if any([balance == 0 for balance in self.base_pool.balances]):
                raise ZeroLiquidityError("One or more of the base pool tokens has a zero balance.")
            if any([balance == 0 for balance in self.balances]):
                raise ZeroLiquidityError("One or more of the tokens has a zero balance.")

            token_in_from_metapool = token_in in self.tokens
            token_out_from_metapool = token_out in self.tokens
            assert token_in_from_metapool or token_out_from_metapool

            if token_in_from_metapool and self.balances[self.tokens.index(token_in)] == 0:
                raise ZeroLiquidityError(f"{token_in} has a zero balance.")
            if token_out_from_metapool and self.balances[self.tokens.index(token_out)] == 0:
                raise ZeroLiquidityError(f"{token_out} has a zero balance.")

            token_in_from_basepool = token_in in self.base_pool.tokens
            token_out_from_basepool = token_out in self.base_pool.tokens
            assert token_in_from_basepool or token_out_from_basepool

            if (
                token_in_from_basepool
                and self.base_pool.balances[self.base_pool.tokens.index(token_in)] == 0
            ):
                raise ZeroLiquidityError(f"{token_in} has a zero balance.")
            if (
                token_out_from_basepool
                and self.base_pool.balances[self.base_pool.tokens.index(token_out)] == 0
            ):
                raise ZeroLiquidityError(f"{token_out} has a zero balance.")

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
        elif self.is_metapool and all(tokens_used_in_base_pool):
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
        else:
            raise ValueError(
                "Tokens not held by pool or in underlying base pool"
            )  # pragma: no cover
