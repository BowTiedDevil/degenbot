"""On-chain verification of shipped deployment addresses (cast harness).

Every factory address in ``registry/deployments.json`` is checked against the
chain it claims to live on, using Foundry's ``cast``. Three tiers, each
progressively stronger and more expensive:

**Tier 1 — bytecode presence** (default, cheapest, key-less).
    ``cast code <factory> --rpc-url <chain_rpc>`` returns non-empty bytecode
    (length > 2, i.e. not the bare ``"0x"`` of an empty account / EOA). This is
    the check that caught the Balancer factory corruption — three addresses
    that were *never deployed contracts* (zero on-chain bytecode, zero Etherscan
    tx history), wholesale-fabricated placeholders that looked plausible but
    resolved to nothing. Runs against a local RPC; no API key needed.

**Tier 2 — selector fingerprint** (opt-in via ``DEGENBOT_VERIFY_DEPLOYMENTS=2``).
    The deployed bytecode is decoded into its 4-byte function selectors
    (``cast selectors``) and asserted to expose the *factory interface* expected
    for the deployment's ``pool_type`` (e.g. a ``uniswap-v2`` factory must expose
    ``getPair(address,address)`` + ``createPair(address,address)``; a
    ``_balancer-weighted`` factory must expose ``getCreationCode()`` +
    ``isPoolFromFactory(address)``). A fabricated address that happens to land on
    *some* deployed contract still fails here unless it impersonates the right
    factory interface. Key-less (selectors resolve via ``cast 4byte`` /
    openchain.xyz, but the assertion is on raw 4-byte presence, so no network
    beyond the RPC is strictly required).

**Tier 3 — Etherscan source name** (opt-in via ``DEGENBOT_VERIFY_DEPLOYMENTS=3``,
    requires ``ETHERSCAN_API_KEY``). ``getsourcecode`` returns the verified
    Solidity ``ContractName``, asserted to match the expected name for the
    ``pool_type`` (e.g. ``WeightedPoolFactory``, ``UniswapV2Factory``). The
    deepest signal — but unreliable for unverified contracts and unavailable
    without an API key.

**Tier 4 — init_code_hash reproduces pool address** (opt-in via
    ``DEGENBOT_VERIFY_DEPLOYMENTS=4``). The CREATE2 ``init_code_hash`` is the
    one field that's silently breakable and unverified by Tiers 1-3: a wrong
    hash means CREATE2 yields the wrong pair/pool address — silently broken
    pool discovery. This tier recomputes the CREATE2 address of a *known
    deployed* pair/pool in-process (the codebase's own auditable
    ``generate_v2/v3_pool_address``) and asserts it equals the address the
    on-chain factory reports via ``getPair`` / ``getPool`` — proving the stored
    hash reproduces ground truth. Factories without a registered known pair
    skip (not fail); a zero address (pair absent) skips too. Key-less.

Running
-------
The whole module is marked ``online_rpc`` and so is **deselected by default**
(``pyproject.toml`` → ``-m "not slow and not base and not online_rpc"``) — CI
runs offline and never needs a node. Run on demand against a local/mainnet RPC::

    just verify-deployments

Escalate tiers with the env var (default ``1``)::

    # Tier 2 (+selector fingerprint)
    DEGENBOT_VERIFY_DEPLOYMENTS=2 just verify-deployments
    # Tier 3 (+Etherscan source name; needs key)
    DEGENBOT_VERIFY_DEPLOYMENTS=3 ETHERSCAN_API_KEY=... just verify-deployments
    # Tier 4 (+init_code_hash reproduces pool address)
    DEGENBOT_VERIFY_DEPLOYMENTS=4 just verify-deployments

Per-chain rows skip individually when their chain's RPC is unreachable (e.g.
on a machine with only a local Ethereum node, the Base/Arbitrum rows skip).

The tier 1/2/4 read paths run through the injectable
:class:`tests.registry.deployment_verification.CastHarness` seam, which the
record driver below binds to fill the committed golden capture replayed —
fully offline — by ``tests/registry/test_deployment_golden_verification.py``
in the default suite.
"""

from __future__ import annotations

import json
import os
import urllib.request
from functools import cache

import pytest

from degenbot.registry.deployment_loader import DeploymentRecord, load_deployments
from tests.conftest import (
    ARBITRUM_FULL_NODE_HTTP_URI,
    BASE_ARCHIVE_NODE_HTTP_URI,
    ETHEREUM_ARCHIVE_NODE_HTTP_URI,
)
from tests.registry.deployment_verification import (
    EXPECTED_FACTORY_SELECTORS,
    KNOWN_PAIRS,
    CastHarness,
    DeploymentGoldenCapture,
    FoundryCast,
    compute_pool_address,
    is_zero_address,
    onchain_pool_address,
    resolve_runtime_create2_fields,
)

