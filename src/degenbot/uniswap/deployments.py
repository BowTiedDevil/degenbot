"""Uniswap V2/V3/V4 deployment addresses and chain configuration.

The factory ``address`` / ``deployer`` / ``pool_init_hash`` for every V2/V3
``*ExchangeDeployment`` are the single source of truth in
``registry/deployments.json`` (verified on-chain by the cast harness in
``tests/registry/test_deployment_onchain_verification.py``, and cross-checked
against these constants by ``tests/registry/test_deployments_single_source.py``).
The V2/V3 constants below are *built* from the loaded ``DeploymentRecord`` via
:func:`_v2` / :func:`_v3`, so the deployer + init_hash can never drift from the
JSON — only the factory address is a literal here, and it doubles as the
``(chain_id, factory)`` lookup key. A factory address absent from the JSON raises
``KeyError`` at import (fail-loud) rather than shipping a stray constant.

V4 ``pool_manager`` / ``state_view`` are singleton addresses with no factory path
in the registry yet; they remain literals pending a V4 deployment-row epic.
"""

from dataclasses import dataclass

import eth_typing
from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.registry.deployment_loader import DeploymentRecord, load_deployments
from degenbot.types.abstract import AbstractExchangeDeployment


@dataclass(slots=True, frozen=True)
class UniswapFactoryDeployment:
    """UniswapFactoryDeployment class."""

    address: ChecksumAddress
    deployer: ChecksumAddress | None
    pool_init_hash: str


@dataclass(slots=True, frozen=True)
class UniswapPoolManagerDeployment:
    """UniswapPoolManagerDeployment class."""

    address: ChecksumAddress


@dataclass(slots=True, frozen=True)
class UniswapStateViewDeployment:
    """UniswapStateViewDeployment class."""

    address: ChecksumAddress


@dataclass(slots=True, frozen=True)
class UniswapV2ExchangeDeployment(AbstractExchangeDeployment):
    """UniswapV2ExchangeDeployment class."""

    factory: UniswapFactoryDeployment


@dataclass(slots=True, frozen=True)
class UniswapV3ExchangeDeployment(AbstractExchangeDeployment):
    """UniswapV3ExchangeDeployment class."""

    factory: UniswapFactoryDeployment


@dataclass(slots=True, frozen=True)
class UniswapV4ExchangeDeployment(AbstractExchangeDeployment):
    """UniswapV4ExchangeDeployment class."""

    pool_manager: UniswapPoolManagerDeployment
    state_view: UniswapStateViewDeployment


# (chain_id, lowercase-factory) → DeploymentRecord — the JSON source of truth,
# indexed once at module load. ``chain_id`` from the JSON is an ``int``;
# ``eth_typing.ChainId`` is an ``IntEnum`` so it hashes equal for the lookup.
_RECORDS: dict[tuple[int, str], DeploymentRecord] = {
    (r.chain_id, r.factory.lower()): r for r in load_deployments()
}


def _record(chain_id: int, factory: str, constant_name: str) -> DeploymentRecord:
    """Look up the JSON ``DeploymentRecord`` for ``(chain_id, factory)``.

    Args:
        chain_id: The deployment's chain id.
        factory: The factory address (any casing).
        constant_name: The Python symbol being built, named in the error if the
            address is missing from the JSON.

    Returns:
        The matching deployment record.

    Raises:
        KeyError: If the factory is absent from the JSON — i.e. a Python
            constant carries an address the verified JSON does not know about.

    """
    record = _RECORDS.get((chain_id, factory.lower()))
    if record is None:
        msg = (
            f"No deployments.json record for {constant_name} "
            f"(chain {chain_id}, factory {factory}) — the JSON is the source of "
            "truth and does not know this factory address."
        )
        raise KeyError(msg)
    return record


def _factory(
    chain_id: int, factory: str, constant_name: str
) -> UniswapFactoryDeployment:
    """Build a :class:`UniswapFactoryDeployment` from the JSON record.

    ``deployer`` and ``pool_init_hash`` are read from the record (``None`` /
    empty → the dataclass's ``None`` / ``""`` normalization, matching the V2
    convention that a null deployer means "the factory itself").

    Returns:
        The factory deployment with fields sourced from the JSON record.

    """
    record = _record(chain_id, factory, constant_name)
    return UniswapFactoryDeployment(
        address=get_checksum_address(factory),
        deployer=get_checksum_address(record.deployer) if record.deployer else None,
        pool_init_hash=record.init_hash or "",
    )


def _v2(name: str, chain_id: int, factory: str) -> UniswapV2ExchangeDeployment:
    """Build a V2 deployment constant whose factory fields come from the JSON.

    Returns:
        The deployment, with ``deployer`` and ``init_hash`` read from the JSON
        record keyed by ``(chain_id, factory)``.

    """
    return UniswapV2ExchangeDeployment(
        name=name,
        chain_id=chain_id,
        factory=_factory(chain_id, factory, name),
    )


