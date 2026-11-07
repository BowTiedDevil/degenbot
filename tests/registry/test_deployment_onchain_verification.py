"""On-chain verification of shipped deployment addresses (cast harness).

Every factory address in ``registry/deployments.json`` is checked against the
chain it claims to live on, using Foundry's ``cast``. Two tiers stay live and
on demand; the other two are replayed offline from a committed capture
(``test_deployment_golden_verification.py``), so no live path reads an external
verifier:

**Tier 1 — bytecode presence** (default, cheapest, key-less).
    ``cast code <factory> --rpc-url <chain_rpc>`` returns non-empty bytecode
    (length > 2, i.e. not the bare ``"0x"`` of an empty account / EOA). This is
    the check that caught the Balancer factory corruption — three addresses
    that were *never deployed contracts* (zero on-chain bytecode, zero Etherscan
    tx history), wholesale-fabricated placeholders that looked plausible but
    resolved to nothing. Runs against a local RPC; no API key needed.

**Captured — Tier 2 (selector fingerprint) and Tier 3 (verified source name).**
    Both are facts about a deployed address that do not change between runs,
    so the record driver below stores them and the hermetic replay module
    asserts them: the selector set the bytecode exposes, and the verified
    source-contract name. Recording the name goes through
    :class:`tests.registry.deployment_verification.BlockscoutVerifiedSource`
    (key-less), never a per-run external lookup.

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
(``pyproject.toml`` → ``-m "not slow and not base and not online_rpc and not stub_drift"``) — CI
runs offline and never needs a node. The dedicated ``deployment_verify`` marker
additionally excludes the module from ``just test-fork``'s selection: these are
intentional per-row live drift checks, not routine fork coverage. Its single
owning live lane is ``just verify-deployments`` (recording:
``just record-deployment-golden``). Run on demand against a local/mainnet RPC::

    just verify-deployments

Escalate tiers with the env var (default ``1``)::

    # Tier 4 (+init_code_hash reproduces pool address)
    DEGENBOT_VERIFY_DEPLOYMENTS=4 just verify-deployments

    # Re-record the captured tier-2/tier-3 facts (selectors + verified name)
    just record-deployment-golden

Per-chain rows skip individually when their chain's RPC is unreachable (e.g.
on a machine with only a local Ethereum node, the Base/Arbitrum rows skip).

The tier 1/4 read paths run through the injectable
:class:`tests.registry.deployment_verification.CastHarness` seam, and the tier-3
name through
:class:`tests.registry.deployment_verification.BlockscoutVerifiedSource`; the
record driver below binds both to fill the committed golden capture replayed —
fully offline — by ``tests/registry/test_deployment_golden_verification.py``
in the default suite.
"""

from __future__ import annotations

import os
from functools import cache

import pytest

from degenbot.registry.deployment_loader import DeploymentRecord, load_deployments
from tests.conftest import (
    TEST_ARBITRUM_FULL_NODE_HTTP_URI,
    TEST_BASE_ARCHIVE_NODE_HTTP_URI,
    TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI,
)
from tests.registry.deployment_verification import (
    KNOWN_PAIRS,
    BlockscoutVerifiedSource,
    CastHarness,
    DeploymentGoldenCapture,
    FoundryCast,
    VerifiedSource,
    compute_pool_address,
    is_zero_address,
    onchain_pool_address,
    resolve_runtime_create2_fields,
)

# ---------------------------------------------------------------------------
# Tier selection
# ---------------------------------------------------------------------------

_VERIFY_TIER = int(os.environ.get("DEGENBOT_VERIFY_DEPLOYMENTS", "1"))
"""Live verification tier: 1 = bytecode presence (default), 4 = +init_code_hash
reproduces the pool address. Tiers 2/3 are captured facts replayed offline."""


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
    1: TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI,
    8453: TEST_BASE_ARCHIVE_NODE_HTTP_URI,
    42161: TEST_ARBITRUM_FULL_NODE_HTTP_URI,
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


