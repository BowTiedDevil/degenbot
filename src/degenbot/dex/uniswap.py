from typing import Any, Dict

from eth_typing import ChainId, ChecksumAddress, HexStr
from eth_utils.address import to_checksum_address

from .baseclasses import UniswapFactory, UniswapRouter, UniswapV2Dex, UniswapV3Dex

# Mainnet DEX
EthereumMainnetUniswapV2 = UniswapV2Dex(
    name="Ethereum Mainnet Uniswap V2",
    chain=ChainId.ETH,
    factory=UniswapFactory(
        address=to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
        pool_init_hash=HexStr("0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"),
    ),
)
EthereumMainnetSushiswapV2 = UniswapV2Dex(
    name="Ethereum Mainnet Sushiswap V2",
    chain=ChainId.ETH,
    factory=UniswapFactory(
        address=to_checksum_address("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
        pool_init_hash=HexStr("0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"),
    ),
)
EthereumMainnetUniswapV3 = UniswapV3Dex(
    name="Ethereum Mainnet Uniswap V3",
    chain=ChainId.ETH,
    factory=UniswapFactory(
        address=to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
        pool_init_hash=HexStr("0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"),
    ),
    tick_lens=to_checksum_address("0xbfd8137f7d1516D3ea5cA83523914859ec47F573"),
)
EthereumMainnetSushiswapV3 = UniswapV3Dex(
    name="Ethereum Mainnet Sushiswap V3",
    chain=ChainId.ETH,
    factory=UniswapFactory(
        address=to_checksum_address("0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"),
        pool_init_hash=HexStr("0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"),
    ),
    tick_lens=to_checksum_address("0xFB70AD5a200d784E7901230E6875d91d5Fa6B68c"),
)

# Mainnet TickLens Contracts
TICKLENS_ADDRESSES: Dict[
    int,  # Chain ID
    Dict[
        ChecksumAddress,  # Factory address
        ChecksumAddress,  # TickLens address
    ],
] = {
    ChainId.ETH: {
        EthereumMainnetUniswapV3.factory.address: EthereumMainnetUniswapV3.tick_lens,
        EthereumMainnetSushiswapV3.factory.address: EthereumMainnetSushiswapV3.tick_lens,
    }
}

# Mainnet Routers
EthereumMainnetUniswapV2Router = UniswapRouter(
    address=to_checksum_address("0xf164fC0Ec4E93095b804a4795bBe1e041497b92a"),
    name="Ethereum Mainnet UniswapV2 Router",
    dex=[EthereumMainnetUniswapV2],
)
EthereumMainnetUniswapV2Router2 = UniswapRouter(
    address=to_checksum_address("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"),
    name="Ethereum Mainnet UniswapV2 Router 2",
    dex=[EthereumMainnetUniswapV2],
)
EthereumMainnetSushiswapV2Router = UniswapRouter(
    address=to_checksum_address("0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F"),
    name="Ethereum Mainnet Sushiswap Router",
    dex=[EthereumMainnetSushiswapV2],
)
EthereumMainnetUniswapV3Router = UniswapRouter(
    address=to_checksum_address("0xE592427A0AEce92De3Edee1F18E0157C05861564"),
    name="Ethereum Mainnet Uniswap V3 Router",
    dex=[EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapV3Router2 = UniswapRouter(
    address=to_checksum_address("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"),
    name="Ethereum Mainnet Uniswap V3 Router2",
    dex=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetSushiswapV3Router = UniswapRouter(
    address=to_checksum_address("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"),
    name="Ethereum Mainnet Uniswap V3 Router2",
    dex=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapUniversalRouter = UniswapRouter(
    address=to_checksum_address("0xEf1c6E67703c7BD7107eed8303Fbe6EC2554BF6B"),
    name="Ethereum Mainnet Uniswap Universal Router",
    dex=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapUniversalRouter = UniswapRouter(
    address=to_checksum_address("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD"),
    name="Ethereum Mainnet Uniswap Universal Router V1_2",
    dex=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapUniversalRouter = UniswapRouter(
    address=to_checksum_address("0x3F6328669a86bef431Dc6F9201A5B90F7975a023"),
    name="Ethereum Mainnet Uniswap Universal Router V1_3",
    dex=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)


# _ROUTERS = {
#     ChainId.ETH: {
#         to_checksum_address("0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F"): {
#             "name": "Sushiswap: Router",
#             "factory_address": {
#                 2: to_checksum_address("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac")
#             },
#         },
#         to_checksum_address("0xf164fC0Ec4E93095b804a4795bBe1e041497b92a"): {
#             "name": "UniswapV2: Router",
#             "factory_address": {
#                 2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
#             },
#         },
#         to_checksum_address("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"): {
#             "name": "UniswapV2: Router 2",
#             "factory_address": {
#                 2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
#             },
#         },
#         to_checksum_address("0xE592427A0AEce92De3Edee1F18E0157C05861564"): {
#             "name": "UniswapV3: Router",
#             "factory_address": {
#                 3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")
#             },
#         },
#         to_checksum_address("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"): {
#             "name": "UniswapV3: Router 2",
#             "factory_address": {
#                 2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
#                 3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
#             },
#         },
#         to_checksum_address("0xEf1c6E67703c7BD7107eed8303Fbe6EC2554BF6B"): {
#             "name": "Uniswap Universal Router",
#             "factory_address": {
#                 2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
#                 3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
#             },
#         },
#         to_checksum_address("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD"): {
#             "name": "Universal Universal Router (V1_2)",
#             "factory_address": {
#                 2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
#                 3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
#             },
#         },
#         to_checksum_address("0x3F6328669a86bef431Dc6F9201A5B90F7975a023"): {
#             "name": "Universal Universal Router (V1_3)",
#             "factory_address": {
#                 2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
#                 3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
#             },
#         },
#     }
# }


FACTORY_ADDRESSES: Dict[
    int,  # Chain ID
    Dict[
        ChecksumAddress,  # Factory address
        Dict[str, Any],
    ],
] = {
    ChainId.ETH: {
        EthereumMainnetUniswapV2.factory.address: {
            "init_hash": EthereumMainnetUniswapV2.factory.pool_init_hash
        },
        EthereumMainnetUniswapV3.factory.address: {
            "init_hash": EthereumMainnetUniswapV3.factory.pool_init_hash
        },
        EthereumMainnetSushiswapV2.factory.address: {
            "init_hash": EthereumMainnetSushiswapV2.factory.pool_init_hash
        },
        EthereumMainnetSushiswapV3.factory.address: {
            "init_hash": EthereumMainnetSushiswapV3.factory.pool_init_hash
        },
    }
}


# FACTORY_ADDRESSES: Dict[
#     int,  # Chain ID
#     Dict[
#         ChecksumAddress,  # Factory address
#         Dict[str, Any],
#     ],
# ] = {
#     1: {
#         # Uniswap (V2)
#         to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"): {
#             "init_hash": "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
#         },
#         # Uniswap (V3)
#         to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"): {
#             "init_hash": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
#         },
#         # Sushiswap (V2)
#         to_checksum_address("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"): {
#             "init_hash": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"
#         },
#         # Sushiswap (V3)
#         to_checksum_address("0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"): {
#             "init_hash": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
#         },
#     },
#     42161: {
#         # Uniswap (V3)
#         to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"): {
#             "init_hash": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
#         },
#         # Sushiswap (V2)
#         to_checksum_address("0xc35DADB65012eC5796536bD9864eD8773aBc74C4"): {
#             "init_hash": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"
#         },
#         # Sushiswap (V3)
#         to_checksum_address("0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"): {
#             "init_hash": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
#         },
#     },
# }


# TICKLENS_ADDRESSES: Dict[
#     int,  # Chain ID
#     Dict[
#         ChecksumAddress,  # Factory address
#         ChecksumAddress,  # TickLens address
#     ],
# ] = {
#     # Ethereum Mainnet
#     1: {
#         # Uniswap V3
#         # ref: https://docs.uniswap.org/contracts/v3/reference/deployments
#         to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"): to_checksum_address(
#             "0xbfd8137f7d1516D3ea5cA83523914859ec47F573"
#         ),
#         # Sushiswap V3
#         # ref: https://docs.sushi.com/docs/Products/V3%20AMM/Periphery/Deployment%20Addresses
#         to_checksum_address("0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"): to_checksum_address(
#             "0xFB70AD5a200d784E7901230E6875d91d5Fa6B68c"
#         ),
#     },
#     # Arbitrum
#     42161: {
#         # Uniswap V3
#         # ref: https://docs.uniswap.org/contracts/v3/reference/deployments
#         to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"): to_checksum_address(
#             "0xbfd8137f7d1516D3ea5cA83523914859ec47F573"
#         ),
#         # Sushiswap V3
#         # ref: https://docs.sushi.com/docs/Products/V3%20AMM/Periphery/Deployment%20Addresses
#         to_checksum_address("0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"): to_checksum_address(
#             "0x8516944E89f296eb6473d79aED1Ba12088016c9e"
#         ),
#     },
# }
