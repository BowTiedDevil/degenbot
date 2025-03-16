from dataclasses import dataclass

from eth_typing import ChainId, ChecksumAddress

from degenbot.cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.types import AbstractExchangeDeployment


@dataclass(slots=True, frozen=True)
class UniswapFactoryDeployment:
    address: ChecksumAddress
    deployer: ChecksumAddress | None
    pool_init_hash: str


@dataclass(slots=True, frozen=True)
class UniswapV2ExchangeDeployment(AbstractExchangeDeployment):
    factory: UniswapFactoryDeployment


@dataclass(slots=True, frozen=True)
class UniswapV3ExchangeDeployment(AbstractExchangeDeployment):
    factory: UniswapFactoryDeployment


@dataclass(slots=True, frozen=True)
class UniswapRouterDeployment:
    address: ChecksumAddress
    chain_id: int
    name: str
    exchanges: list[UniswapV2ExchangeDeployment | UniswapV3ExchangeDeployment]


def register_exchange(exchange: UniswapV2ExchangeDeployment | UniswapV3ExchangeDeployment) -> None:
    if exchange.chain_id not in FACTORY_DEPLOYMENTS:
        FACTORY_DEPLOYMENTS[exchange.chain_id] = {}

    if exchange.factory.address in FACTORY_DEPLOYMENTS[exchange.chain_id]:
        raise DegenbotValueError(message="Exchange is already registered.")

    FACTORY_DEPLOYMENTS[exchange.chain_id][exchange.factory.address] = exchange.factory


def register_router(router: UniswapRouterDeployment) -> None:
    if router.chain_id not in ROUTER_DEPLOYMENTS:
        ROUTER_DEPLOYMENTS[router.chain_id] = {}

    if router.address in ROUTER_DEPLOYMENTS[router.chain_id]:
        raise DegenbotValueError(message="Router is already registered.")

    ROUTER_DEPLOYMENTS[router.chain_id][router.address] = router


