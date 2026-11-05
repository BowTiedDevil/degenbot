"""Hermetic golden replay of the deployment-verification tiers (tiers 1/2/3/4).

Replays the committed capture
(``tests/golden/data/registry/deployment_onchain_verification.json``) inside
the default suite: no cast binary, no RPC, no networks, no markers. The
assertions are the same tier invariants the record driver
(``test_deployment_onchain_verification.py``) records, and the two modules share
one expectations home (``tests/registry/deployment_verification.py``):

- tier 1 — every captured factory row has on-chain bytecode present;
- tier 2 — the captured selector set covers the expected factory interface
  for the row's ``pool_type``;
- tier 3 — the captured verified source-contract name matches the expected
  name for the row's ``pool_type``;
- tier 4 — the in-process CREATE2 recompute (runtime-sourced deployer +
  init_hash through the auditable ``generate_v2/v3_pool_address``) equals the
  recorded on-chain ``getPair``/``getPool`` address — the init_code_hash
  drift detector.

A row absent from the capture (chain unreachable at record time, or simply
never recorded) skips loudly with a reason and the re-record command. A row
present but missing a tier's FIELD is a capture gap — it fails, name and all,
because that is drift in the artifact, not a fact about the chain. A row whose
tier-3 name was recorded as absent carries the reason and skips with it.

Refresh the capture with ``just record-deployment-golden``.
"""

from __future__ import annotations

from functools import cache

import pytest

from degenbot.registry.deployment_loader import DeploymentRecord, load_deployments
from tests.registry.deployment_verification import (
    EXPECTED_CONTRACT_NAME,
    EXPECTED_FACTORY_SELECTORS,
    GOLDEN_CAPTURE_PATH,
    DeploymentGoldenCapture,
    compute_pool_address,
    resolve_runtime_create2_fields,
)

_RECORDS: list[DeploymentRecord] = load_deployments()
"""All shipped deployments, loaded once at collection time."""

_RECORD_IDS = [f"{r.chain_id}-{r.factory[:10]}…-{r.name}" for r in _RECORDS]

_UNRECORDED = (
    "{name} ({factory}, chain {chain}) has no golden capture entry — "
    "record it with `just record-deployment-golden` (capture: "
    f"{GOLDEN_CAPTURE_PATH})"
)

_UNCAPTURED_FIELD = (
    "{name} ({factory}, chain {chain}) has a golden capture entry with no "
    "{field!r} field — the capture predates that tier's recording, so this "
    "run cannot assert it. Re-record with `just record-deployment-golden` "
    f"(capture: {GOLDEN_CAPTURE_PATH})"
)


@cache
def _capture() -> DeploymentGoldenCapture:
    """Read-only handle over the committed capture (fast/fail per suite)."""
    return DeploymentGoldenCapture(mode="replay")


def _captured_row(record: DeploymentRecord) -> dict:
    """The capture entry for a row, skipping loudly when absent."""
    row = _capture().row(record.chain_id, record.factory)
    if row is None:
        pytest.skip(
            _UNRECORDED.format(name=record.name, factory=record.factory, chain=record.chain_id),
        )
    return row


