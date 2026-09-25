"""Repository gate for the Python/Rust build feature contract.

The package build must use the binding's declared Cargo defaults. Development
profiling and observability features are selected only through the canonical
``dev-features`` alias used by the local bootstrap path.
"""

from __future__ import annotations

import tomllib
from pathlib import Path

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

    assert binding["features"]["dev-features"] == DEVELOPMENT_FEATURES
    assert "uv sync --locked --dev --no-install-project" in justfile
    assert "uv run --no-sync maturin develop --features dev-features" in justfile
    cargo_check = (
        "cargo check --locked -p degenbot_rs --all-targets --manifest-path "
        "rust/Cargo.toml --features dev-features"
    )
    assert cargo_check in justfile
