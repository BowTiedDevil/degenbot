"""Golden snapshot: ``load_deployments()`` reproduces the live registry.

The shipped ``deployments.json`` is the single source of deployment data
(chain_id, factory → deployer / init_hash / variant / dex_identity). This test
pins that the JSON loader reproduces — field for field — the registrations
the inline ``_register_*_deployments()`` tuples currently feed into
``pool_type_registry`` at import time.

If a registration drifts between the JSON and the inline tuples (or the inline
tuples are later deleted in favour of the loader), this test surfaces it as a
field-level diff keyed by ``(chain_id, factory)``.
"""

from __future__ import annotations

from operator import itemgetter

import pytest

from degenbot.registry.deployment_loader import (
    POOL_TYPE_MAP,
    DeploymentRecord,
    load_deployments,
    register_from_deployments,
)
from degenbot.registry.pool_type import pool_type_registry

_RECORDS = load_deployments()
_KEYS = sorted({(r.chain_id, r.factory) for r in _RECORDS}, key=itemgetter(0, 1))
_IDS = [f"{c}-{f[:10]}" for c, f in _KEYS]


def _records_by_key() -> dict[tuple[int, str], DeploymentRecord]:
    return {(r.chain_id, r.factory): r for r in load_deployments()}


def _live_registrations() -> dict[tuple[int, str], tuple[object, ...]]:
    """Snapshot the live ``pool_type_registry`` singleton (populated at import).

    Returns:
        ``(chain_id, factory) → (pool_class, variant, family_str, dex_id, deployer, init_hash)``.
    """
    out: dict[tuple[int, str], tuple[object, ...]] = {}
    for (chain_id, factory), (
        pool_class,
        desc,
        deployment,
    ) in pool_type_registry.registrations.items():
        family_str = desc.family.value if desc.family is not None else None
        dex_id = pool_type_registry.get_v2_identity(chain_id, factory)
        init = deployment.pool_init_hash or None
        out[chain_id, factory] = (
            pool_class,
            desc.variant,
            family_str,
            dex_id,
            deployment.deployer,
            init,
        )
    return out


class TestLoadDeploymentsMatchesLiveRegistry:
    """The JSON loader reproduces ``pool_type_registry`` field-for-field."""

    def test_loads_at_least_one_record(self) -> None:
        """Sanity: the shipped JSON is non-empty."""
        assert len(_RECORDS) > 0

    def test_keys_match_live_registry_exactly(self) -> None:
        """Every (chain_id, factory) in the live registry is in the JSON, and vice versa."""
        json_keys = set(_records_by_key())
        live_keys = set(_live_registrations())
        assert json_keys == live_keys, (
            f"JSON-only keys: {json_keys - live_keys}\nlive-only keys: {live_keys - json_keys}"
        )

    @pytest.mark.parametrize(("chain_id", "factory"), _KEYS, ids=_IDS)
    def test_pool_type_resolves_to_live_class(self, chain_id: int, factory: str) -> None:
        """The JSON ``pool_type`` maps to the same Python class the registry holds."""
        record = _records_by_key()[chain_id, factory]
        live_class, *_ = _live_registrations()[chain_id, factory]
        assert POOL_TYPE_MAP[record.pool_type] is live_class

    @pytest.mark.parametrize(("chain_id", "factory"), _KEYS, ids=_IDS)
    def test_variant_matches(self, chain_id: int, factory: str) -> None:
        """The JSON variant (or class-attr fallback) matches the registry's variant."""
        record = _records_by_key()[chain_id, factory]
        _, live_variant, *_ = _live_registrations()[chain_id, factory]
        # JSON variant=null means "use class attr" (loader passes None → register
        # uses getattr(cls, "variant", None)); a string means explicit override.
        resolved = record.variant
        if resolved is None:
            resolved = getattr(POOL_TYPE_MAP[record.pool_type], "variant", None)
        assert resolved == live_variant

    @pytest.mark.parametrize(("chain_id", "factory"), _KEYS, ids=_IDS)
    def test_family_matches(self, chain_id: int, factory: str) -> None:
        """The JSON family override (or auto-derive) matches the registry's family."""
        record = _records_by_key()[chain_id, factory]
        _, _, live_family, *_ = _live_registrations()[chain_id, factory]
        if record.family is not None:
            assert record.family == live_family

    @pytest.mark.parametrize(("chain_id", "factory"), _KEYS, ids=_IDS)
    def test_deployment_data_matches(self, chain_id: int, factory: str) -> None:
        """deployer + init_hash match the registry's deployment data."""
        record = _records_by_key()[chain_id, factory]
        _, _, _, _, live_deployer, live_init = _live_registrations()[chain_id, factory]
        # JSON deployer=null means "use factory" (the loader/register default).
        expected_deployer = record.deployer if record.deployer is not None else factory
        assert expected_deployer == live_deployer
        # JSON init_hash=null/"" → None.
        expected_init = record.init_hash or None
        assert expected_init == live_init

    @pytest.mark.parametrize(("chain_id", "factory"), _KEYS, ids=_IDS)
    def test_dex_identity_matches(self, chain_id: int, factory: str) -> None:
        """The JSON dex_variant resolves to the same DexIdentity (or both None)."""
        record = _records_by_key()[chain_id, factory]
        _, _, _, live_dex, *_ = _live_registrations()[chain_id, factory]
        if record.dex_variant is None:
            assert live_dex is None
        else:
            assert live_dex is not None
            assert live_dex.variant == record.dex_variant  # type: ignore[union-attr]


