from dataclasses import dataclass

import eth_typing
from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.types.abstract import AbstractExchangeDeployment


@dataclass(slots=True, frozen=True)
class UniswapFactoryDeployment:
    address: ChecksumAddress
    deployer: ChecksumAddress | None
    pool_init_hash: str


@dataclass(slots=True, frozen=True)
class UniswapPoolManagerDeployment:
    address: ChecksumAddress


@dataclass(slots=True, frozen=True)
class UniswapStateViewDeployment:
    address: ChecksumAddress


@dataclass(slots=True, frozen=True)
class UniswapV2ExchangeDeployment(AbstractExchangeDeployment):
    factory: UniswapFactoryDeployment


@dataclass(slots=True, frozen=True)
class UniswapV3ExchangeDeployment(AbstractExchangeDeployment):
    factory: UniswapFactoryDeployment


@dataclass(slots=True, frozen=True)
class UniswapV4ExchangeDeployment(AbstractExchangeDeployment):
    pool_manager: UniswapPoolManagerDeployment
    state_view: UniswapStateViewDeployment


def register_exchange(exchange: UniswapV2ExchangeDeployment | UniswapV3ExchangeDeployment) -> None:
    if exchange.chain_id not in FACTORY_DEPLOYMENTS:
        FACTORY_DEPLOYMENTS[exchange.chain_id] = {}

    if exchange.factory.address in FACTORY_DEPLOYMENTS[exchange.chain_id]:
        raise DegenbotValueError(message="Exchange is already registered.")

    FACTORY_DEPLOYMENTS[exchange.chain_id][exchange.factory.address] = exchange.factory