# Mainnet DEX --------------- START
EthereumMainnetUniswapV2 = UniswapV2ExchangeDeployment(
    name="Ethereum Mainnet Uniswap V2",
    chain_id=ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
        deployer=None,
        pool_init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
    ),
)
EthereumMainnetSushiswapV2 = UniswapV2ExchangeDeployment(
    name="Ethereum Mainnet Sushiswap V2",
    chain_id=ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
        deployer=None,
        pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
    ),
)
EthereumMainnetUniswapV3 = UniswapV3ExchangeDeployment(
    name="Ethereum Mainnet Uniswap V3",
    chain_id=ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
EthereumMainnetSushiswapV3 = UniswapV3ExchangeDeployment(
    name="Ethereum Mainnet Sushiswap V3",
    chain_id=ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
# Mainnet DEX --------------- END


# Base DEX ---------------- START
BaseAerodromeV2 = UniswapV2ExchangeDeployment(
    name="Aerodrome V2",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da"),
        deployer=None,
        pool_init_hash="",
    ),
)
BaseAerodromeV3 = UniswapV3ExchangeDeployment(
    name="Base Aerodrome V3",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"),
        deployer=None,
        pool_init_hash="",
    ),
)
BasePancakeswapV2 = UniswapV2ExchangeDeployment(
    name="Pancakeswap V2",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E"),
        deployer=None,
        pool_init_hash="0x57224589c67f3f30a6b0d7a1b54cf3153ab84563bc609ef41dfb34f8b2974d2d",
    ),
)
BasePancakeswapV3 = UniswapV3ExchangeDeployment(
    name="Pancakeswap V3",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
        deployer=get_checksum_address("0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"),
        pool_init_hash="0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
    ),
)
BaseSushiswapV2 = UniswapV2ExchangeDeployment(
    name="Sushiswap V2",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x71524B4f93c58fcbF659783284E38825f0622859"),
        deployer=None,
        pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
    ),
)
BaseSushiswapV3 = UniswapV3ExchangeDeployment(
    name="Sushiswap V3",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0xc35DADB65012eC5796536bD9864eD8773aBc74C4"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
BaseSwapbasedV2 = UniswapV2ExchangeDeployment(
    name="Swapbased V2",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x04C9f118d21e8B767D2e50C946f0cC9F6C367300"),
        deployer=None,
        pool_init_hash="0xb64118b4e99d4a4163453838112a1695032df46c09f7f09064d4777d2767f8ea",
    ),
)
BaseUniswapV2 = UniswapV2ExchangeDeployment(
    name="Uniswap V2",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6"),
        deployer=None,
        pool_init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
    ),
)
BaseUniswapV3 = UniswapV3ExchangeDeployment(
    name="Uniswap V3",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x33128a8fC17869897dcE68Ed026d694621f6FDfD"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
# Base DEX -------------------- END


# Arbitrum DEX -------------- START
ArbitrumSushiswapV2 = UniswapV2ExchangeDeployment(
    name="Arbitrum Sushiswap V2",
    chain_id=ChainId.ARB1,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0xc35DADB65012eC5796536bD9864eD8773aBc74C4"),
        deployer=None,
        pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
    ),
)
ArbitrumUniswapV3 = UniswapV3ExchangeDeployment(
    name="Arbitrum Uniswap V3",
    chain_id=ChainId.ARB1,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
ArbitrumSushiswapV3 = UniswapV3ExchangeDeployment(
    name="Arbitrum Sushiswap V3",
    chain_id=ChainId.ARB1,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
# ----------------------------- END


# Mainnet Routers ----------- START
EthereumMainnetUniswapV2Router = UniswapRouterDeployment(
    address=get_checksum_address("0xf164fC0Ec4E93095b804a4795bBe1e041497b92a"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet UniswapV2 Router",
    exchanges=[EthereumMainnetUniswapV2],
)
EthereumMainnetUniswapV2Router2 = UniswapRouterDeployment(
    address=get_checksum_address("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet UniswapV2 Router 2",
    exchanges=[EthereumMainnetUniswapV2],
)
EthereumMainnetSushiswapV2Router = UniswapRouterDeployment(
    address=get_checksum_address("0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Sushiswap Router",
    exchanges=[EthereumMainnetSushiswapV2],
)
EthereumMainnetUniswapV3Router = UniswapRouterDeployment(
    address=get_checksum_address("0xE592427A0AEce92De3Edee1F18E0157C05861564"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap V3 Router",
    exchanges=[EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapV3Router2 = UniswapRouterDeployment(
    address=get_checksum_address("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap V3 Router2",
    exchanges=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapUniversalRouter = UniswapRouterDeployment(
    address=get_checksum_address("0xEf1c6E67703c7BD7107eed8303Fbe6EC2554BF6B"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap Universal Router",
    exchanges=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapUniversalRouterV1_2 = UniswapRouterDeployment(
    address=get_checksum_address("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap Universal Router V1_2",
    exchanges=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
EthereumMainnetUniswapUniversalRouterV1_3 = UniswapRouterDeployment(
    address=get_checksum_address("0x3F6328669a86bef431Dc6F9201A5B90F7975a023"),
    chain_id=ChainId.ETH,
    name="Ethereum Mainnet Uniswap Universal Router V1_3",
    exchanges=[EthereumMainnetUniswapV2, EthereumMainnetUniswapV3],
)
# Mainnet Routers ------------- END


# Base Routers -------------- START
BaseSushiswapRouter = UniswapRouterDeployment(
    address=get_checksum_address("0xFB7eF66a7e61224DD6FcD0D7d9C3be5C8B049b9f"),
    chain_id=ChainId.BASE,
    name="Sushiswap V3 SwapRouter",
    exchanges=[BaseSushiswapV3],
)
# Base Routers ---------------- END


# Arbitrum Routers ---------- START
ArbitrumSushiswapV2Router = UniswapRouterDeployment(
    address=get_checksum_address("0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Sushiswap Router",
    exchanges=[ArbitrumSushiswapV2],
)
ArbitrumUniswapV3Router = UniswapRouterDeployment(
    address=get_checksum_address("0xE592427A0AEce92De3Edee1F18E0157C05861564"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap V3 Router",
    exchanges=[ArbitrumUniswapV3],
)
ArbitrumUniswapV3Router2 = UniswapRouterDeployment(
    address=get_checksum_address("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap V3 Router2",
    exchanges=[ArbitrumUniswapV3],
)
ArbitrumUniswapUniversalRouter = UniswapRouterDeployment(
    address=get_checksum_address("0x4C60051384bd2d3C01bfc845Cf5F4b44bcbE9de5"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap Universal Router",
    exchanges=[ArbitrumUniswapV3],
)
ArbitrumUniswapUniversalRouter2 = UniswapRouterDeployment(
    address=get_checksum_address("0xeC8B0F7Ffe3ae75d7FfAb09429e3675bb63503e4"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap Universal Router V1_2",
    exchanges=[ArbitrumUniswapV3],
)
ArbitrumUniswapUniversalRouter3 = UniswapRouterDeployment(
    address=get_checksum_address("0x5E325eDA8064b456f4781070C0738d849c824258"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Uniswap Universal Router 4",
    exchanges=[ArbitrumUniswapV3],
)
ArbitrumSushiswapV3Router = UniswapRouterDeployment(
    address=get_checksum_address("0x8A21F6768C1f8075791D08546Dadf6daA0bE820c"),
    chain_id=ChainId.ARB1,
    name="Arbitrum Sushiswap V3 Router",
    exchanges=[ArbitrumSushiswapV3],
)
# ----------------------------- END


FACTORY_DEPLOYMENTS: dict[
    int,  # chain ID
    dict[ChecksumAddress, UniswapFactoryDeployment],
] = {
    ChainId.ETH: {
        EthereumMainnetSushiswapV2.factory.address: EthereumMainnetSushiswapV2.factory,
        EthereumMainnetSushiswapV3.factory.address: EthereumMainnetSushiswapV3.factory,
        EthereumMainnetUniswapV2.factory.address: EthereumMainnetUniswapV2.factory,
        EthereumMainnetUniswapV3.factory.address: EthereumMainnetUniswapV3.factory,
    },
    ChainId.BASE: {
        BaseAerodromeV2.factory.address: BaseAerodromeV2.factory,
        BaseAerodromeV3.factory.address: BaseAerodromeV3.factory,
        BasePancakeswapV2.factory.address: BasePancakeswapV2.factory,
        BasePancakeswapV3.factory.address: BasePancakeswapV3.factory,
        BaseSushiswapV2.factory.address: BaseSushiswapV2.factory,
        BaseSushiswapV3.factory.address: BaseSushiswapV3.factory,
        BaseSwapbasedV2.factory.address: BaseSwapbasedV2.factory,
        BaseUniswapV2.factory.address: BaseUniswapV2.factory,
        BaseUniswapV3.factory.address: BaseUniswapV3.factory,
    },
    ChainId.ARB1: {
        ArbitrumSushiswapV2.factory.address: ArbitrumSushiswapV2.factory,
        ArbitrumSushiswapV3.factory.address: ArbitrumSushiswapV3.factory,
        ArbitrumUniswapV3.factory.address: ArbitrumUniswapV3.factory,
    },
}


ROUTER_DEPLOYMENTS: dict[
    int,  # chain ID
    dict[
        ChecksumAddress,  # Router Address
        UniswapRouterDeployment,
    ],
] = {
    ChainId.ETH: {
        EthereumMainnetSushiswapV2Router.address: EthereumMainnetSushiswapV2Router,
        EthereumMainnetUniswapV2Router.address: EthereumMainnetUniswapV2Router,
        EthereumMainnetUniswapV2Router2.address: EthereumMainnetUniswapV2Router2,
        EthereumMainnetUniswapV3Router.address: EthereumMainnetUniswapV3Router,
        EthereumMainnetUniswapV3Router2.address: EthereumMainnetUniswapV3Router2,
        EthereumMainnetUniswapUniversalRouter.address: EthereumMainnetUniswapUniversalRouter,
        EthereumMainnetUniswapUniversalRouterV1_2.address: EthereumMainnetUniswapUniversalRouterV1_2,  # noqa: E501
        EthereumMainnetUniswapUniversalRouterV1_3.address: EthereumMainnetUniswapUniversalRouterV1_3,  # noqa: E501
    },
    ChainId.BASE: {
        BaseSushiswapRouter.address: BaseSushiswapRouter,
    },
    ChainId.ARB1: {
        ArbitrumUniswapUniversalRouter.address: ArbitrumUniswapUniversalRouter,
        ArbitrumUniswapUniversalRouter2.address: ArbitrumUniswapUniversalRouter2,
        ArbitrumUniswapUniversalRouter3.address: ArbitrumUniswapUniversalRouter3,
    },
}