class TestDeploymentRecordShape:
    """The record dataclass is well-formed."""

    def test_is_frozen(self) -> None:
        """DeploymentRecord is frozen (immutable)."""
        import dataclasses

        record = _RECORDS[0]
        with pytest.raises(dataclasses.FrozenInstanceError):
            record.chain_id = 999  # type: ignore[misc]

    def test_factory_is_checksummed(self) -> None:
        """Every factory address in the JSON is EIP-55 checksummed."""
        from degenbot.checksum_cache import get_checksum_address

        for record in _RECORDS:
            assert record.factory == get_checksum_address(record.factory)


class TestOverlayMerge:
    """The ``[deployments] overlay`` config mechanism merges user deployments."""

    def test_programmatic_overlay_overrides_on_conflict(self, tmp_path) -> None:  # noqa: ANN001
        """An overlay entry with the same (chain_id, factory) overrides the shipped default."""
        # Override Uniswap V2 mainnet with a different init_hash (simulating
        # a user correcting/replacing a deployment).
        factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        overlay = {
            "deployments": [
                {
                    "name": "Custom Uniswap V2",
                    "chain_id": 1,
                    "pool_type": "uniswap-v2",
                    "variant": None,
                    "dex_variant": "uniswap-v2",
                    "family": None,
                    "factory": factory,
                    "deployer": None,
                    "init_hash": "0x" + "ab" * 32,
                }
            ]
        }
        import json

        overlay_file = tmp_path / "overlay.json"
        overlay_file.write_text(json.dumps(overlay), encoding="utf-8")

        records = load_deployments(overlay_path=overlay_file)
        # The overridden entry carries the overlay's init_hash + name.
        record = {(r.chain_id, r.factory): r for r in records}[(1, factory)]
        assert record.name == "Custom Uniswap V2"
        assert record.init_hash == "0x" + "ab" * 32

    def test_programmatic_overlay_adds_new_deployment(self, tmp_path) -> None:  # noqa: ANN001
        """An overlay entry with a new (chain_id, factory) appends to the shipped set."""
        import json

        from degenbot.checksum_cache import get_checksum_address

        new_factory = get_checksum_address("0x" + "a" * 40)
        overlay = {
            "deployments": [
                {
                    "name": "My Custom DEX",
                    "chain_id": 999,
                    "pool_type": "uniswap-v2",
                    "variant": "mycustom",
                    "dex_variant": None,
                    "family": None,
                    "factory": new_factory,
                    "deployer": None,
                    "init_hash": "0x" + "cd" * 32,
                }
            ]
        }
        overlay_file = tmp_path / "overlay.json"
        overlay_file.write_text(json.dumps(overlay), encoding="utf-8")

        records = load_deployments(overlay_path=overlay_file)
        record = {(r.chain_id, r.factory): r for r in records}[(999, new_factory)]
        assert record.name == "My Custom DEX"
        assert record.init_hash == "0x" + "cd" * 32
        # The shipped entries are unaffected.
        assert len(records) > 1

    def test_missing_overlay_file_is_silent_noop(self, tmp_path) -> None:  # noqa: ANN001
        """A non-existent overlay path falls back to shipped defaults only."""
        records = load_deployments(overlay_path=tmp_path / "nonexistent.json")
        # Same count as the shipped JSON (no overlay applied).
        shipped = load_deployments()
        assert len(records) == len(shipped)
        assert {(r.chain_id, r.factory) for r in records} == {
            (r.chain_id, r.factory) for r in shipped
        }


class TestRegisterFromDeployments:
    """``register_from_deployments`` populates a fresh registry from records."""

    def test_registers_every_record(self) -> None:
        """Every DeploymentRecord produces a registration in the target registry."""
        from degenbot.registry.pool_type import PoolTypeRegistry

        reg = PoolTypeRegistry()
        records = load_deployments()
        register_from_deployments(records, reg)
        assert len(reg.registrations) == len(records)

    def test_keys_match_records(self) -> None:
        """The registry keys are exactly the records' (chain_id, factory)."""
        from degenbot.registry.pool_type import PoolTypeRegistry

        reg = PoolTypeRegistry()
        register_from_deployments(load_deployments(), reg)
        record_keys = {(r.chain_id, r.factory) for r in load_deployments()}
        assert set(reg.registrations) == record_keys

    def test_fresh_registry_reproduces_singleton(self) -> None:
        """A fresh registry fed by the loader matches the live singleton field-for-field."""
        from degenbot.registry.pool_type import PoolTypeRegistry, pool_type_registry

        fresh = PoolTypeRegistry()
        fresh.set_default_v2_class(
            pool_type_registry.get_v2_class(1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")  # type: ignore[arg-type]
        )
        register_from_deployments(load_deployments(), fresh)
        # Same keyset.
        assert set(fresh.registrations) == set(pool_type_registry.registrations)
        # Same pool_class for every key.
        for key in fresh.registrations:
            assert fresh.registrations[key][0] is pool_type_registry.registrations[key][0]

    def test_balancer_family_override_resolves(self) -> None:
        """The Balancer weighted-pool family string → PoolFamily.WEIGHTED."""
        from degenbot.registry.pool_type import PoolTypeRegistry
        from degenbot.types.pool_type import PoolFamily

        reg = PoolTypeRegistry()
        register_from_deployments(load_deployments(), reg)
        weighted_factory = "0x8e9AA87E45e92BAD7D5F7F9dd794cEa12f21707B"
        desc = reg.get_descriptor(1, weighted_factory)
        assert desc is not None
        assert desc.family == PoolFamily.WEIGHTED

    def test_dex_identity_resolved_for_uniswap_v2(self) -> None:
        """The uniswap-v2 dex_variant resolves to a non-None PyDexIdentity."""
        from degenbot.registry.pool_type import PoolTypeRegistry

        reg = PoolTypeRegistry()
        register_from_deployments(load_deployments(), reg)
        identity = reg.get_v2_identity(1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
        assert identity is not None
        assert identity.variant == "uniswap-v2"
