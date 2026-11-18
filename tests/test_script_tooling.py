"""The scripts that turn cargo metadata and a diff into shell-consumable flags.

The mapping is a pure function over a metadata payload and a path list, so it is
proven here rather than through a shell pipeline: the pre-push tier's test
selection is only as trustworthy as this mapping.
"""

from __future__ import annotations

import pathlib

_ROOT = pathlib.Path(__file__).resolve().parents[1]


from tests.repo_script import load_script

rap = load_script("rust_affected_packages", _ROOT / "scripts" / "rust_affected_packages.py")
vm = load_script("vendor_members", _ROOT / "scripts" / "vendor_members.py")
cmf = load_script("cargo_metadata_field", _ROOT / "scripts" / "cargo_metadata_field.py")

_METADATA = {
    "packages": [
        {
            "name": "degenbot-pools",
            "manifest_path": "/repo/rust/crates/foundation/degenbot-pools/Cargo.toml",
        },
        {"name": "degenbot-solver-lab", "manifest_path": "/repo/rust/tools/solver-lab/Cargo.toml"},
        {"name": "degenbot-evm2", "manifest_path": "/repo/rust/vendor/evm2/Cargo.toml"},
        {"name": "outside", "manifest_path": "/elsewhere/Cargo.toml"},
    ]
}


def test_a_crate_source_maps_to_its_package() -> None:
    selected = rap.affected_packages(
        _METADATA,
        ["rust/crates/foundation/degenbot-pools/src/solve/runtime.rs"],
        pathlib.Path("/repo"),
    )
    assert selected == ["degenbot-pools"]


def test_a_package_manifest_maps_to_its_package() -> None:
    selected = rap.affected_packages(
        _METADATA, ["rust/tools/solver-lab/Cargo.toml"], pathlib.Path("/repo")
    )
    assert selected == ["degenbot-solver-lab"]


def test_a_non_rust_path_selects_nothing() -> None:
    assert (
        rap.affected_packages(_METADATA, [".github/workflows/ci.yml"], pathlib.Path("/repo")) == []
    )


def test_selection_is_deduplicated_and_sorted() -> None:
    selected = rap.affected_packages(
        _METADATA,
        [
            "rust/tools/solver-lab/src/bin/x/main.rs",
            "rust/crates/foundation/degenbot-pools/src/lib.rs",
            "rust/crates/foundation/degenbot-pools/Cargo.toml",
        ],
        pathlib.Path("/repo"),
    )
    assert selected == ["degenbot-pools", "degenbot-solver-lab"]


def test_a_package_outside_the_root_is_not_a_candidate() -> None:
    assert rap.affected_packages(_METADATA, ["outside/src/lib.rs"], pathlib.Path("/repo")) == []


def test_vendored_members_become_exclude_flags() -> None:
    assert vm.exclusions(_METADATA) == ["--exclude", "degenbot-evm2"]


def test_a_metadata_field_reads_top_level_values() -> None:
    payload = {"target_directory": "/repo/rust/target", "workspace_root": None}
    assert cmf.field_value(payload, "target_directory") == "/repo/rust/target"
    assert not cmf.field_value(payload, "workspace_root")
    assert not cmf.field_value(payload, "absent")
