#!/usr/bin/env python3
"""The tier-3 harness artifact map and every consumer of it.

`harness-artifacts.tsv` binds each committed artifact to its git-tracked Solidity
source, the path its compiler emits it at, the build script that produces it, and
its publish group (`oracle` | `executor`). Build staging, publish selection, the
source hashes in `artifacts/manifest.json`, and the compile-vs-use verifier all
read that one table, so an artifact cannot be published or verified through a
hand-maintained list that drifts from it.

Subcommands (paths are relative to this script's directory):

    scripts   unique build scripts, in map order
    rows      the selected rows' artifact entries, one per line
    stage     copy a group's raw compiler output into OUT_DIR at its entry path
    publish   copy a group's OUT_DIR entries into the tracked artifacts/ tree,
              then rewrite the source-hash manifest
    manifest  rewrite artifacts/manifest.json from the map alone
    verify    byte-compare the selected OUT_DIR entries against the committed
              artifacts (creation code and deployed runtime), failing on a
              missing artifact or a missing/non-hex bytecode field
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

TIER3_ROOT = Path(__file__).resolve().parent
MAP_PATH = TIER3_ROOT / "harness-artifacts.tsv"
ARTIFACTS_DIR = TIER3_ROOT / "artifacts"
MANIFEST_PATH = ARTIFACTS_DIR / "manifest.json"
DEFAULT_OUT_DIR = TIER3_ROOT / "out"

# The artifact JSON fields whose bytes must survive a fresh compile. Creation
# code proves the constructor output; the deployed runtime proves what the hook
# actually runs against the Rust engine once deployed.
CHECKED_FIELDS = ("bytecode.object", "deployedBytecode.object")
COLUMNS = ("artifact", "source", "compiler_output", "build_script", "group")
PUBLISH_GROUPS = ("oracle", "executor")
REBUILD_RECIPE = {
    "oracle": "just rebuild-tier3-artifacts",
    "executor": "just rebuild-tier3-executor-stubs",
}
HEX_DIGITS = frozenset("0123456789abcdef")


class ArtifactMapError(RuntimeError):
    """A malformed map entry, a missing build output, or an unreadable artifact."""


@dataclass(frozen=True)
class Row:
    """One committed artifact and the pipeline that produces it."""

    artifact: str
    source: str
    compiler_output: str
    build_script: str
    group: str


def _relative(value: str) -> Path:
    path = Path(value)
    if path.is_absolute() or ".." in path.parts:
        raise ArtifactMapError(f"{MAP_PATH.name}: path {value!r} must stay inside tier3-oracle/")
    return path


def load_map() -> list[Row]:
    """Parse and validate the map; every artifact entry appears exactly once."""
    rows: list[Row] = []
    seen: set[str] = set()
    for lineno, line in enumerate(MAP_PATH.read_text(encoding="utf-8").splitlines(), start=1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) != len(COLUMNS):
            raise ArtifactMapError(
                f"{MAP_PATH.name}:{lineno}: expected {len(COLUMNS)} tab-separated columns "
                f"{COLUMNS}, found {len(fields)}"
            )
        row = Row(*fields)
        if row.artifact in seen:
            raise ArtifactMapError(f"{MAP_PATH.name}:{lineno}: duplicate artifact {row.artifact!r}")
        seen.add(row.artifact)
        if row.group not in PUBLISH_GROUPS:
            raise ArtifactMapError(
                f"{MAP_PATH.name}:{lineno}: group {row.group!r} is not one of {PUBLISH_GROUPS}"
            )
        if not (TIER3_ROOT / _relative(row.source)).is_file():
            raise ArtifactMapError(f"{MAP_PATH.name}:{lineno}: source {row.source} is missing")
        if not (TIER3_ROOT / _relative(row.build_script)).is_file():
            raise ArtifactMapError(
                f"{MAP_PATH.name}:{lineno}: build script {row.build_script} is missing"
            )
        rows.append(row)
    if not rows:
        raise ArtifactMapError(f"{MAP_PATH.name} lists no artifacts")
    return rows


def select(
    rows: Sequence[Row], *, group: str | None = None, script: str | None = None
) -> list[Row]:
    """The rows a consumer asked for; an empty selection is a map bug, not a no-op."""
    picked = [
        row
        for row in rows
        if (group is None or row.group == group) and (script is None or row.build_script == script)
    ]
    if not picked:
        raise ArtifactMapError(
            f"no artifact-map row matches group={group!r} build_script={script!r}"
        )
    return picked


def _out_dir(value: str | None) -> Path:
    return Path(value).resolve() if value else DEFAULT_OUT_DIR


def manifest_payload(rows: Sequence[Row]) -> dict[str, dict[str, str]]:
    """artifact -> {source, sha256-of-source} for the toolchain-free drift test."""
    payload: dict[str, dict[str, str]] = {}
    for row in rows:
        source = TIER3_ROOT / row.source
        if not source.is_file():
            raise ArtifactMapError(f"row {row.artifact}: source {row.source} is missing")
        payload[row.artifact] = {
            "source": row.source,
            "sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
        }
    return payload


def write_manifest() -> int:
    """Rewrite the committed manifest from the whole map, sources only."""
    rows = load_map()
    MANIFEST_PATH.write_text(
        json.dumps(manifest_payload(rows), indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"wrote {MANIFEST_PATH.name} ({len(rows)} artifacts)")
    return 0


def stage_rows(rows: Sequence[Row], raw_dir: Path, out_dir: Path) -> int:
    """Lay a group's compiler output out at its entry path under OUT_DIR."""
    sources = [(row, raw_dir / row.compiler_output) for row in rows]
    missing = [str(path) for _, path in sources if not path.is_file()]
    if missing:
        listing = "\n  ".join(missing)
        raise ArtifactMapError(
            f"missing compiler output under {raw_dir}:\n  {listing}\n"
            "The build did not emit every artifact the map expects."
        )
    for row, path in sources:
        target = out_dir / row.artifact
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
    print(f"staged {len(sources)} artifact(s) into {out_dir}")
    return 0


