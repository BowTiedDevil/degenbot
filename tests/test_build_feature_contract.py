"""Repository gate for the Python/Rust build feature contract.

The package build must use the binding's declared Cargo defaults. Development
profiling and observability features are selected only through the canonical
``dev-features`` alias used by the local bootstrap path.
"""

from __future__ import annotations

import re
import tomllib
from pathlib import Path

import degenbot

REPO_ROOT = Path(__file__).parents[1]
BINDING_MANIFEST = REPO_ROOT / "rust/crates/shells/degenbot-python/Cargo.toml"
DEVELOPMENT_FEATURES = [
    "pyo3/extension-module",
    "degenbot-bot/hotpath",
    "degenbot-bot/hotpath-prometheus",
    "degenbot-solvers/hotpath",
    "degenbot-bot/allocator-ctrl",
    "otel",
    "mimalloc",
]


def test_python_package_build_uses_release_equivalent_defaults() -> None:
    pyproject = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    binding = tomllib.loads(BINDING_MANIFEST.read_text(encoding="utf-8"))

    assert "features" not in pyproject["tool"]["maturin"]
    assert "dev-features" not in binding["features"].get("default", [])


def test_development_features_have_one_canonical_alias() -> None:
    binding = tomllib.loads(BINDING_MANIFEST.read_text(encoding="utf-8"))
    justfile = (REPO_ROOT / "justfile").read_text(encoding="utf-8")
    build_script = (REPO_ROOT / "scripts/build_extension.sh").read_text(encoding="utf-8")

    assert binding["features"]["dev-features"] == DEVELOPMENT_FEATURES
    assert "uv sync --locked --dev --no-install-project" in justfile
    # The canonical dev-feature install lives in the shared build primitive
    # (scripts/build_extension.sh), which `just dev` and coverage route through.
    assert "uv run --no-sync maturin develop --features dev-features" in build_script
    cargo_check = (
        "cargo check --locked -p degenbot_rs --all-targets --manifest-path "
        "rust/Cargo.toml --features dev-features"
    )
    assert cargo_check in justfile


def test_installed_version_matches_the_cargo_workspace_single_source():
    """The installed dist metadata must equal the Cargo workspace version (ADR-009).

    maturin derives the wheel version from the binding crate's inherited
    `[workspace.package]` version and normalizes it to PEP 440
    (`0.6.0-alpha.15` -> `0.6.0a15`). Drift here means an artifact built from a
    different tree than the one checked out.
    """
    cargo_toml = (Path(__file__).resolve().parents[1] / "rust" / "Cargo.toml").read_text()
    workspace_pkg = cargo_toml[cargo_toml.index("[workspace.package]") :]
    match = re.search(r'^version = "([^"]+)', workspace_pkg, re.MULTILINE)
    assert match, "rust/Cargo.toml has no [workspace.package] version"
    cargo_version = match.group(1)
    normalized = re.sub(
        r"-(alpha|beta|rc)\.",
        lambda m: {"alpha": "a", "beta": "b", "rc": "c"}[m.group(1)],
        cargo_version,
    )
    assert degenbot.__version__ == normalized