# ---------------------------------------------------------------------------
# Parametrization
# ---------------------------------------------------------------------------

_RECORDS: list[DeploymentRecord] = load_deployments()
"""All shipped deployments, loaded once at collection time."""

_RECORD_IDS = [f"{r.chain_id}-{r.factory[:10]}…-{r.name}" for r in _RECORDS]


@pytest.fixture
def verified_source() -> VerifiedSource:
    """The verified-source seam the record driver records names through."""
    return BlockscoutVerifiedSource()


@pytest.fixture
def deployment_capture(request: pytest.FixtureRequest) -> DeploymentGoldenCapture:
    """The golden capture handle, bound to this run's ``--golden-mode``.

    Skips at setup outside ``--golden-mode=record``: only a record run may
    write capture rows (the class docstring's contract); the hermetic replay
    module constructs its own replay-mode handle directly.

    """
    mode: str = request.config.getoption("--golden-mode")
    if mode != "record":
        pytest.skip(
            f"golden capture records only under --golden-mode=record (this run: {mode!r})",
        )
    return DeploymentGoldenCapture(mode=mode)


@pytest.mark.deployment_verify
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


@pytest.mark.deployment_verify
@pytest.mark.online_rpc
class TestRecordDeploymentGoldenCapture:
    """Record driver: fill the golden capture from the live chains.

    Skipped unless ``--golden-mode=record`` (wired up as
    ``just record-deployment-golden``). Every row on a reachable chain
    persists its chain-derived tier 1/2/4 facts into the committed capture.

    The tier-3 name is chain-independent (a verifier lookup, not an RPC read),
    so it is recorded for EVERY shipped row, including rows whose chain this
    run cannot reach: those keep their previously recorded chain facts and
    gain the name field. A row whose verifier cannot answer records the reason
    instead of a name, and the hermetic replay module
    (``test_deployment_golden_verification.py``) skips it with that reason
    instead of passing silently.
    """

    @pytest.mark.parametrize("record", _RECORDS, ids=_RECORD_IDS)
    def test_capture_deployment_row(
        self,
        deployment_capture: DeploymentGoldenCapture,
        record: DeploymentRecord,
        verified_source: VerifiedSource,
    ) -> None:
        """Capture one deployment row's tier 1/2/3/4 facts from their sources."""
        name_facts = self._capture_contract_name(verified_source, record)
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            # The name needs no chain read, so it still lands on the row a
            # previous run recorded; the chain facts stay as they were.
            deployment_capture.merge_row(record.chain_id, record.factory, name_facts)
            pytest.skip(skip)
        rpc_url = _CHAIN_RPC[record.chain_id]
        deployment_capture.bind_chain(record.chain_id, rpc_url)
        code = _CAST.code(record.factory, rpc_url=rpc_url)
        row = {
            "name": record.name,
            "pool_type": record.pool_type,
            "bytecode_present": len(code) > 2,
            "selectors": sorted(_CAST.selectors(code)),
            "pool": self._capture_pool(record, rpc_url),
        }
        row.update(name_facts)
        deployment_capture.put_row(record.chain_id, record.factory, row)

    @staticmethod
    def _capture_contract_name(verified_source: VerifiedSource, record: DeploymentRecord) -> dict:
        """Capture the tier-3 verified source name, or its recorded skip reason.

        A transport fault is recorded as the skip reason rather than aborting
        the run: a verifier outage must not cost the tier-1/2/4 facts this run
        proved. The replay module skips that row with the recorded reason
        (loud, never silent).
        """
        try:
            name = verified_source.contract_name(record.chain_id, record.factory)
        except Exception as exc:  # ruff: ignore[blind-except] — verifier outages are any-transport
            return {"contract_name": None, "contract_name_skip": f"verifier call failed: {exc}"}
        if not name:
            return {
                "contract_name": None,
                "contract_name_skip": "address is not verified on the source endpoint",
            }
        return {"contract_name": name}

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