def publish_rows(rows: Sequence[Row], out_dir: Path) -> int:
    """Copy the selected entries into the tracked tree, then refresh the manifest.

    The whole selection is checked before any copy, so a half-built group leaves
    the committed artifacts untouched instead of partially republished.
    """
    sources = [(row, out_dir / row.artifact) for row in rows]
    missing = [str(path) for _, path in sources if not path.is_file()]
    if missing:
        listing = "\n  ".join(missing)
        groups = sorted({row.group for row in rows})
        recipes = ", ".join(REBUILD_RECIPE[group] for group in groups)
        raise ArtifactMapError(
            f"missing build output for the {groups[0]} publish group in {out_dir}:\n  {listing}\n"
            f"Build it first ({recipes}) so OUT_DIR holds every entry it publishes."
        )
    for row, path in sources:
        target = ARTIFACTS_DIR / row.artifact
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
        print(f"  published artifacts/{row.artifact}")
    return write_manifest()


def _normalize_hex(value: str) -> str:
    text = value[2:] if value.lower().startswith("0x") else value
    return text.lower()


def _load_artifact(path: Path) -> dict:
    if not path.is_file():
        raise ArtifactMapError(f"missing artifact {path}")
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArtifactMapError(f"unreadable artifact {path}: {error}") from error
    if not isinstance(payload, dict):
        raise ArtifactMapError(f"artifact {path} is not a JSON object")
    return payload


def _bytecode_field(payload: dict, field: str, path: Path) -> str:
    section, _, key = field.partition(".")
    node = payload.get(section)
    value = node.get(key) if isinstance(node, dict) else None
    if not isinstance(value, str):
        raise ArtifactMapError(f"{path} is missing `{field}`")
    text = _normalize_hex(value)
    if not text or not HEX_DIGITS.issuperset(text):
        raise ArtifactMapError(f"{path} has a non-hex `{field}`")
    return text


