from web3 import Web3
from web3.types import BlockIdentifier

from ..functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from ..uniswap.v2_liquidity_pool import UniswapV2Pool
from ..uniswap.v3_liquidity_pool import UniswapV3Pool


class PancakeV2Pool(UniswapV2Pool): ...


class PancakeV3Pool(UniswapV3Pool):
    def get_price_and_tick(
        self, w3: Web3, block_identifier: BlockIdentifier | None = None
    ) -> tuple[int, int]:
        price, tick, *_ = raw_call(
            w3=w3,
            address=self.address,
            calldata=encode_function_calldata(
                function_prototype="slot0()",
                function_arguments=None,
            ),
            return_types=[
                "uint160",
                "int24",
                "uint16",
                "uint16",
                "uint16",
                "uint32",
                "bool",
            ],
            block_identifier=get_number_for_block_identifier(block_identifier),
        )
        return price, tick
