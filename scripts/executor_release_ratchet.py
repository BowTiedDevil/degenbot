#!/usr/bin/env python3
"""Generate and enforce the immutable cmd_executor release index.

The generated index binds every accepted release package to the SHA-256 of its
contract, manifest, artifacts, and package descriptor. ``check`` performs two
independent checks: the committed index must exactly match regeneration, and
every package in the base index must survive byte-for-byte unchanged. Editing a
frozen file and regenerating therefore cannot launder a semantic change.

live-invariant gate (ADR-087 D2): watches accepted cmd_executor release packages and their sequential release index; not tripwire precedent.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

RELEASES_DIR = Path("rust/crates/foundation/degenbot-executor/releases")
INDEX_PATH = RELEASES_DIR / "index.json"
SCHEMA_VERSION = 1
FIRST_TRACKED_RELEASE = 2
_HEX_256 = re.compile(r"^[0-9a-f]{64}$")


class GateError(Exception):
    """The gate cannot make a trustworthy determination."""


def canonical(payload: dict[str, Any]) -> str:
    return json.dumps(payload, indent=2, sort_keys=True) + "\n"


def sha256(path: Path) -> str:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError as error:
        raise GateError(f"required release file {path} is unreadable: {error}") from error


def relative_file(root: Path, value: str) -> Path:
    path = Path(value)
    if path.is_absolute() or ".." in path.parts:
        raise GateError(f"release file path must stay inside the repository: {value!r}")
    return root / path


def package_descriptor(root: Path, descriptor: Path) -> dict[str, Any]:
    try:
        payload = json.loads(descriptor.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise GateError(f"unreadable release descriptor {descriptor}: {error}") from error
    if set(payload) != {"release_id", "ordinal", "files"}:
        raise GateError(f"{descriptor}: expected exactly release_id, ordinal, and files")
    ordinal = payload["ordinal"]
    if type(ordinal) is not int or ordinal < FIRST_TRACKED_RELEASE:
        raise GateError(f"{descriptor}: invalid release ordinal {ordinal!r}")
    if descriptor.parent.name != f"v{ordinal}":
        raise GateError(f"{descriptor}: directory does not match ordinal {ordinal}")
    if payload["release_id"] != f"cmd_executor/{ordinal}":
        raise GateError(f"{descriptor}: release_id does not match ordinal {ordinal}")
    declared = payload["files"]
    if (
        not isinstance(declared, list)
        or not declared
        or any(not isinstance(value, str) or not value for value in declared)
    ):
        raise GateError(f"{descriptor}: files must be a non-empty string list")
    if len(declared) != len(set(declared)):
        raise GateError(f"{descriptor}: duplicate file declaration")

    descriptor_relative = descriptor.relative_to(root).as_posix()
    paths = sorted({*declared, descriptor_relative})
    hashes = {value: sha256(relative_file(root, value)) for value in paths}
    return {
        "release_id": payload["release_id"],
        "ordinal": ordinal,
        "files": hashes,
    }


def build_index(root: Path) -> dict[str, Any]:
    release_root = root / RELEASES_DIR
    descriptors = sorted(release_root.glob("v*/package.json"))
    if not descriptors:
        raise GateError(f"no release packages found under {release_root}")
    packages = [package_descriptor(root, descriptor) for descriptor in descriptors]
    ordinals = [package["ordinal"] for package in packages]
    expected = list(range(FIRST_TRACKED_RELEASE, FIRST_TRACKED_RELEASE + len(packages)))
    if ordinals != expected:
        raise GateError(f"release sequence must be {expected}, got {ordinals}")

    owners: dict[str, str] = {}
    for package in packages:
        for value in package["files"]:
            previous = owners.get(value)
            if previous is not None:
                raise GateError(
                    f"release file {value} is shared by {previous} and {package['release_id']}; "
                    "each release owns its package files"
                )
            owners[value] = package["release_id"]
    return {"schema_version": SCHEMA_VERSION, "packages": packages}


def validate_index(payload: Any, origin: str) -> list[dict[str, Any]]:
    if not isinstance(payload, dict) or set(payload) != {"schema_version", "packages"}:
        raise GateError(f"{origin}: expected exactly schema_version and packages")
    if payload["schema_version"] != SCHEMA_VERSION:
        raise GateError(f"{origin}: unsupported schema_version {payload['schema_version']!r}")
    packages = payload["packages"]
    if not isinstance(packages, list) or not packages:
        raise GateError(f"{origin}: packages must be a non-empty list")
    for package in packages:
        if not isinstance(package, dict) or set(package) != {"release_id", "ordinal", "files"}:
            raise GateError(f"{origin}: malformed package entry {package!r}")
        ordinal = package["ordinal"]
        if type(ordinal) is not int or package["release_id"] != f"cmd_executor/{ordinal}":
            raise GateError(f"{origin}: release tag/ordinal mismatch in {package!r}")
        files = package["files"]
        if not isinstance(files, dict) or not files:
            raise GateError(f"{origin}: {package['release_id']} has no file hashes")
        for value, digest in files.items():
            if (
                not isinstance(value, str)
                or not isinstance(digest, str)
                or not _HEX_256.fullmatch(digest)
            ):
                raise GateError(f"{origin}: invalid file hash in {package['release_id']}")
    ordinals = [package["ordinal"] for package in packages]
    expected = list(range(FIRST_TRACKED_RELEASE, FIRST_TRACKED_RELEASE + len(packages)))
    if ordinals != expected:
        raise GateError(f"{origin}: release sequence must be {expected}, got {ordinals}")
    return packages


def read_index(path: Path) -> tuple[str, list[dict[str, Any]]]:
    try:
        text = path.read_text(encoding="utf-8")
        payload = json.loads(text)
    except (OSError, json.JSONDecodeError) as error:
        raise GateError(f"unreadable release index {path}: {error}") from error
    return text, validate_index(payload, str(path))


def base_index_from_git(root: Path) -> list[dict[str, Any]] | None:
    explicit = os.environ.get("DEGENBOT_RELEASE_RATCHET_BASE")
    if explicit:
        base = explicit
    else:
        upstream = subprocess.run(
            ["git", "merge-base", "HEAD", "@{upstream}"],
            cwd=root,
            text=True,
            capture_output=True,
            check=False,
        )
        base = upstream.stdout.strip() if upstream.returncode == 0 else "HEAD"
    result = subprocess.run(
        ["git", "show", f"{base}:{INDEX_PATH.as_posix()}"],
        cwd=root,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        return None
    return validate_index(json.loads(result.stdout), f"{base}:{INDEX_PATH}")


def check(root: Path, base_index: Path | None) -> list[str]:
    expected = canonical(build_index(root))
    committed_path = root / INDEX_PATH
    committed_text, current = read_index(committed_path)
    complaints: list[str] = []
    if committed_text != expected:
        complaints.append(
            f"{INDEX_PATH}: generated index drift; regenerate only while adding a new release"
        )

    base = read_index(base_index)[1] if base_index is not None else base_index_from_git(root)
    if base is not None:
        current_by_id = {package["release_id"]: package for package in current}
        for previous in base:
            release_id = previous["release_id"]
            present = current_by_id.get(release_id)
            if present is None:
                complaints.append(f"{release_id}: accepted release package was removed")
            elif present != previous:
                before = previous["files"]
                after = present["files"]
                for value in sorted(set(before) | set(after)):
                    if before.get(value) != after.get(value):
                        complaints.append(f"{release_id}: frozen file changed: {value}")
    return complaints


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("generate", "check"))
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--base-index", type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        if args.command == "generate":
            (root / INDEX_PATH).write_text(canonical(build_index(root)), encoding="utf-8")
            print(f"generated {INDEX_PATH}")
            return 0
        complaints = check(root, args.base_index)
    except GateError as error:
        print(f"executor release ratchet error: {error}", file=sys.stderr)
        return 2
    if complaints:
        for complaint in complaints:
            print(f"executor release ratchet: {complaint}", file=sys.stderr)
        return 1
    print(f"executor release ratchet: clean ({INDEX_PATH})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
