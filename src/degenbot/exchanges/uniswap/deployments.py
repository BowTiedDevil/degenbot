from typing import Dict

from eth_typing import ChainId, ChecksumAddress, HexStr
from eth_utils.address import to_checksum_address

from .dataclasses import (
    UniswapFactoryDeployment,
    UniswapRouterDeployment,
    UniswapTickLensDeployment,
    UniswapV2ExchangeDeployment,
    UniswapV3ExchangeDeployment,
)

# Mainnet DEX --------------- START
# ---------------------------------

# Uniswap V2 and forks
EthereumMainnetUniswapV2 = UniswapV2ExchangeDeployment(
    name="Ethereum Mainnet Uniswap V2",
    chain_id=ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
        pool_init_hash=HexStr("0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"),
    ),
)
EthereumMainnetSushiswapV2 = UniswapV2ExchangeDeployment(
    name="Ethereum Mainnet Sushiswap V2",
    chain_id=ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=to_checksum_address("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
        pool_init_hash=HexStr("0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"),
    ),
)

# Uniswap V3 and forks
EthereumMainnetUniswapV3 = UniswapV3ExchangeDeployment(
    name="Ethereum Mainnet Uniswap V3",
    chain_id=ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
        pool_init_hash=HexStr("0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"),
    ),
    tick_lens=UniswapTickLensDeployment(
        to_checksum_address("0xbfd8137f7d1516D3ea5cA83523914859ec47F573")
    ),
)
EthereumMainnetSushiswapV3 = UniswapV3ExchangeDeployment(
    name="Ethereum Mainnet Sushiswap V3",
    chain_id=ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=to_checksum_address("0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"),
        pool_init_hash=HexStr("0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"),
    ),
    tick_lens=UniswapTickLensDeployment(
        to_checksum_address("0xFB70AD5a200d784E7901230E6875d91d5Fa6B68c")
    ),
)
# ----------------------------- END


# Arbitrum DEX -------------- START
# ---------------------------------

# Uniswap V2 and forks
ArbitrumSushiswapV2 = UniswapV2ExchangeDeployment(
    name="Arbitrum Sushiswap V2",
    chain_id=ChainId.ARB1,
    factory=UniswapFactoryDeployment(
        address=to_checksum_address("0xc35DADB65012eC5796536bD9864eD8773aBc74C4"),
        pool_init_hash=HexStr("0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"),
    ),
)

# Uniswap V3 and forks
ArbitrumUniswapV3 = UniswapV3ExchangeDeployment(
    name="Arbitrum Uniswap V3",
    chain_id=ChainId.ARB1,
    factory=UniswapFactoryDeployment(
        address=to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
        pool_init_hash=HexStr("0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"),
    ),
    tick_lens=UniswapTickLensDeployment(
        to_checksum_address("0xbfd8137f7d1516D3ea5cA83523914859ec47F573")
    ),
)
ArbitrumSushiswapV3 = UniswapV3ExchangeDeployment(
    name="Arbitrum Sushiswap V3",
    chain_id=ChainId.ARB1,
    factory=UniswapFactoryDeployment(
        address=to_checksum_address("0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"),
        pool_init_hash=HexStr("0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"),
    ),
    tick_lens=UniswapTickLensDeployment(
        to_checksum_address("0x8516944E89f296eb6473d79aED1Ba12088016c9e")
    ),
)
# ----------------------------- END


# Mainnet Routers ----------- START
# ---------------------------------