def _v3(name: str, chain_id: int, factory: str) -> UniswapV3ExchangeDeployment:
    """Build a V3 deployment constant whose factory fields come from the JSON.

    Returns:
        The deployment, with ``deployer`` and ``init_hash`` read from the JSON
        record keyed by ``(chain_id, factory)``.

    """
    return UniswapV3ExchangeDeployment(
        name=name,
        chain_id=chain_id,
        factory=_factory(chain_id, factory, name),
    )


# Mainnet DEX --------------- START
EthereumMainnetPancakeswapV2 = _v2(
    name="Ethereum Mainnet Pancakeswap V2",
    chain_id=eth_typing.ChainId.ETH,
    factory="0x1097053Fd2ea711dad45caCcc45EfF7548fCB362",
)
EthereumMainnetPancakeswapV3 = _v3(
    name="Ethereum Mainnet Pancakeswap V3",
    chain_id=eth_typing.ChainId.ETH,
    factory="0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
)
EthereumMainnetSushiswapV2 = _v2(
    name="Ethereum Mainnet Sushiswap V2",
    chain_id=eth_typing.ChainId.ETH,
    factory="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
)
EthereumMainnetSushiswapV3 = _v3(
    name="Ethereum Mainnet Sushiswap V3",
    chain_id=eth_typing.ChainId.ETH,
    factory="0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F",
)
EthereumMainnetUniswapV2 = _v2(
    name="Ethereum Mainnet Uniswap V2",
    chain_id=eth_typing.ChainId.ETH,
    factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
)
EthereumMainnetUniswapV3 = _v3(
    name="Ethereum Mainnet Uniswap V3",
    chain_id=eth_typing.ChainId.ETH,
    factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
)
EthereumMainnetUniswapV4 = UniswapV4ExchangeDeployment(
    name="Ethereum Mainnet Uniswap V4",
    chain_id=eth_typing.ChainId.ETH,
    pool_manager=UniswapPoolManagerDeployment(
        address=get_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90"),
    ),
    state_view=UniswapStateViewDeployment(
        address=get_checksum_address("0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"),
    ),
)
# Mainnet DEX --------------- END


# Base DEX ---------------- START
BaseAerodromeV2 = _v2(
    name="Aerodrome V2",
    chain_id=eth_typing.ChainId.BASE,
    factory="0x420DD381b31aEf6683db6B902084cB0FFECe40Da",
)
BaseAerodromeV3 = _v3(
    name="Base Aerodrome V3",
    chain_id=eth_typing.ChainId.BASE,
    factory="0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A",
)
BasePancakeswapV2 = _v2(
    name="Pancakeswap V2",
    chain_id=eth_typing.ChainId.BASE,
    factory="0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E",
)
BasePancakeswapV3 = _v3(
    name="Pancakeswap V3",
    chain_id=eth_typing.ChainId.BASE,
    factory="0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
)
BaseSushiswapV2 = _v2(
    name="Sushiswap V2",
    chain_id=eth_typing.ChainId.BASE,
    factory="0x71524B4f93c58fcbF659783284E38825f0622859",
)
BaseSushiswapV3 = _v3(
    name="Sushiswap V3",
    chain_id=eth_typing.ChainId.BASE,
    factory="0xc35DADB65012eC5796536bD9864eD8773aBc74C4",
)
BaseSwapbasedV2 = _v2(
    name="Swapbased V2",
    chain_id=eth_typing.ChainId.BASE,
    factory="0x04C9f118d21e8B767D2e50C946f0cC9F6C367300",
)
BaseUniswapV2 = _v2(
    name="Uniswap V2",
    chain_id=eth_typing.ChainId.BASE,
    factory="0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6",
)
BaseUniswapV3 = _v3(
    name="Uniswap V3",
    chain_id=eth_typing.ChainId.BASE,
    factory="0x33128a8fC17869897dcE68Ed026d694621f6FDfD",
)
BaseUniswapV4 = UniswapV4ExchangeDeployment(
    name="Uniswap V4",
    chain_id=eth_typing.ChainId.BASE,
    pool_manager=UniswapPoolManagerDeployment(
        address=get_checksum_address("0x498581fF718922c3f8e6A244956aF099B2652b2b"),
    ),
    state_view=UniswapStateViewDeployment(
        address=get_checksum_address("0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71"),
    ),
)
# Base DEX -------------------- END


# Arbitrum DEX -------------- START
ArbitrumCamelotV2 = _v2(
    name="Arbitrum Camelot V2",
    chain_id=eth_typing.ChainId.ARB1,
    factory="0x6EcCab422D763aC031210895C81787E87B43A652",
)
ArbitrumSushiswapV2 = _v2(
    name="Arbitrum Sushiswap V2",
    chain_id=eth_typing.ChainId.ARB1,
    factory="0xc35DADB65012eC5796536bD9864eD8773aBc74C4",
)
ArbitrumUniswapV3 = _v3(
    name="Arbitrum Uniswap V3",
    chain_id=eth_typing.ChainId.ARB1,
    factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
)
ArbitrumSushiswapV3 = _v3(
    name="Arbitrum Sushiswap V3",
    chain_id=eth_typing.ChainId.ARB1,
    factory="0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e",
)
# ----------------------------- END
