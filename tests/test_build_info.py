"""Build-identity tests for the Rust extension (stale-.so detector).

Background (AGENTS.md "Rebuilding the Rust `.so` after edits"): maturin/uv have
repeatedly served a stale cached artifact for `degenbot._ffi` after Rust edits
while reporting a successful rebuild. Every compile of `degenbot_rs` runs
`rust/crates/degenbot-python/build.rs`, which embeds a `<count, fingerprint>`
build identity (the counter advancing only when the crate's source content
changes). Any installed extension whose fingerprint differs from the repo
receipt predates the latest build.
"""

import tomllib
from pathlib import Path

import pytest

from degenbot.build_info import installed_build_number, read_receipt, verify_build_fresh

REPO_ROOT = Path(__file__).parents[1]


def test_build_number_is_positive() -> None:
    # build.rs runs on EVERY compile of degenbot_rs (including this test
    # build), so the installed extension must always carry a number >= 1.
    # 0 is the no-build.rs fallback and means the tagging broke.
    assert installed_build_number() >= 1


def test_installed_extension_is_fresh() -> None:
    # The primary gate: fails whenever the installed .so was built from
    # different sources than the latest recorded build (the cached-artifact
    # failure mode). Skips where there is no repo receipt to compare against
    # (non-editable/published install, fresh clone before a first build).
    if read_receipt() is None:
        pytest.skip("no .build-number receipt (non-editable install)")
    verify_build_fresh()


@pytest.mark.parametrize(
    "relative_path",
    [
        Path("rust/Cargo.toml"),
        Path("rust/Cargo.lock"),
        Path(".cargo/config.toml"),
        Path("rust/crates/degenbot/src/lib.rs"),
        Path("rust/crates/degenbot/src/investigation/mod.rs"),
        Path("rust/crates/degenbot-cli/build.rs"),
        Path("rust/crates/degenbot-python/build.rs"),
        Path("rust/crates/degenbot-python/build_scan.rs"),
    ],
)
def test_uv_cache_keys_cover_rust_build_inputs(relative_path: Path) -> None:
    pyproject = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text())
    cache_keys = pyproject["tool"]["uv"]["cache-keys"]
    input_path = REPO_ROOT / relative_path

    matched = any(
        input_path == REPO_ROOT / match
        for key in cache_keys
        for match in REPO_ROOT.glob(key["file"])
    )
    assert matched, f"{relative_path} must invalidate the Python build cache"