# Uniswap V2-only
EthereumMainnetUniswapV2Router = UniswapRouterDeployment(
    address=to_checksum_address("0xf164fC0Ec4E93095b804a4795bBe1e041497b92a"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet UniswapV2 Router",
    exchanges=[EthereumMainnetUniswapV2],
)
EthereumMainnetUniswapV2Router2 = UniswapRouterDeployment(
    address=to_checksum_address("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet UniswapV2 Router 2",
    exchanges=[EthereumMainnetUniswapV2],
)
EthereumMainnetSushiswapV2Router = UniswapRouterDeployment(
    address=to_checksum_address("0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Sushiswap Router",
    exchanges=[EthereumMainnetSushiswapV2],
)

# Uniswap V3-only
EthereumMainnetUniswapV3Router = UniswapRouterDeployment(
    address=to_checksum_address("0xE592427A0AEce92De3Edee1F18E0157C05861564"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap V3 Router",
    exchanges=[EthereumMainnetUniswapV3],
)

# Uniswap V2 and V3
EthereumMainnetUniswapV3Router2 = UniswapRouterDeployment(
    address=to_checksum_address("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap V3 Router2",
    exchanges=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapUniversalRouter = UniswapRouterDeployment(
    address=to_checksum_address("0xEf1c6E67703c7BD7107eed8303Fbe6EC2554BF6B"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap Universal Router",
    exchanges=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapUniversalRouterV1_2 = UniswapRouterDeployment(
    address=to_checksum_address("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap Universal Router V1_2",
    exchanges=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapUniversalRouterV1_3 = UniswapRouterDeployment(
    address=to_checksum_address("0x3F6328669a86bef431Dc6F9201A5B90F7975a023"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap Universal Router V1_3",
    exchanges=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
# ----------------------------- END


# Arbitrum Routers ---------- START
# ---------------------------------

# Uniswap V2-only
ArbitrumSushiswapV2Router = UniswapRouterDeployment(
    address=to_checksum_address("0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Sushiswap Router",
    exchanges=[ArbitrumSushiswapV2],
)

# Uniswap V3-only
ArbitrumUniswapV3Router = UniswapRouterDeployment(
    address=to_checksum_address("0xE592427A0AEce92De3Edee1F18E0157C05861564"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap V3 Router",
    exchanges=[ArbitrumUniswapV3],
)

# Uniswap V2 and V3
ArbitrumUniswapV3Router2 = UniswapRouterDeployment(
    address=to_checksum_address("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap V3 Router2",
    exchanges=[ArbitrumUniswapV3],
)
ArbitrumUniswapUniversalRouter = UniswapRouterDeployment(
    address=to_checksum_address("0x4C60051384bd2d3C01bfc845Cf5F4b44bcbE9de5"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap Universal Router",
    exchanges=[ArbitrumUniswapV3],
)
ArbitrumUniswapUniversalRouter2 = UniswapRouterDeployment(
    address=to_checksum_address("0xeC8B0F7Ffe3ae75d7FfAb09429e3675bb63503e4"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap Universal Router V1_2",
    exchanges=[ArbitrumUniswapV3],
)
ArbitrumUniswapUniversalRouter3 = UniswapRouterDeployment(
    address=to_checksum_address("0x5E325eDA8064b456f4781070C0738d849c824258"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap Universal Router 4",
    exchanges=[ArbitrumUniswapV3],
)
ArbitrumSushiswapV3Router = UniswapRouterDeployment(
    address=to_checksum_address("0x8A21F6768C1f8075791D08546Dadf6daA0bE820c"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Sushiswap V3 Router",
    exchanges=[ArbitrumSushiswapV3],
)
# ----------------------------- END


FACTORY_DEPLOYMENTS: Dict[
    int,  # chain ID
    Dict[ChecksumAddress, UniswapFactoryDeployment],
] = {
    ChainId.ETH: {
        EthereumMainnetUniswapV2.factory.address: EthereumMainnetUniswapV2.factory,
        EthereumMainnetUniswapV3.factory.address: EthereumMainnetUniswapV3.factory,
        EthereumMainnetSushiswapV2.factory.address: EthereumMainnetSushiswapV2.factory,
        EthereumMainnetSushiswapV3.factory.address: EthereumMainnetSushiswapV3.factory,
    },
    ChainId.ARB1: {
        ArbitrumUniswapV3.factory.address: ArbitrumUniswapV3.factory,
        ArbitrumSushiswapV2.factory.address: ArbitrumSushiswapV2.factory,
        ArbitrumSushiswapV3.factory.address: ArbitrumSushiswapV3.factory,
    },
}


ROUTER_DEPLOYMENTS: Dict[
    int,  # chain ID
    Dict[
        ChecksumAddress,  # Router Address
        UniswapRouterDeployment,
    ],
] = {
    ChainId.ETH: {
        EthereumMainnetUniswapV2Router.address: EthereumMainnetUniswapV2Router,
        EthereumMainnetUniswapV2Router2.address: EthereumMainnetUniswapV2Router2,
        EthereumMainnetUniswapV3Router.address: EthereumMainnetUniswapV3Router,
        EthereumMainnetUniswapV3Router2.address: EthereumMainnetUniswapV3Router2,
        EthereumMainnetUniswapUniversalRouter.address: EthereumMainnetUniswapUniversalRouter,
        EthereumMainnetUniswapUniversalRouterV1_2.address: EthereumMainnetUniswapUniversalRouterV1_2,
        EthereumMainnetUniswapUniversalRouterV1_3.address: EthereumMainnetUniswapUniversalRouterV1_3,
        EthereumMainnetSushiswapV2Router.address: EthereumMainnetSushiswapV2Router,
    },
    ChainId.ARB1: {
        ArbitrumUniswapUniversalRouter.address: ArbitrumUniswapUniversalRouter,
        ArbitrumUniswapUniversalRouter2.address: ArbitrumUniswapUniversalRouter2,
        ArbitrumUniswapUniversalRouter3.address: ArbitrumUniswapUniversalRouter3,
    },
}


TICKLENS_DEPLOYMENTS: Dict[
    int,  # Chain ID
    Dict[
        ChecksumAddress,  # Factory address
        UniswapTickLensDeployment,
    ],
] = {
    ChainId.ETH: {
        EthereumMainnetUniswapV3.factory.address: EthereumMainnetUniswapV3.tick_lens,
        EthereumMainnetSushiswapV3.factory.address: EthereumMainnetSushiswapV3.tick_lens,
    },
    ChainId.ARB1: {
        ArbitrumUniswapV3.factory.address: ArbitrumUniswapV3.tick_lens,
        ArbitrumSushiswapV3.factory.address: ArbitrumSushiswapV3.tick_lens,
    },
}