# Mainnet DEX --------------- START
EthereumMainnetPancakeswapV2 = UniswapV2ExchangeDeployment(
    name="Ethereum Mainnet Pancakeswap V2",
    chain_id=eth_typing.ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x1097053Fd2ea711dad45caCcc45EfF7548fCB362"),
        deployer=None,
        pool_init_hash="0x57224589c67f3f30a6b0d7a1b54cf3153ab84563bc609ef41dfb34f8b2974d2d",
    ),
)
EthereumMainnetPancakeswapV3 = UniswapV3ExchangeDeployment(
    name="Ethereum Mainnet Pancakeswap V3",
    chain_id=eth_typing.ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
        deployer=get_checksum_address("0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"),
        pool_init_hash="0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
    ),
)
EthereumMainnetSushiswapV2 = UniswapV2ExchangeDeployment(
    name="Ethereum Mainnet Sushiswap V2",
    chain_id=eth_typing.ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
        deployer=None,
        pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
    ),
)
EthereumMainnetSushiswapV3 = UniswapV3ExchangeDeployment(
    name="Ethereum Mainnet Sushiswap V3",
    chain_id=eth_typing.ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
EthereumMainnetUniswapV2 = UniswapV2ExchangeDeployment(
    name="Ethereum Mainnet Uniswap V2",
    chain_id=eth_typing.ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
        deployer=None,
        pool_init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
    ),
)
EthereumMainnetUniswapV3 = UniswapV3ExchangeDeployment(
    name="Ethereum Mainnet Uniswap V3",
    chain_id=eth_typing.ChainId.ETH,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
EthereumMainnetUniswapV4 = UniswapV4ExchangeDeployment(
    name="Ethereum Mainnet Uniswap V4",
    chain_id=eth_typing.ChainId.ETH,
    pool_manager=UniswapPoolManagerDeployment(
        address=get_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90"),
    ),
    state_view=UniswapStateViewDeployment(
        address=get_checksum_address("0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227")
    ),
)
# Mainnet DEX --------------- END


# Base DEX ---------------- START
BaseAerodromeV2 = UniswapV2ExchangeDeployment(
    name="Aerodrome V2",
    chain_id=eth_typing.ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da"),
        deployer=None,
        pool_init_hash="",
    ),
)
BaseAerodromeV3 = UniswapV3ExchangeDeployment(
    name="Base Aerodrome V3",
    chain_id=eth_typing.ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"),
        deployer=None,
        pool_init_hash="",
    ),
)
BasePancakeswapV2 = UniswapV2ExchangeDeployment(
    name="Pancakeswap V2",
    chain_id=eth_typing.ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E"),
        deployer=None,
        pool_init_hash="0x57224589c67f3f30a6b0d7a1b54cf3153ab84563bc609ef41dfb34f8b2974d2d",
    ),
)
BasePancakeswapV3 = UniswapV3ExchangeDeployment(
    name="Pancakeswap V3",
    chain_id=eth_typing.ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
        deployer=get_checksum_address("0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"),
        pool_init_hash="0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
    ),
)
BaseSushiswapV2 = UniswapV2ExchangeDeployment(
    name="Sushiswap V2",
    chain_id=eth_typing.ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x71524B4f93c58fcbF659783284E38825f0622859"),
        deployer=None,
        pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
    ),
)
BaseSushiswapV3 = UniswapV3ExchangeDeployment(
    name="Sushiswap V3",
    chain_id=eth_typing.ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0xc35DADB65012eC5796536bD9864eD8773aBc74C4"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
BaseSwapbasedV2 = UniswapV2ExchangeDeployment(
    name="Swapbased V2",
    chain_id=eth_typing.ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x04C9f118d21e8B767D2e50C946f0cC9F6C367300"),
        deployer=None,
        pool_init_hash="0xb64118b4e99d4a4163453838112a1695032df46c09f7f09064d4777d2767f8ea",
    ),
)
BaseUniswapV2 = UniswapV2ExchangeDeployment(
    name="Uniswap V2",
    chain_id=eth_typing.ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6"),
        deployer=None,
        pool_init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
    ),
)
BaseUniswapV3 = UniswapV3ExchangeDeployment(
    name="Uniswap V3",
    chain_id=eth_typing.ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x33128a8fC17869897dcE68Ed026d694621f6FDfD"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
BaseUniswapV4 = UniswapV4ExchangeDeployment(
    name="Uniswap V4",
    chain_id=eth_typing.ChainId.BASE,
    pool_manager=UniswapPoolManagerDeployment(
        address=get_checksum_address("0x498581fF718922c3f8e6A244956aF099B2652b2b"),
    ),
    state_view=UniswapStateViewDeployment(
        address=get_checksum_address("0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71")
    ),
)
# Base DEX -------------------- END


# Arbitrum DEX -------------- START
ArbitrumSushiswapV2 = UniswapV2ExchangeDeployment(
    name="Arbitrum Sushiswap V2",
    chain_id=eth_typing.ChainId.ARB1,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0xc35DADB65012eC5796536bD9864eD8773aBc74C4"),
        deployer=None,
        pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
    ),
)
ArbitrumUniswapV3 = UniswapV3ExchangeDeployment(
    name="Arbitrum Uniswap V3",
    chain_id=eth_typing.ChainId.ARB1,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
ArbitrumSushiswapV3 = UniswapV3ExchangeDeployment(
    name="Arbitrum Sushiswap V3",
    chain_id=eth_typing.ChainId.ARB1,
    factory=UniswapFactoryDeployment(
        address=get_checksum_address("0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"),
        deployer=None,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    ),
)
# ----------------------------- END


FACTORY_DEPLOYMENTS: dict[
    int,  # chain ID
    dict[ChecksumAddress, UniswapFactoryDeployment],
] = {
    eth_typing.ChainId.ETH: {
        EthereumMainnetSushiswapV2.factory.address: EthereumMainnetSushiswapV2.factory,
        EthereumMainnetSushiswapV3.factory.address: EthereumMainnetSushiswapV3.factory,
        EthereumMainnetUniswapV2.factory.address: EthereumMainnetUniswapV2.factory,
        EthereumMainnetUniswapV3.factory.address: EthereumMainnetUniswapV3.factory,
    },
    eth_typing.ChainId.BASE: {
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
    eth_typing.ChainId.ARB1: {
        ArbitrumSushiswapV2.factory.address: ArbitrumSushiswapV2.factory,
        ArbitrumSushiswapV3.factory.address: ArbitrumSushiswapV3.factory,
        ArbitrumUniswapV3.factory.address: ArbitrumUniswapV3.factory,
    },
}
