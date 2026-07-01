"""Single-source-of-truth: ``uniswap/deployments.py`` matches ``deployments.json``.

The factory address + ``deployer`` + ``init_hash`` for every V2/V3
``*ExchangeDeployment`` constant must agree with the same deployment's record in
``registry/deployments.json``. The JSON is the verified, harness-checked source
of truth (``tests/registry/test_deployment_onchain_verification.py``); the Python
dataclasses are the companion-layer bindings consumed by the CLI + tests.

Two independently-authored sources for the same three values is a drift hazard
— it is exactly this class of silent divergence that shipped three fabricated
Balancer factory addresses. This test locks the invariant: any future edit that
changes one source without the other fails here.

Normalization (matching :func:`tests.registry.test_full_exchange_registration`):
- ``deployer``: the JSON record keeps ``None`` when the deployment's deployer is
  the factory itself; the dataclass also keeps ``None`` there. Compare directly.
- ``init_hash``: the loader normalizes empty strings to ``None``; the dataclass
  keeps empty ``""``. Normalize ``"" or None`` before comparing.
- Addresses are compared EIP-55-checksummed (both sides already checksum, but
  we lowercase to be robust to casing-only differences).
"""

from __future__ import annotations

import pytest

from degenbot.registry.deployment_loader import DeploymentRecord, load_deployments
from degenbot.uniswap.deployments import (
    ArbitrumCamelotV2,
    ArbitrumSushiswapV2,
    ArbitrumSushiswapV3,
    ArbitrumUniswapV3,
    BaseAerodromeV2,
    BaseAerodromeV3,
    BasePancakeswapV2,
    BasePancakeswapV3,
    BaseSushiswapV2,
    BaseSushiswapV3,
    BaseSwapbasedV2,
    BaseUniswapV2,
    BaseUniswapV3,
    EthereumMainnetPancakeswapV2,
    EthereumMainnetPancakeswapV3,
    EthereumMainnetSushiswapV2,
    EthereumMainnetSushiswapV3,
    EthereumMainnetUniswapV2,
    EthereumMainnetUniswapV3,
    UniswapV2ExchangeDeployment,
    UniswapV3ExchangeDeployment,
)

# Every V2/V3 *ExchangeDeployment constant (V4 uses PoolManager, not factory —
# excluded). Order is stable for readable parametrize ids.
_V2V3Deployment = UniswapV2ExchangeDeployment | UniswapV3ExchangeDeployment

_V2_V3_DEPLOYMENTS: list[_V2V3Deployment] = [
    EthereumMainnetPancakeswapV2,
    EthereumMainnetPancakeswapV3,
    EthereumMainnetSushiswapV2,
    EthereumMainnetSushiswapV3,
    EthereumMainnetUniswapV2,
    EthereumMainnetUniswapV3,
    BaseAerodromeV2,
    BaseAerodromeV3,
    BasePancakeswapV2,
    BasePancakeswapV3,
    BaseSushiswapV2,
    BaseSushiswapV3,
    BaseSwapbasedV2,
    BaseUniswapV2,
    BaseUniswapV3,
    ArbitrumCamelotV2,
    ArbitrumSushiswapV2,
    ArbitrumUniswapV3,
    ArbitrumSushiswapV3,
]


def _ids() -> list[str]:
    return [f"{d.chain_id}-{d.name}" for d in _V2_V3_DEPLOYMENTS]


# Index the JSON once, keyed by (chain_id, lowercase factory) — the independent
# source of truth. A constant whose factory isn't in the JSON means the Python
# side carries an address the verified JSON doesn't know about → fail loud.
_RECORDS: dict[tuple[int, str], DeploymentRecord] = {
    (r.chain_id, r.factory.lower()): r for r in load_deployments()
}


def _record_for(deployment: _V2V3Deployment) -> DeploymentRecord:
    """Look up the JSON record matching a deployment's (chain_id, factory)."""
    factory = deployment.factory.address
    key = (deployment.chain_id, factory.lower())
    record = _RECORDS.get(key)
    if record is None:
        msg = (
            f"{deployment.name} (chain {deployment.chain_id}, factory {factory}) "
            "has no matching record in deployments.json — the JSON is the "
            "verified source of truth and does not know this factory address."
        )
        raise AssertionError(msg)
    return record


@pytest.mark.parametrize("deployment", _V2_V3_DEPLOYMENTS, ids=_ids())
class TestDeploymentsPyMatchesJson:
    """Every V2/V3 deployment constant must agree with its JSON record."""

    def test_factory_address_is_in_json(self, deployment: _V2V3Deployment) -> None:
        """The constant's factory address resolves to a JSON record.

        This is the fail-loud guard: a Python constant whose factory address is
        absent from the verified JSON is a stray address the harness cannot
        cover. (After the collapse refactor, the constants are *built* from the
        JSON, so this is guaranteed structurally; until then it's a real check.)
        """
        assert _record_for(deployment) is not None

    def test_deployer_matches(self, deployment: _V2V3Deployment) -> None:
        """The deployer address matches the JSON record (None ↔ None)."""
        record = _record_for(deployment)
        py_deployer = deployment.factory.deployer
        json_deployer = record.deployer
        # Both may be None (deployer == factory). When present, both are
        # checksummed in the JSON; lowercase-compare to be casing-robust.
        if py_deployer is None or json_deployer is None:
            assert py_deployer is None, (
                f"{deployment.name}: deployer py={py_deployer} != json={json_deployer}"
            )
            assert json_deployer is None, (
                f"{deployment.name}: deployer py={py_deployer} != json={json_deployer}"
            )
        else:
            assert py_deployer.lower() == json_deployer.lower(), (
                f"{deployment.name}: deployer py={py_deployer} != json={json_deployer}"
            )

    def test_init_hash_matches(self, deployment: _V2V3Deployment) -> None:
        """The pool_init_hash matches the JSON record (``""`` ↔ None)."""
        record = _record_for(deployment)
        py_hash = deployment.factory.pool_init_hash or None
        json_hash = record.init_hash
        assert py_hash == json_hash, (
            f"{deployment.name}: init_hash py={py_hash!r} != json={json_hash!r}"
        )


class TestNoUntrackedPythonDeployments:
    """Every Python V2/V3 constant has a JSON record (no stray factories)."""

    def test_all_constants_have_records(self) -> None:
        """Sum check: every constant resolves.

        If a constant's factory is missing from the JSON, the per-deployment
        ``test_factory_address_is_in_json`` would also fail — this gives a single
        aggregate view at collection time.
        """
        for deployment in _V2_V3_DEPLOYMENTS:
            _record_for(deployment)  # raises AssertionError if missing