# ---------------------------------------------------------------------------
# Tier selection
# ---------------------------------------------------------------------------

_VERIFY_TIER = int(os.environ.get("DEGENBOT_VERIFY_DEPLOYMENTS", "1"))
"""Verification tier: 1 = bytecode presence (default), 2 = +selector
fingerprint, 3 = +Etherscan source ContractName, 4 = +init_code_hash
reproduces pool address."""

_ETHERSCAN_API_KEY = os.environ.get("ETHERSCAN_API_KEY", "")


def _tier(gte: int) -> bool:
    """True when the active verification tier is >= ``gte``."""
    return gte <= _VERIFY_TIER


# ---------------------------------------------------------------------------
# Per-chain RPC resolution + reachability
# ---------------------------------------------------------------------------

# Mirrors tests/conftest.py URIs (env var → tests.env → built-in default).
# The local Ethereum node (tests.env → http://localhost:8545) covers chain 1;
# Base/Arbitrum default to public RPCs and skip when unreachable.
_CHAIN_RPC: dict[int, str] = {
    1: ETHEREUM_ARCHIVE_NODE_HTTP_URI,
    8453: BASE_ARCHIVE_NODE_HTTP_URI,
    42161: ARBITRUM_FULL_NODE_HTTP_URI,
}

_CAST: CastHarness = FoundryCast()


@cache
def _rpc_reachable(rpc_url: str) -> bool:
    """Quick ``cast block-number`` to confirm the RPC responds.

    Cached per RPC URL so the 32-row parametrization makes one reachability
    check per *chain*, not per row.
    """
    try:
        _CAST.block_number(rpc_url=rpc_url)
    except Exception:  # ruff: ignore[blind-except] — unreachable RPC manifests as any of cast's failure modes
        return False
    return True


def _skip_if_unreachable(chain_id: int) -> str | None:
    """Return a skip reason if ``chain_id``'s RPC is unreachable, else None."""
    entry = _CHAIN_RPC.get(chain_id)
    if entry is None:
        return f"no RPC mapping for chain_id={chain_id} (harness covers {sorted(_CHAIN_RPC)})"
    if not _rpc_reachable(entry):
        return f"chain_id={chain_id} RPC ({entry}) not reachable — set tests.env / env var"
    return None


def _etherscan_source_name(chain_id: int, factory: str) -> str | None:
    """Fetch the verified ``ContractName`` from Etherscan, or None if unverified."""
    key = _ETHERSCAN_API_KEY
    if not key:
        pytest.skip("Tier 3 requires ETHERSCAN_API_KEY")  # pragma: no cover
    url = (
        f"https://api.etherscan.io/v2/api?chainid={chain_id}"
        f"&module=contract&action=getsourcecode&address={factory}&apikey={key}"
    )
    with urllib.request.urlopen(url, timeout=30) as resp:
        data = json.loads(resp.read())
    result = data.get("result")
    if not isinstance(result, list) or not result:
        return None
    name = result[0].get("ContractName")
    return name if isinstance(name, str) and name else None


# ---------------------------------------------------------------------------
# Tier 3 — expected verified ContractName substring per pool_type
# ---------------------------------------------------------------------------

# None entries skip the name assertion (the contract is still asserted
# deployed via Tiers 1-2). Names verified via Etherscan getsourcecode.
EXPECTED_CONTRACT_NAME: dict[str, str | None] = {
    "uniswap-v2": "Factory",
    "uniswap-v3": "UniswapV3Factory",
    "pancakeswap-v3": "PancakeV3Factory",
    "sushiswap-v3": "UniswapV3Factory",  # Sushi V3 forks the V3 factory contract
    "aerodrome-v2": "PoolFactory",
    "aerodrome-v3": "PoolFactory",
    "balancer-weighted": "WeightedPool",  # WeightedPoolFactory / WeightedPool2TokensFactory
    "balancer-stable": None,  # StablePoolFactory OR ComposableStablePoolFactory
}


# ---------------------------------------------------------------------------
# Parametrization
# ---------------------------------------------------------------------------

_RECORDS: list[DeploymentRecord] = load_deployments()
"""All shipped deployments, loaded once at collection time."""

