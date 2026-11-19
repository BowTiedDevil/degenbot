"""The scripts that turn cargo metadata and a diff into shell-consumable flags.

The mapping is a pure function over a metadata payload and a path list, so it is
proven here rather than through a shell pipeline: the pre-push tier's test
selection is only as trustworthy as this mapping.
"""

from __future__ import annotations

import json
import pathlib
from types import SimpleNamespace

import pytest

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


def _member(
    name: str, directory: str, *declared: dict[str, object], root: str | pathlib.Path = "/repo"
) -> dict[str, object]:
    """Build one synthetic `cargo metadata` package entry under `root`.

    Args:
        name: The package name Cargo reports, aliases notwithstanding.
        directory: The repo-relative directory holding the manifest.
        declared: The dependencies the package declares, one dict per edge.
        root: The repository root the manifest path is made absolute against.

    Returns:
        A package object shaped like a `cargo metadata --no-deps` entry.
    """
    package: dict[str, object] = {
        "name": name,
        "manifest_path": f"{root}/{directory}/Cargo.toml",
    }
    if declared:
        package["dependencies"] = list(declared)
    return package


_NESTED = {
    "packages": [
        _member("degenbot-evm2-jit", "rust/vendor/crates/jit"),
        _member("degenbot-evm2-jit-backend", "rust/vendor/crates/jit/backend"),
    ]
}

# The nested pair plus a package that declares the nested one, so a direct hit
# has both a parent directory and a reverse edge to get wrong.
_WORKSPACE = {
    "packages": [
        _member("degenbot-evm2-jit", "rust/vendor/crates/jit"),
        _member("degenbot-evm2-jit-backend", "rust/vendor/crates/jit/backend"),
        _member(
            "degenbot-evm2-jit-codegen",
            "rust/vendor/crates/jit/codegen",
            {"name": "degenbot-evm2-jit-backend", "rename": "evm2_jit_backend", "kind": None},
        ),
    ]
}

_GLOBAL_INPUTS = [
    "rust/Cargo.toml",
    "rust/Cargo.lock",
    "rust/.config/nextest.toml",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    "pyproject.toml",
]

_EDGE_DECLARATIONS = [
    pytest.param({"name": "core", "kind": None}, id="normal"),
    pytest.param({"name": "core", "kind": "dev"}, id="dev"),
    pytest.param({"name": "core", "kind": "build"}, id="build"),
    pytest.param({"name": "core", "kind": None, "optional": True}, id="optional"),
    pytest.param({"name": "core", "kind": "dev", "target": "cfg(unix)"}, id="target"),
]


def _completed(returncode: int, stdout: str = "", stderr: str = "") -> SimpleNamespace:
    """Stand in for the attributes this module reads off a completed process."""
    return SimpleNamespace(returncode=returncode, stdout=stdout, stderr=stderr)


def test_a_nested_member_owns_its_own_directory() -> None:
    selected = rap.affected_packages(
        _NESTED, ["rust/vendor/crates/jit/backend/src/lib.rs"], pathlib.Path("/repo")
    )
    assert selected == ["degenbot-evm2-jit-backend"]


def test_a_directly_touched_package_selects_its_dependents() -> None:
    selected = rap.affected_packages(
        _WORKSPACE, ["rust/vendor/crates/jit/backend/src/lib.rs"], pathlib.Path("/repo")
    )
    assert selected == ["degenbot-evm2-jit-backend", "degenbot-evm2-jit-codegen"]


def test_selection_closes_over_multi_level_dependent_chains() -> None:
    metadata = {
        "packages": [
            _member("core", "rust/core"),
            _member("mid", "rust/mid", {"name": "core", "kind": None}),
            _member("top", "rust/top", {"name": "mid", "kind": "dev"}),
        ]
    }
    selected = rap.affected_packages(
        metadata,
        ["rust/core/src/lib.rs", "rust/mid/Cargo.toml"],
        pathlib.Path("/repo"),
    )
    assert selected == ["core", "mid", "top"]


@pytest.mark.parametrize("declared", _EDGE_DECLARATIONS)
def test_every_declared_edge_kind_carries_selection(declared: dict[str, object]) -> None:
    metadata = {
        "packages": [
            _member("core", "rust/core"),
            _member("dependent", "rust/dependent", declared, {"name": "serde", "kind": None}),
        ]
    }
    selected = rap.affected_packages(metadata, ["rust/core/src/lib.rs"], pathlib.Path("/repo"))
    assert selected == ["core", "dependent"]


@pytest.mark.parametrize("path", _GLOBAL_INPUTS)
def test_a_global_build_input_selects_every_member(path: str) -> None:
    selected = rap.affected_packages(_WORKSPACE, [path], pathlib.Path("/repo"))
    assert selected == [
        "degenbot-evm2-jit",
        "degenbot-evm2-jit-backend",
        "degenbot-evm2-jit-codegen",
    ]


def test_a_cross_package_rename_selects_both_ownerships() -> None:
    selected = rap.affected_packages(
        _WORKSPACE,
        [
            "rust/vendor/crates/jit/backend/src/moved.rs",
            "rust/vendor/crates/jit/codegen/src/moved.rs",
        ],
        pathlib.Path("/repo"),
    )
    assert selected == ["degenbot-evm2-jit-backend", "degenbot-evm2-jit-codegen"]


