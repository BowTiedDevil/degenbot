"""Typed pool-family boundary for graph construction.

``classify_pool_kind(s)`` accepts only the Rust-backed ``PoolKind`` values used by
pathfinding. Unsupported family tokens fail at the typed FFI boundary instead of
consulting SQLAlchemy model classes.
"""

from pathlib import Path

import pytest

from degenbot.exceptions.base import DegenbotValueError
from degenbot.pathfinding import (
    PoolKind,
    classify_pool_kind,
    classify_pool_kinds,
    convert_pool_type_filter,
)


def test_every_supported_pool_kind_round_trips() -> None:
    assert classify_pool_kind(PoolKind.V2) is PoolKind.V2
    assert classify_pool_kind(PoolKind.V3) is PoolKind.V3
    assert classify_pool_kind(PoolKind.V4) is PoolKind.V4


def test_pool_kinds_are_deduplicated() -> None:
    assert classify_pool_kinds([PoolKind.V3, PoolKind.V2, PoolKind.V3]) == {
        PoolKind.V2,
        PoolKind.V3,
    }


def test_unsupported_family_token_aborts_loudly() -> None:
    with pytest.raises(ValueError, match="Unsupported pool kind"):
        classify_pool_kinds(["V5"])
    with pytest.raises(DegenbotValueError, match="Unsupported pool kind"):
        convert_pool_type_filter([{"V5"}])


def test_runtime_pathfinding_operator_runner_sources_do_not_import_orm() -> None:
    root = Path(__file__).resolve().parents[2]
    runtime_sources = (
        root / "src/degenbot/pathfinding/_pathfinding.py",
        root / "src/degenbot/runner/build_paths.py",
        root / "src/degenbot/operator/operator_channel.py",
    )
    forbidden = "degenbot.database.models"
    manifest = "degenbot.database.species_manifest"
    for source in runtime_sources:
        text = source.read_text(encoding="utf-8")
        assert forbidden not in text, source
        assert manifest not in text, source

    pyo3_source = (
        root / "rust/crates/shells/degenbot-python/src/pathfinding/mod.rs"
    ).read_text(encoding="utf-8")
    assert "degenbot.database.models" not in pyo3_source
    assert "__mapper__" not in pyo3_source
    assert "polymorphic_identity" not in pyo3_source