_RECORD_IDS = [f"{r.chain_id}-{r.factory[:10]}…-{r.name}" for r in _RECORDS]


@pytest.fixture
def deployment_capture(request: pytest.FixtureRequest) -> DeploymentGoldenCapture:
    """The golden capture handle, bound to this run's ``--golden-mode``."""
    mode: str = request.config.getoption("--golden-mode")
    return DeploymentGoldenCapture(mode=mode)


@pytest.mark.online_rpc
@pytest.mark.parametrize(
    "record",
    _RECORDS,
    ids=_RECORD_IDS,
)
class TestDeploymentOnchainVerification:
    """Verify every shipped deployment address against its chain."""

    # --- Tier 1: bytecode presence ---------------------------------------

    def test_tier1_factory_has_bytecode(self, record: DeploymentRecord) -> None:
        """Tier 1: the factory address resolves to deployed bytecode.

        This is the cheapest, key-less check and the one that catches
        fabricated / typo'd / never-deployed addresses. ``cast code`` returns
        ``"0x"`` (length 2) for empty accounts; any deployed contract returns
        a longer hex string.
        """
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            pytest.skip(skip)
        rpc_url = _CHAIN_RPC[record.chain_id]
        code = _CAST.code(record.factory, rpc_url=rpc_url)
        assert len(code) > 2, (
            f"{record.name} ({record.factory}, chain {record.chain_id}) has no "
            f"bytecode — `cast code` returned {code!r}. The address is not a "
            f"deployed contract. This is the Balancer-corruption bug class: a "
            f"plausible-looking address that resolves to nothing on-chain."
        )

    # --- Tier 2: selector fingerprint ------------------------------------

    @pytest.mark.skipif(not _tier(2), reason="Tier 2 needs DEGENBOT_VERIFY_DEPLOYMENTS>=2")
    def test_tier2_factory_selector_fingerprint(self, record: DeploymentRecord) -> None:
        """Tier 2: the deployed bytecode exposes the expected factory interface.

        The factory's 4-byte selector set (decoded from its bytecode via
        ``cast selectors``) must be a *superset* of the canonical selectors for
        its ``pool_type``. A fabricated address that happened to land on some
        unrelated deployed contract fails here unless it impersonates the right
        factory interface.
        """
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            pytest.skip(skip)
        expected = EXPECTED_FACTORY_SELECTORS.get(record.pool_type)
        if expected is None:
            pytest.skip(f"no selector fingerprint defined for pool_type={record.pool_type!r}")
        rpc_url = _CHAIN_RPC[record.chain_id]
        deployed = _CAST.selectors(_CAST.code(record.factory, rpc_url=rpc_url))
        missing = expected - deployed
        assert not missing, (
            f"{record.name} ({record.factory}, chain {record.chain_id}, "
            f"pool_type={record.pool_type!r}) is missing expected factory "
            f"selectors {sorted(missing)}. Deployed selectors: {sorted(deployed)[:12]}…"
        )

    # --- Tier 3: Etherscan source ContractName --------------------------

    @pytest.mark.skipif(not _tier(3), reason="Tier 3 needs DEGENBOT_VERIFY_DEPLOYMENTS>=3")
    def test_tier3_etherscan_contract_name(self, record: DeploymentRecord) -> None:
        """Tier 3: the Etherscan-verified ContractName matches the pool_type.

        The deepest signal — the verified Solidity source's contract name.
        Unverified contracts (ContractName=="") are skipped, not failed: source
        verification is optional and absent for some legitimate factories.
        """
        if not _ETHERSCAN_API_KEY:
            pytest.skip("Tier 3 requires ETHERSCAN_API_KEY")
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            pytest.skip(skip)
        name = _etherscan_source_name(record.chain_id, record.factory)
        expected_substring = EXPECTED_CONTRACT_NAME.get(record.pool_type)
        if expected_substring is None:
            pytest.skip(f"no expected ContractName defined for pool_type={record.pool_type!r}")
        if not name:
            pytest.skip(f"{record.name} ({record.factory}) source not verified on Etherscan")
        assert expected_substring.lower() in name.lower(), (
            f"{record.name} ({record.factory}, chain {record.chain_id}) "
            f"Etherscan ContractName={name!r} does not contain "
            f"{expected_substring!r} (expected for pool_type={record.pool_type!r})"
        )

    # --- Tier 4: init_code_hash reproduces pool address ----------------

    @pytest.mark.skipif(not _tier(4), reason="Tier 4 needs DEGENBOT_VERIFY_DEPLOYMENTS>=4")
    def test_tier4_init_hash_reproduces_pool_address(self, record: DeploymentRecord) -> None:
        """Tier 4: the stored CREATE2 ``init_code_hash`` reproduces a real pool.

        Recomputes the CREATE2 address of a *known deployed* pair/pool
        in-process and asserts it equals the address the on-chain factory
        reports via ``getPair`` / ``getPool``.

        This is the only tier that exercises ``init_code_hash`` — the field
        silently breakable when wrong (a wrong hash → wrong CREATE2 address →
        pool lookups silently miss). Skips when the factory has no registered
        known pair, when the pair isn't deployed (on-chain returns zero), or
        when the deployment has no init_hash.
        """
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            pytest.skip(skip)
        if not record.init_hash:
            pytest.skip(f"{record.name}: no init_hash (CREATE2 not used)")
        known = KNOWN_PAIRS.get((record.chain_id, record.factory.lower()))
        if known is None:
            pytest.skip(
                f"{record.name}: no known pair registered for this factory — "
                "add one to KNOWN_PAIRS to cover it"
            )
        rpc_url = _CHAIN_RPC[record.chain_id]
        deployer, init_hash = resolve_runtime_create2_fields(
            record.chain_id,
            record.factory,
            record.pool_type,
        )
        on_chain = onchain_pool_address(_CAST, record.factory, known, rpc_url=rpc_url)
        if is_zero_address(on_chain):
            pytest.skip(
                f"{record.name}: getPair/getPool returned zero — "
                "the known pair is not deployed on this chain/factory"
            )
        computed = compute_pool_address(deployer, init_hash, known)
        assert computed == on_chain, (
            f"{record.name} ({record.factory}, chain {record.chain_id}): "
            f"CREATE2 with runtime-sourced init_hash produced {computed}, but the "
            f"factory reports {on_chain} for the known pair. The stored "
            f"init_code_hash does not reproduce ground truth."
        )


