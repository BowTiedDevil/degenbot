from collections.abc import Iterable
from fractions import Fraction
from threading import Lock
from typing import Any, Literal

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from typing_extensions import override
from web3.contract.contract import Contract

from degenbot.exchanges.solidly.deployments import FACTORY_DEPLOYMENTS
from degenbot.exchanges.solidly.types import SolidlyExchangeDeployment

from .. import config
from ..erc20_token import Erc20Token
from ..exceptions import (
    ExternalUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
    ZeroSwapError,
)
from ..logging import logger
from ..manager.token_manager import Erc20TokenHelperManager
from ..registry.all_pools import AllPools
from ..types import AbstractLiquidityPool
from ..uniswap.v2_functions import (
    constant_product_calc_exact_in,
    constant_product_calc_exact_out,
)
from .abi import AERODROME_V2_POOL_ABI
from .solidly_functions import generate_aerodrome_v2_pool_address


class SolidlyV2LiquidityPool(AbstractLiquidityPool):
    def __init__(
        self,
        address: ChecksumAddress | str,
        tokens: list[Erc20Token] | None = None,
        abi: list[Any] | None = None,
        factory_address: str | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        silent: bool = False,
        verify_address: bool = True,
    ) -> None:
        self.address = to_checksum_address(address)

        self.address: ChecksumAddress = to_checksum_address(address)
        self.abi = abi if abi is not None else AERODROME_V2_POOL_ABI

        w3 = config.get_web3()
        w3_contract = self.w3_contract
        chain_id = w3.eth.chain_id

        self.factory = (
            to_checksum_address(factory_address)
            if factory_address is not None
            else w3_contract.functions.factory().call()
        )
        self.deployer_address = (
            to_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )
        self.stable = w3_contract.functions.stable().call()

        if init_hash is not None:
            self.init_hash = init_hash
        else:
            try:
                self.init_hash = FACTORY_DEPLOYMENTS[chain_id][self.factory].pool_init_hash
            except KeyError:
                self.init_hash = UNISWAP_V2_MAINNET_POOL_INIT_HASH

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

    def _verified_address(self) -> ChecksumAddress:
        return generate_aerodrome_v2_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self.token0.address, self.token1.address),
            implementation_address=to_checksum_address(
                "0xA4e46b4f701c62e14DF11B48dCe76A7d793CD6d7"
            ),
            stable=self.stable,
        )

    @property
    def w3_contract(self) -> Contract:
        return config.get_web3().eth.contract(address=self.address, abi=self.abi)