def test_an_empty_diff_selects_nothing() -> None:
    assert rap.affected_packages(_WORKSPACE, [], pathlib.Path("/repo")) == []


@pytest.mark.parametrize("path", ["rust/README.md", "rust/tools/README.md"])
def test_documentation_under_rust_selects_nothing(path: str) -> None:
    assert rap.affected_packages(_WORKSPACE, [path], pathlib.Path("/repo")) == []


def test_an_unmapped_rust_path_refuses_selection() -> None:
    with pytest.raises(rap.SelectionRefused, match="rust/tools/notes"):
        rap.affected_packages(_WORKSPACE, ["rust/tools/notes.toml"], pathlib.Path("/repo"))


def test_the_outgoing_diff_reads_both_ends_of_a_rename(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[list[str]] = []

    def fake_run(argv: list[str], *, check: bool = False) -> SimpleNamespace:
        calls.append(argv)
        if "--abbrev-ref" in argv:
            return _completed(0, "origin/main\n")
        return _completed(
            0,
            "rust/vendor/crates/jit/backend/src/moved.rs\n"
            "rust/vendor/crates/jit/codegen/src/moved.rs\n",
        )

    monkeypatch.setattr(rap, "_run", fake_run)
    assert rap.outgoing_paths() == [
        "rust/vendor/crates/jit/backend/src/moved.rs",
        "rust/vendor/crates/jit/codegen/src/moved.rs",
    ]
    assert "--no-renames" in calls[-1]
    assert calls[-1][-1] == "origin/main...HEAD"


def test_a_failed_diff_refuses_selection(monkeypatch: pytest.MonkeyPatch) -> None:
    def fake_run(argv: list[str], *, check: bool = False) -> SimpleNamespace:
        if "--abbrev-ref" in argv:
            return _completed(0, "origin/main\n")
        if "diff" in argv:
            return _completed(128, "", "fatal: bad revision 'origin/main...HEAD'")
        return _completed(0, "origin/main\n")

    monkeypatch.setattr(rap, "_run", fake_run)
    with pytest.raises(rap.SelectionRefused, match="git diff"):
        rap.outgoing_paths()


def test_an_unresolvable_base_refuses_selection(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        rap, "_run", lambda argv, *, check=False: _completed(1, "", "fatal: no upstream")
    )
    with pytest.raises(rap.SelectionRefused, match="base"):
        rap.outgoing_paths()


def test_the_metadata_read_is_offline(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[list[str]] = []

    def fake_run(argv: list[str], *, check: bool = False) -> SimpleNamespace:
        calls.append(argv)
        return _completed(0, json.dumps({"packages": []}))

    monkeypatch.setattr(rap, "_run", fake_run)
    assert rap._metadata() == {"packages": []}
    assert calls == [
        [
            "cargo",
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
            "rust/Cargo.toml",
        ]
    ]


def test_a_metadata_failure_refuses_selection(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(
        rap,
        "_run",
        lambda argv, *, check=False: _completed(1, "", "error: manifest not found"),
    )
    assert rap.main([]) == 1
    assert "rust/Cargo.toml" in capsys.readouterr().err


def test_the_cli_reports_a_refusal_without_a_traceback(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str], tmp_path: pathlib.Path
) -> None:
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(
        rap,
        "_run",
        lambda argv, *, check=False: _completed(
            0, json.dumps({"packages": [_member("core", "rust/core", root=tmp_path)]})
        ),
    )
    assert rap.main(["rust/tools/notes.toml"]) == 1
    assert "rust/tools/notes.toml" in capsys.readouterr().err


def test_the_cli_emits_package_flags(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str], tmp_path: pathlib.Path
) -> None:
    monkeypatch.chdir(tmp_path)
    metadata = {
        "packages": [
            _member("degenbot-evm2-jit-backend", "rust/vendor/crates/jit/backend", root=tmp_path),
            _member(
                "degenbot-evm2-jit-codegen",
                "rust/vendor/crates/jit/codegen",
                {"name": "degenbot-evm2-jit-backend", "kind": None},
                root=tmp_path,
            ),
        ]
    }
    monkeypatch.setattr(
        rap, "_run", lambda argv, *, check=False: _completed(0, json.dumps(metadata))
    )
    assert rap.main(["rust/vendor/crates/jit/backend/src/lib.rs"]) == 0
    assert capsys.readouterr().out == "-p degenbot-evm2-jit-backend -p degenbot-evm2-jit-codegen \n"


def test_vendored_members_become_exclude_flags() -> None:
    assert vm.exclusions(_METADATA) == ["--exclude", "degenbot-evm2"]


def test_a_metadata_field_reads_top_level_values() -> None:
    payload = {"target_directory": "/repo/rust/target", "workspace_root": None}
    assert cmf.field_value(payload, "target_directory") == "/repo/rust/target"
    assert not cmf.field_value(payload, "workspace_root")
    assert not cmf.field_value(payload, "absent")