class TestDeploymentGoldenVerification:
    """Replay the committed capture against the tier invariants."""

    def test_capture_records_at_least_one_verified_name(self) -> None:
        """Tier 3 has real coverage — a capture with no names cannot pass silently.

        Every row may legitimately record ``contract_name: null`` with a reason
        (unverified address, verifier outage), but a capture where that is
        *always* the answer has lost the tier-3 fact entirely: the replay would
        skip every row and prove nothing.
        """
        named = [key for key, row in _capture().rows().items() if row.get("contract_name")]
        assert named, (
            f"The golden deployment capture at {GOLDEN_CAPTURE_PATH} records no "
            "verified contract name for any row — tier 3 has nothing to assert. "
            "Re-record with `just record-deployment-golden`."
        )

    def test_capture_covers_at_least_one_chain(self) -> None:
        """The capture is populated — replay can never silently skip everything."""
        assert _capture().rows(), (
            f"The golden deployment capture at {GOLDEN_CAPTURE_PATH} has no rows. "
            "Re-record with `just record-deployment-golden` against reachable RPCs."
        )

    @pytest.mark.parametrize("record", _RECORDS, ids=_RECORD_IDS)
    def test_tier1_bytecode_present(self, record: DeploymentRecord) -> None:
        """Tier 1 (replay): the captured factory address has bytecode."""
        row = _captured_row(record)
        assert row["bytecode_present"] is True, (
            f"{record.name} ({record.factory}, chain {record.chain_id}) recorded "
            f"bytecode_present={row['bytecode_present']!r} — the address did not "
            "resolve to deployed bytecode when captured. This is the "
            "Balancer-corruption bug class: a plausible-looking address that "
            "resolves to nothing on-chain."
        )

    @pytest.mark.parametrize("record", _RECORDS, ids=_RECORD_IDS)
    def test_tier2_selector_fingerprint(self, record: DeploymentRecord) -> None:
        """Tier 2 (replay): the captured selectors cover the expected interface."""
        row = _captured_row(record)
        expected = EXPECTED_FACTORY_SELECTORS.get(record.pool_type)
        if expected is None:
            pytest.skip(f"no selector fingerprint defined for pool_type={record.pool_type!r}")
        deployed = set(row["selectors"])
        missing = expected - deployed
        assert not missing, (
            f"{record.name} ({record.factory}, chain {record.chain_id}, "
            f"pool_type={record.pool_type!r}) is missing expected factory "
            f"selectors {sorted(missing)} from its capture. Captured selectors: "
            f"{sorted(deployed)[:12]}… — the factory interface changed on-chain "
            "or the capture is stale."
        )

    @pytest.mark.parametrize("record", _RECORDS, ids=_RECORD_IDS)
    def test_tier3_verified_contract_name(self, record: DeploymentRecord) -> None:
        """Tier 3 (replay): the captured verified name matches the pool_type.

        The deepest identity signal, recorded once instead of re-fetched from a
        verifier per run. A row recorded with ``contract_name: null`` carries the
        reason it has no name (unverified address, verifier outage); that skips
        with the recorded reason. A row whose entry has no ``contract_name``
        field at all is a capture gap and fails.
        """
        row = _captured_row(record)
        if "contract_name" not in row:
            raise AssertionError(
                _UNCAPTURED_FIELD.format(
                    name=record.name,
                    factory=record.factory,
                    chain=record.chain_id,
                    field="contract_name",
                )
            )
        name = row["contract_name"]
        if name is None:
            pytest.skip(
                f"{record.name}: tier 3 skipped at record time — "
                f"{row.get('contract_name_skip', 'no reason recorded')}"
            )
        expected_substring = EXPECTED_CONTRACT_NAME.get(record.pool_type)
        if expected_substring is None:
            pytest.skip(f"no expected ContractName defined for pool_type={record.pool_type!r}")
        assert expected_substring.lower() in name.lower(), (
            f"{record.name} ({record.factory}, chain {record.chain_id}) recorded "
            f"verified name {name!r}, which does not contain {expected_substring!r} "
            f"(expected for pool_type={record.pool_type!r}) — the address points at "
            "a different contract family, or the capture is stale."
        )

    @pytest.mark.parametrize("record", _RECORDS, ids=_RECORD_IDS)
    def test_tier4_create2_recompute_matches_onchain(self, record: DeploymentRecord) -> None:
        """Tier 4 (replay): the in-process recompute matches the on-chain address.

        Recomputes CREATE2 in-process from the Rust runtime's deployer +
        init_hash resolvers (hermetic, no RPC) and compares against the
        captured on-chain ``getPair``/``getPool`` address — the init_code_hash
        drift detector.
        """
        row = _captured_row(record)
        pool = row["pool"]
        if "skip" in pool:
            pytest.skip(f"{record.name}: tier 4 skipped at record time — {pool['skip']}")
        deployer, init_hash = resolve_runtime_create2_fields(
            record.chain_id,
            record.factory,
            record.pool_type,
        )
        known = (pool["kind"], pool["tokens"][0], pool["tokens"][1], pool["fee"])
        computed = compute_pool_address(deployer, init_hash, known)
        assert computed == pool["computed_address"], (
            f"{record.name} ({record.factory}, chain {record.chain_id}): the "
            f"runtime-sourced CREATE2 inputs now produce {computed}, but the "
            f"capture recorded {pool['computed_address']}. The deployer/"
            f"init_hash resolution has drifted from the captured state "
            f"(deployer={deployer}, init_hash={init_hash})."
        )
        assert computed == pool["on_chain_address"], (
            f"{record.name} ({record.factory}, chain {record.chain_id}): "
            f"CREATE2 with runtime-sourced init_hash produced {computed}, but "
            f"the factory reported {pool['on_chain_address']} for the known "
            "pair when captured. The stored init_code_hash does not reproduce "
            "ground truth."
        )