@pytest.mark.online_rpc
class TestRecordDeploymentGoldenCapture:
    """Record driver: fill the golden capture from the live chains.

    Skipped unless ``--golden-mode=record`` (wired up as
    ``just record-deployment-golden``). Every row on a reachable chain
    persists its tier 1/2/4 facts into the committed capture; unreachable
    chains contribute nothing, and the hermetic replay module
    (``test_deployment_golden_verification.py``) skips those rows with a
    reason instead of passing silently.
    """

    @pytest.mark.parametrize("record", _RECORDS, ids=_RECORD_IDS)
    def test_capture_deployment_row(
        self,
        deployment_capture: DeploymentGoldenCapture,
        record: DeploymentRecord,
    ) -> None:
        """Capture one deployment row's tier 1/2/4 facts from the chain."""
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            pytest.skip(skip)
        rpc_url = _CHAIN_RPC[record.chain_id]
        deployment_capture.bind_chain(record.chain_id, rpc_url)
        code = _CAST.code(record.factory, rpc_url=rpc_url)
        deployment_capture.put_row(
            record.chain_id,
            record.factory,
            {
                "name": record.name,
                "pool_type": record.pool_type,
                "bytecode_present": len(code) > 2,
                "selectors": sorted(_CAST.selectors(code)),
                "pool": self._capture_pool(record, rpc_url),
            },
        )

    @staticmethod
    def _capture_pool(record: DeploymentRecord, rpc_url: str) -> dict:
        """Capture the tier-4 pool facts for a row, or its skip reason.

        Mirrors the live tier-4 test's skip semantics: no registered known
        pair, or a zero address from getPair/getPool, records a ``skip``
        entry rather than pool data.
        """
        known = KNOWN_PAIRS.get((record.chain_id, record.factory.lower()))
        if known is None:
            return {"skip": "no known pair registered for this factory"}
        on_chain = onchain_pool_address(_CAST, record.factory, known, rpc_url=rpc_url)
        if is_zero_address(on_chain):
            return {
                "skip": (
                    "getPair/getPool returned zero — the known pair is not "
                    "deployed on this chain/factory"
                ),
            }
        deployer, init_hash = resolve_runtime_create2_fields(
            record.chain_id,
            record.factory,
            record.pool_type,
        )
        computed = compute_pool_address(deployer, init_hash, known)
        return {
            "kind": known[0],
            "tokens": [known[1], known[2]],
            "fee": known[3],
            "deployer": deployer,
            "init_hash": init_hash,
            "on_chain_address": on_chain,
            "computed_address": computed,
        }
