from collections.abc import Iterable
from fractions import Fraction
from threading import Lock
from typing import Any, Literal

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from eth_utils.crypto import keccak
from typing_extensions import override
from web3.contract.contract import Contract

from .. import config
from ..erc20_token import Erc20Token
from ..exceptions import (
    ExternalUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
    ZeroSwapError,
)
from ..exchanges.solidly.deployments import FACTORY_DEPLOYMENTS
from ..exchanges.solidly.types import SolidlyExchangeDeployment
from ..logging import logger
from ..manager.token_manager import Erc20TokenHelperManager
from ..registry.all_pools import AllPools
from ..types import AbstractLiquidityPool
from .abi import AERODROME_V2_FACTORY_ABI, AERODROME_V2_POOL_ABI
from .solidly_functions import (
    generate_aerodrome_v2_pool_address,
    solidly_calc_exact_in_stable,
    solidly_calc_exact_in_volatile,
)
from .types import (
    AerodromeV2PoolSimulationResult,
    AerodromeV2PoolState,
    AerodromeV2PoolStateUpdated,
)


class AerodromeV2LiquidityPool(AbstractLiquidityPool):
    def __init__(
        self,
        address: ChecksumAddress | str,
        tokens: list[Erc20Token] | None = None,
        abi: list[Any] | None = None,
        factory_address: str | None = None,
        deployer_address: str | None = None,
        fee: Fraction | None = None,
        silent: bool = False,
        archive_states: bool = True,
        verify_address: bool = True,
    ) -> None:
        self.address = to_checksum_address(address)
        self.abi = abi if abi is not None else AERODROME_V2_POOL_ABI

        w3 = config.get_web3()
        self.update_block = w3.eth.block_number
        chain_id = w3.eth.chain_id
        w3_contract = self.w3_contract

        self.factory = (
            to_checksum_address(factory_address)
            if factory_address is not None
            else w3_contract.functions.factory().call()
        )
        self.deployer_address = (
            to_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )
        self.stable = w3_contract.functions.stable().call()

        if fee is None:
            factory_contract = w3.eth.contract(address=self.factory, abi=AERODROME_V2_FACTORY_ABI)
            pool_fee = factory_contract.functions.getFee(self.address, self.stable).call()
            fee = Fraction(pool_fee, 10_000)
        self.fee = fee

        if tokens is not None:
            self.token0, self.token1 = sorted(tokens)
        else:
            _token_manager = Erc20TokenHelperManager(chain_id)
            self.token0 = _token_manager.get_erc20token(
                address=w3_contract.functions.token0().call(),
                silent=silent,
            )
            self.token1 = _token_manager.get_erc20token(
                address=w3_contract.functions.token1().call(),
                silent=silent,
            )
        self.tokens = (self.token0, self.token1)

        if verify_address:
            verified_address = self._verified_address()
            if verified_address != self.address:
                raise ValueError(
                    f"Pool address verification failed. Provided: {self.address}, expected: {verified_address}"  # noqa:E501
                )

        self.name = f"{self.token0}-{self.token1} (AerodromeV2, {100*self.fee.numerator/self.fee.denominator:.2f}%)"  # noqa:E501

        self.reserves_token0, self.reserves_token1, *_ = (
            self.w3_contract.functions.getReserves().call(block_identifier=self.update_block)
        )
        self._state = AerodromeV2PoolState(
            pool=self.address,
            reserves_token0=self.reserves_token0,
            reserves_token1=self.reserves_token1,
        )
        self._pool_state_archive = {self.update_block: self._state} if archive_states else None

        AllPools(chain_id)[self.address] = self

        self._subscribers = set()

        if not silent:  # pragma: no cover
            logger.info(self.name)
            logger.info(f"• Token 0: {self.token0} - Reserves: {self.reserves_token0}")
            logger.info(f"• Token 1: {self.token1} - Reserves: {self.reserves_token1}")

    def _verified_address(self) -> ChecksumAddress:
        # The implementation address is hard-coded into the contract
        implementation_address = to_checksum_address(
            config.get_web3().eth.get_code(self.address)[10:30]
        )

        return generate_aerodrome_v2_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self.token0.address, self.token1.address),
            implementation_address=to_checksum_address(implementation_address),
            stable=self.stable,
        )

    @property
    def w3_contract(self) -> Contract:
        return config.get_web3().eth.contract(address=self.address, abi=self.abi)

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: AerodromeV2PoolState | None = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        """

        if token_in not in self.tokens:
            raise ValueError("token_in not recognized.")

        TOKEN_IN: Literal[0, 1] = 0 if token_in == self.token0 else 1

        if token_in_quantity <= 0:  # pragma: no cover
            raise ZeroSwapError("token_in_quantity must be positive")

        if override_state:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        reserves_0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        if self.stable:
            return solidly_calc_exact_in_stable(
                amount_in=token_in_quantity,
                token_in=TOKEN_IN,
                reserves0=reserves_0,
                reserves1=reserves_1,
                decimals0=10**self.token0.decimals,
                decimals1=10**self.token1.decimals,
                fee=self.fee,
            )
        else:
            return solidly_calc_exact_in_volatile(
                amount_in=token_in_quantity,
                token_in=TOKEN_IN,
                reserves0=reserves_0,
                reserves1=reserves_1,
                fee=self.fee,
            )
