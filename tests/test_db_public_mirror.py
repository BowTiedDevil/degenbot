"""Public ``degenbot.db`` mirror and companion import-boundary tests."""

from __future__ import annotations

import ast
from pathlib import Path

import degenbot._ffi as ffi
import degenbot.db as public_db
import degenbot.updater as public_updater


REPO_ROOT = Path(__file__).resolve().parents[1]
DB_STUB = REPO_ROOT / "src" / "degenbot" / "_ffi" / "db.pyi"
ROOT_STUB = REPO_ROOT / "src" / "degenbot" / "_ffi" / "__init__.pyi"
UPDATER_OWNED = frozenset(
    {
        "LiquidityUpdateEvent",
        "V2PoolRowInput",
        "V3PoolRowInput",
        "V4PoolRowInput",
    }
)


def _stub_exports(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(), filename=str(path))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == "__all__" for target in node.targets
        ):
            return set(ast.literal_eval(node.value))
    msg = f"{path} has no literal __all__"
    raise AssertionError(msg)


def test_public_db_exports_are_importable() -> None:
    for name in public_db.__all__:
        assert getattr(public_db, name) is not None


def test_public_db_mirror_matches_authoritative_stub() -> None:
    mirrored = set(public_db.__all__)
    stubbed = _stub_exports(DB_STUB)

    assert mirrored - {"Erc20TokenRow"} == stubbed - UPDATER_OWNED
    assert mirrored.isdisjoint(UPDATER_OWNED)
    assert "Erc20TokenRow" not in stubbed
    assert "Erc20TokenRow" in _stub_exports(ROOT_STUB)


def test_updater_owns_pool_update_types_excluded_from_public_db() -> None:
    updater_exports = set(public_updater.__all__)

    assert UPDATER_OWNED <= updater_exports
    assert UPDATER_OWNED.isdisjoint(public_db.__all__)
    for name in UPDATER_OWNED:
        assert getattr(public_updater, name) is getattr(ffi.db, name)


def test_public_db_mirror_keeps_ffi_symbol_identity() -> None:
    for name in public_db.__all__:
        if name == "Erc20TokenRow":
            continue
        assert getattr(public_db, name) is getattr(ffi.db, name)

    assert public_db.Erc20TokenRow is ffi.Erc20TokenRow


def test_retired_private_db_imports_are_absent_from_companions() -> None:
    callers = (
        REPO_ROOT / "src" / "degenbot" / "uniswap" / "concentrated" / "snapshot_readers.py",
        REPO_ROOT / "src" / "degenbot" / "aave" / "analysis" / "orchestrator.py",
    )
    for path in callers:
        source = path.read_text()
        assert "from degenbot._ffi.db" not in source
        assert "from degenbot.db import" in source


def test_builder_uses_public_erc20_row_type() -> None:
    source = (REPO_ROOT / "src" / "degenbot" / "builders" / "erc20_builder.py").read_text()
    assert "from degenbot.db import Erc20TokenRow" in source