def verify_rows(rows: Sequence[Row], out_dir: Path) -> bool:
    """Byte-compare every selected entry against its committed artifact."""
    ok = True
    for row in rows:
        committed_path = ARTIFACTS_DIR / row.artifact
        rebuilt_path = out_dir / row.artifact
        try:
            committed = _load_artifact(committed_path)
            rebuilt = _load_artifact(rebuilt_path)
            compared = {
                field: (
                    _bytecode_field(committed, field, committed_path),
                    _bytecode_field(rebuilt, field, rebuilt_path),
                )
                for field in CHECKED_FIELDS
            }
        except ArtifactMapError as error:
            print(f"  \u2717 {row.artifact}: {error}")
            ok = False
            continue
        drifted = [
            field
            for field, (committed_hex, rebuilt_hex) in compared.items()
            if committed_hex != rebuilt_hex
        ]
        if drifted:
            print(f"  \u2717 {row.artifact}: committed {', '.join(drifted)} \u2260 fresh compile")
            print(
                f"    Rebuild + re-commit: {REBUILD_RECIPE[row.group]} (PUBLISH=1), "
                "then commit tier3-oracle/artifacts/."
            )
            ok = False
        else:
            print(f"  \u2713 {row.artifact}: creation code + deployed runtime match fresh compile")
    return ok


def _add_selection(parser: argparse.ArgumentParser, *, group_required: bool = False) -> None:
    parser.add_argument("--group", choices=PUBLISH_GROUPS, required=group_required)
    parser.add_argument("--script", help="restrict to one build script's rows")


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    commands = parser.add_subparsers(dest="command", required=True)

    commands.add_parser("scripts", help="unique build scripts, in map order")

    rows_parser = commands.add_parser("rows", help="selected artifact entries")
    _add_selection(rows_parser)

    stage_parser = commands.add_parser("stage", help="lay raw compiler output out under OUT_DIR")
    stage_parser.add_argument("--raw-dir", required=True, help="the compiler's raw output root")
    stage_parser.add_argument("--out-dir", help=f"default: {DEFAULT_OUT_DIR}")
    _add_selection(stage_parser, group_required=True)

    publish_parser = commands.add_parser("publish", help="republish a group + its manifest")
    publish_parser.add_argument("--out-dir", help=f"default: {DEFAULT_OUT_DIR}")
    _add_selection(publish_parser)

    commands.add_parser("manifest", help="rewrite artifacts/manifest.json from the map")

    verify_parser = commands.add_parser("verify", help="compile-vs-use byte comparison")
    verify_parser.add_argument("--out-dir", required=True)
    _add_selection(verify_parser)

    args = parser.parse_args(argv)

    try:
        if args.command == "scripts":
            seen: list[str] = []
            for row in load_map():
                if row.build_script not in seen:
                    seen.append(row.build_script)
            print("\n".join(seen))
            return 0
        if args.command == "rows":
            print("\n".join(row.artifact for row in select(load_map(), **vars_of(args))))
            return 0
        if args.command == "stage":
            return stage_rows(
                select(load_map(), **vars_of(args)),
                Path(args.raw_dir).resolve(),
                _out_dir(args.out_dir),
            )
        if args.command == "publish":
            return publish_rows(select(load_map(), **vars_of(args)), _out_dir(args.out_dir))
        if args.command == "manifest":
            return write_manifest()
        if args.command == "verify":
            rows = select(load_map(), **vars_of(args))
            if not verify_rows(rows, _out_dir(args.out_dir)):
                groups = list(dict.fromkeys(row.group for row in rows))
                recipes = " / ".join(REBUILD_RECIPE[group] for group in groups)
                print(
                    f"\nERROR: committed tier-3 artifact(s) do not match a fresh compile. "
                    f"Republish with PUBLISH=1 ({recipes}) and commit the refreshed artifacts/."
                )
                return 1
            return 0
    except ArtifactMapError as error:
        print(f"ERROR: {error}")
        return 1
    raise AssertionError(f"unhandled command {args.command}")


def vars_of(args: argparse.Namespace) -> dict[str, str | None]:
    """The `--group`/`--script` selection, whichever the subcommand declares."""
    return {"group": getattr(args, "group", None), "script": getattr(args, "script", None)}


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
