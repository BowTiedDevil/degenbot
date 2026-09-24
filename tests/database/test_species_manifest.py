"""Tests for the Python species-manifest reader and its consumers.

Covers the three consumers named by ADR-059 D3's Python half:

- the typed reader itself (validation mirroring ``species.rs``);
- the model parity gate (manifest species ↔ SQLAlchemy polymorphic identity ↔
  subclass table are 1:1);
- ``build_paths``' V2/V3/V4 expansion, driven by the manifest rather than a
  second hand-listed enumeration.

The fixture test proves the boundary: a species added to a manifest flows into
the expansion machinery, and the model-less fake is refused with the exact
mismatch error (a class cannot be faked in a TOML fixture).
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from degenbot.database import species_manifest as sm
from degenbot.database.models.pools import (
    UniswapV2PoolTableBase,
    UniswapV3PoolTableBase,
    UniswapV4PoolTable,
)
from degenbot.registry.deployment_loader import load_deployments
from degenbot.runner.build_paths import (
    _POOL_VERSION_MAP,
    _pool_types_from_filter,
)

FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
INIT_HASH = "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"

REAL_V2_TABLES = {
    "uniswap_v2_pools",
    "sushiswap_v2_pools",
    "pancakeswap_v2_pools",
    "camelot_v2_pools",
    "swapbased_v2_pools",
    "aerodrome_v2_pools",
}


def _all_v2_models() -> list[type]:
    return sm.concrete_pool_types(UniswapV2PoolTableBase)


def _all_v3_models() -> list[type]:
    return sm.concrete_pool_types(UniswapV3PoolTableBase)


def _all_v2_v3_models() -> list[type]:
    return _all_v2_models() + _all_v3_models()


def _v2_block(kind: str, table: str) -> str:
    return (
        f'[[species]]\nkind = "{kind}"\nfamily = "v2"\ntable = "{table}"\n'
        f"fee_denominator = 1000\n"
        f'[[species.chains]]\nchain_id = 1\nfactory = "{FACTORY}"\n'
        f'init_codehash = "{INIT_HASH}"\n'
    )


def _v3_block(kind: str, table: str) -> str:
    return (
        f'[[species]]\nkind = "{kind}"\nfamily = "v3"\ntable = "{table}"\n'
        f'fee_denominator = 1000000\nslot_layout = "uniswap_v3"\n'
        f'[[species.chains]]\nchain_id = 1\nfactory = "{FACTORY}"\n'
        f'init_codehash = "{INIT_HASH}"\n'
    )


def _v4_block() -> str:
    return (
        '[[species]]\nkind = "uniswap_v4"\nfamily = "v4"\ntable = "managed"\n'
        "fee_denominator = 1000000\n"
        "[[species.chains]]\nchain_id = 1\n"
        'manager = "0x000000000004444c5dc75cB358380D2e3dE08A90"\n'
    )


# ──────────────────────────────────────────────────────────────────
# Shipped manifest + model parity
# ──────────────────────────────────────────────────────────────────


def test_shipped_manifest_parses_and_is_cached() -> None:
    sm.reset_manifest_cache()
    manifest = sm.manifest()
    assert len(manifest.species) == 11
    assert manifest is sm.manifest()
    assert manifest.get("uniswap_v2") is not None
    assert manifest.get("uniswap_v4").family is sm.Family.V4


def test_manifest_kinds_match_model_identities_one_to_one() -> None:
    manifest_kinds = {s.kind for s in sm.manifest().species}
    assert manifest_kinds == sm.model_pool_kinds()
    assert "uniswap_v4" in manifest_kinds


def test_manifest_tables_match_model_subclass_tables_one_to_one() -> None:
    by_table = {model.__tablename__: model for model in _all_v2_v3_models()}
    assert set(by_table) == sm.model_subclass_tables()
    for species in sm.manifest().species:
        if species.family is sm.Family.V4:
            assert species.table == sm.V4_MANAGED_TABLE
        else:
            model = by_table[species.table]
            assert model.__mapper__.polymorphic_identity == species.kind


def test_parity_gate_accepts_the_shipped_manifest() -> None:
    sm.assert_manifest_model_parity(sm.manifest())


def test_subclass_table_for_kind_projects_the_manifest() -> None:
    manifest = sm.manifest()
    assert manifest.subclass_table_for_kind("uniswap_v2") == "uniswap_v2_pools"
    assert manifest.subclass_table_for_kind("uniswap_v4") is None
    assert manifest.subclass_table_for_kind("unsiwap_v2") is None


def test_shipped_manifest_reader_targets_relocated_foundation_manifest() -> None:
    path = sm.shipped_manifest_path()
    repo_root = Path(__file__).resolve().parents[2]
    expected = repo_root / "rust" / "crates" / "foundation" / "degenbot-db" / "src" / "species.toml"
    assert path == expected
    assert path.is_file()
    assert path.name == "species.toml"
    assert sm.parse_manifest(path.read_text(encoding="utf-8")).species[0].kind == "uniswap_v2"


def test_shipped_manifest_path_falls_back_to_repo_cwd(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    repo_root = Path(__file__).resolve().parents[2]
    monkeypatch.setattr(sm, "__file__", str(tmp_path / "package" / "species_manifest.py"))
    monkeypatch.chdir(repo_root)

    assert sm.shipped_manifest_path() == (
        repo_root / "rust" / "crates" / "foundation" / "degenbot-db" / "src" / "species.toml"
    )


# ──────────────────────────────────────────────────────────────────
# build_paths consumption
# ──────────────────────────────────────────────────────────────────


def test_build_paths_version_map_projects_the_manifest() -> None:
    assert sm.pool_version_map(sm.manifest()) == _POOL_VERSION_MAP
    assert {t.__name__ for t in _POOL_VERSION_MAP["V2"]} == {t.__name__ for t in _all_v2_models()}
    assert {t.__name__ for t in _POOL_VERSION_MAP["V3"]} == {t.__name__ for t in _all_v3_models()}
    assert _POOL_VERSION_MAP["V4"] == [UniswapV4PoolTable]


def test_build_paths_pool_types_from_filter_covers_every_manifest_species() -> None:
    all_types = {t.__name__ for t in _pool_types_from_filter(None)}
    assert all_types == {t.__name__ for t in _all_v2_v3_models()} | {"UniswapV4PoolTable"}
    v3_types = {t.__name__ for t in _pool_types_from_filter({"V3-V4-V3"})}
    assert v3_types == {t.__name__ for t in _all_v3_models()} | {"UniswapV4PoolTable"}


# ──────────────────────────────────────────────────────────────────
# Fixture: a fake species flows through the expansion and is refused
# ──────────────────────────────────────────────────────────────────


def test_fake_species_flows_into_expansion_and_gate_refuses() -> None:
    fake = _v2_block("uniswap_v2", "uniswap_v2_pools") + _v2_block("fake_v2", "fake_v2_pools")
    parsed = sm.parse_manifest(fake, allowed_tables=REAL_V2_TABLES | {"fake_v2_pools"})
    # The extra species is in the parsed manifest — the fixture flowed in.
    assert parsed.get("fake_v2") is not None

    # The expansion resolves each manifest species to a model; the fake has
    # none, so it is refused with the exact boundary error.
    with pytest.raises(
        sm.SpeciesModelMismatchError,
        match=re.escape(
            "manifest species 'fake_v2' has no SQLAlchemy pool model "
            "with polymorphic identity 'fake_v2'",
        ),
    ):
        sm.pool_version_map(parsed)

    with pytest.raises(
        sm.SpeciesModelMismatchError,
        match=re.escape("manifest species without a model: ['fake_v2']"),
    ):
        sm.assert_manifest_model_parity(parsed)


def test_parity_gate_refuses_a_table_mismatch() -> None:
    parsed = sm.parse_manifest(_v2_block("uniswap_v2", "sushiswap_v2_pools"))
    with pytest.raises(
        sm.SpeciesModelMismatchError,
        match=re.escape(
            "manifest species 'uniswap_v2' names table 'sushiswap_v2_pools', "
            "but model UniswapV2PoolTable declares 'uniswap_v2_pools'",
        ),
    ):
        sm.pool_version_map(parsed)


# ──────────────────────────────────────────────────────────────────
# Reader validation (mirrors species.rs)
# ──────────────────────────────────────────────────────────────────


def test_rejects_an_unknown_manifest_key() -> None:
    with pytest.raises(sm.ManifestError, match="unknown key"):
        sm.parse_manifest("extra = 1\n" + _v2_block("uniswap_v2", "uniswap_v2_pools"))


def test_rejects_an_unknown_species_key() -> None:
    body = _v2_block("uniswap_v2", "uniswap_v2_pools").replace(
        'table = "uniswap_v2_pools"',
        'table = "uniswap_v2_pools"\nfee_denom = 1000',
    )
    with pytest.raises(sm.ManifestError, match="unknown key"):
        sm.parse_manifest(body)


def test_rejects_an_unknown_chain_key() -> None:
    body = _v2_block("uniswap_v2", "uniswap_v2_pools") + "extra = 1\n"
    with pytest.raises(sm.ManifestError, match="unknown key"):
        sm.parse_manifest(body)


def test_rejects_a_duplicate_kind() -> None:
    body = _v2_block("uniswap_v2", "uniswap_v2_pools") * 2
    with pytest.raises(sm.ManifestError, match="declared more than once"):
        sm.parse_manifest(body)


def test_rejects_an_unknown_subclass_table() -> None:
    with pytest.raises(sm.ManifestError, match="unknown subclass table"):
        sm.parse_manifest(_v2_block("uniswap_v2", "not_a_pool_table"))


def test_rejects_v3_without_layout_or_denominator() -> None:
    no_layout = (
        '[[species]]\nkind = "uniswap_v3"\nfamily = "v3"\n'
        'table = "uniswap_v3_pools"\nfee_denominator = 1000000\n'
        f'[[species.chains]]\nchain_id = 1\nfactory = "{FACTORY}"\n'
        f'init_codehash = "{INIT_HASH}"\n'
    )
    with pytest.raises(sm.ManifestError, match="missing its slot_layout"):
        sm.parse_manifest(no_layout)
    no_denominator = (
        '[[species]]\nkind = "uniswap_v3"\nfamily = "v3"\n'
        'table = "uniswap_v3_pools"\nslot_layout = "uniswap_v3"\n'
        f'[[species.chains]]\nchain_id = 1\nfactory = "{FACTORY}"\n'
        f'init_codehash = "{INIT_HASH}"\n'
    )
    with pytest.raises(sm.ManifestError, match="missing its fee_denominator"):
        sm.parse_manifest(no_denominator)


def test_rejects_stable_on_non_v2() -> None:
    body = _v3_block("uniswap_v3", "uniswap_v3_pools").replace(
        'slot_layout = "uniswap_v3"\n',
        'slot_layout = "uniswap_v3"\nstable = true\n',
    )
    with pytest.raises(sm.ManifestError, match="declares stable"):
        sm.parse_manifest(body)


def test_rejects_a_non_positive_fee_denominator() -> None:
    body = _v2_block("uniswap_v2", "uniswap_v2_pools").replace(
        "fee_denominator = 1000",
        "fee_denominator = 0",
    )
    with pytest.raises(sm.ManifestError, match="non-positive fee_denominator"):
        sm.parse_manifest(body)


def test_rejects_a_v4_species_naming_a_subclass_table() -> None:
    body = _v4_block().replace('table = "managed"', 'table = "uniswap_v4_pools"')
    with pytest.raises(sm.ManifestError, match="must name table 'managed'"):
        sm.parse_manifest(body)


def test_rejects_a_v4_chain_carrying_a_factory() -> None:
    body = _v4_block().replace('manager = "0x', f'factory = "{FACTORY}"\nmanager = "0x')
    with pytest.raises(sm.ManifestError, match="carries a CREATE2 identifier"):
        sm.parse_manifest(body)


def test_rejects_a_v2_chain_without_a_factory() -> None:
    body = (
        '[[species]]\nkind = "uniswap_v2"\nfamily = "v2"\n'
        'table = "uniswap_v2_pools"\nfee_denominator = 1000\n'
        "[[species.chains]]\nchain_id = 1\n"
    )
    with pytest.raises(sm.ManifestError, match="missing its factory"):
        sm.parse_manifest(body)


def test_rejects_a_duplicate_chain() -> None:
    body = _v2_block("uniswap_v2", "uniswap_v2_pools") + (
        f'[[species.chains]]\nchain_id = 1\nfactory = "{FACTORY}"\ninit_codehash = "{INIT_HASH}"\n'
    )
    with pytest.raises(sm.ManifestError, match="more than once"):
        sm.parse_manifest(body)


def test_rejects_an_empty_species_list() -> None:
    with pytest.raises(sm.ManifestError, match="declares no species"):
        sm.parse_manifest("species = []")


def test_rejects_invalid_toml() -> None:
    with pytest.raises(sm.ManifestError, match="not valid TOML"):
        sm.parse_manifest("this is not = = toml")


def test_rejects_an_invalid_address() -> None:
    body = _v2_block("uniswap_v2", "uniswap_v2_pools").replace(FACTORY, "0xnothex")
    with pytest.raises(sm.ManifestError, match="invalid factory"):
        sm.parse_manifest(body)


def test_allowed_tables_can_be_overridden_for_fixtures() -> None:
    parsed = sm.parse_manifest(
        _v2_block("fake_v2", "fake_v2_pools"),
        allowed_tables={"fake_v2_pools"},
    )
    assert parsed.get("fake_v2") is not None


# ──────────────────────────────────────────────────────────────────
# V4 manager-species add path + deployments.json reconciliation
# ──────────────────────────────────────────────────────────────────


def _v4_manager_block(kind: str, manager: str) -> str:
    return (
        f'[[species]]\nkind = "{kind}"\nfamily = "v4"\ntable = "managed"\n'
        f"fee_denominator = 1000000\n"
        f'[[species.chains]]\nchain_id = 1\nmanager = "{manager}"\n'
    )


def test_fixture_v4_species_loads_and_expansion_refuses_until_a_model_exists() -> None:
    """The documented add-a-species recipe (ADR-059 D3).

    A new V4 manager species is a manifest row. The loader accepts it and its
    manager is keyed by chain; the model parity gate refuses it with the exact
    mismatch until the SQLAlchemy model identity lands. The Rust
    ``MixedPoolManagers`` compose refusal is per-manager and independent of the
    manifest roster (see ``degenbot-strategy``'s refusal test).
    """
    manager = "0x1111111111111111111111111111111111111111"
    parsed = sm.parse_manifest(_v4_manager_block("sushi_v4", manager))
    sushi = parsed.get("sushi_v4")
    assert sushi is not None
    assert sushi.family is sm.Family.V4
    assert sushi.table == sm.V4_MANAGED_TABLE
    assert sushi.chains[1].manager == manager
    assert parsed.subclass_table_for_kind("sushi_v4") is None

    with pytest.raises(
        sm.SpeciesModelMismatchError,
        match=re.escape(
            "manifest species 'sushi_v4' has no SQLAlchemy pool model "
            "with polymorphic identity 'sushi_v4'",
        ),
    ):
        sm.pool_version_map(parsed)

    with pytest.raises(
        sm.SpeciesModelMismatchError,
        match=re.escape("manifest species without a model: ['sushi_v4']"),
    ):
        sm.assert_manifest_model_parity(parsed)


def test_shipped_manifest_chains_reconcile_with_deployments_json() -> None:
    """Every manifest chain agrees with the canonical ``deployments.json``.

    The manifest owns species identity; ``deployments.json`` owns the
    on-chain-resolution extras (CREATE2 deployer, Aerodrome implementation).
    Their shared facts — the per-chain factory + CREATE2 init hash, and the V4
    manager that must never be keyed as a factory — are pinned here so a
    one-sided edit fails.
    """
    records = {(r.chain_id, r.factory.lower()): r for r in load_deployments()}
    for species in sm.manifest().species:
        assert species.chains, species.kind
        for chain in species.chains.values():
            if species.family is sm.Family.V4:
                assert chain.manager is not None
                assert (chain.chain_id, chain.manager.lower()) not in records, (
                    f"V4 manager for {species.kind!r} must not be a factory row"
                )
            else:
                assert chain.factory is not None
                record = records[chain.chain_id, chain.factory.lower()]
                assert record.init_hash == chain.init_codehash, (
                    f"{species.kind!r} chain {chain.chain_id} init hash drift"
                )


# ──────────────────────────────────────────────────────────────────
# Declared-but-unsupported LFJ family (ADR-059 E3)
# ──────────────────────────────────────────────────────────────────

#: A clearly-placeholder LFJ factory used only to exercise the species SHAPE.
#: The shipped manifest deliberately declares no LFJ deployment.
LFJ_FACTORY = "0x000000000000000000000000000000000000dead"


def _lfj_block() -> str:
    return (
        '[[species]]\nkind = "lfj_binned"\nfamily = "lfj"\n'
        'table = "lfj_pools"\n'
        f'[[species.chains]]\nchain_id = 1\nfactory = "{LFJ_FACTORY}"\n'
    )


def test_lfj_family_shape_loads_and_resolves_to_the_model() -> None:
    body = sm.shipped_manifest_path().read_text(encoding="utf-8") + "\n" + _lfj_block()
    parsed = sm.parse_manifest(body)
    lfj = parsed.get("lfj_binned")
    assert lfj is not None
    assert lfj.family is sm.Family.LFJ
    assert lfj.table == sm.LFJ_POOLS
    assert parsed.subclass_table_for_kind("lfj_binned") == sm.LFJ_POOLS
    # The model now exists, so the parity gate resolves the declared family
    # instead of refusing it as model-less.
    sm.assert_manifest_model_parity(parsed)


def test_lfj_kind_is_declared_unsupported_until_a_tier_admits_it() -> None:
    assert "lfj_binned" in sm.DECLARED_UNSUPPORTED_KINDS
    assert "lfj_binned" not in sm.model_pool_kinds()
    assert sm.manifest().get("lfj_binned") is None, "no invented LFJ deployment"


def test_lfj_species_must_name_the_lfj_table() -> None:
    bad = (
        '[[species]]\nkind = "lfj_binned"\nfamily = "lfj"\n'
        'table = "uniswap_v2_pools"\n'
        f'[[species.chains]]\nchain_id = 1\nfactory = "{LFJ_FACTORY}"\n'
    )
    with pytest.raises(sm.ManifestError, match="must name table"):
        sm.parse_manifest(bad)
