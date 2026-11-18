"""Print the `--exclude` flags that keep vendored members out of a workspace lane.

Vendored crates are members of the workspace so their manifests resolve, but a
lane invocation (`cargo test --workspace`, `cargo publish`) must never select
them: they are pinned copies, not code this repository owns.
"""

from __future__ import annotations

import json
import sys
from typing import Any


def exclusions(metadata: dict[str, Any]) -> list[str]:
    """Return the `--exclude <name>` token pairs for every vendored member.

    Args:
        metadata: A `cargo metadata --no-deps` payload.

    Returns:
        Flags in argv order, ready to splice into a cargo invocation.
    """
    flags: list[str] = []
    for package in metadata["packages"]:
        if "/vendor/" in str(package.get("manifest_path", "")):
            flags.extend(["--exclude", package["name"]])
    return flags


def main() -> int:
    """Print the flags for the metadata on stdin.

    Returns:
        A process exit status.
    """
    metadata = json.load(sys.stdin)
    flags = exclusions(metadata)
    sys.stdout.write("".join(f"{flag} " for flag in flags) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
