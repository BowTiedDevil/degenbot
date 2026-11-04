"""Shared plumbing for the oracle-snapshot generators.

Each generator under this directory names its own output path, renders the whole
snapshot body from the oracle reference implementation beside it, and either
writes that body to `--out` (default `target/oracle-regen/<snapshot name>`, which
`target/` being gitignored keeps out of the tree) or compares it byte-for-byte
against the frozen snapshot with `--check`.

The frozen snapshots under `rust/crates/foundation/degenbot-math/tests/` are read
only. They are the parity ground truth the cross-check tests replay, so a
regeneration that disagrees with one is a finding to report, never a file to
overwrite.
"""

from __future__ import annotations

import argparse
import difflib
import sys
from collections.abc import Callable
from pathlib import Path

DEFAULT_OUT_DIR = Path(__file__).resolve().parents[2] / "target/oracle-regen"


def _record_count(body: str) -> str:
    """Describe a snapshot body's shape.

    Args:
        body: The snapshot body.

    Returns:
        The record count for a line-oriented body, or the JSON-object note.
    """
    if body.endswith("\n"):
        return f"{body.count(chr(10))} records"
    return "one JSON object"


def _summarize(label: str, rendered: str, frozen: str) -> int:
    """Report whether two snapshot bodies agree.

    Args:
        label: Human label for the body being compared.
        rendered: The freshly rendered body.
        frozen: The frozen snapshot's body.

    Returns:
        `0` when the bodies are byte-identical, `1` otherwise.
    """
    if rendered == frozen:
        print(f"{label}: MATCH — {len(frozen.encode())} bytes, {_record_count(frozen)}")
        return 0

    print(
        f"{label}: MISMATCH — frozen {len(frozen.encode())} bytes vs regenerated {len(rendered.encode())} bytes"
    )
    diff = difflib.unified_diff(
        frozen.splitlines(keepends=True),
        rendered.splitlines(keepends=True),
        fromfile="frozen",
        tofile="regenerated",
        n=1,
    )
    shown = 0
    for line in diff:
        print(line.rstrip("\n"))
        shown += 1
        if shown >= 40:
            print("... (diff truncated)")
            break
    return 1


def run_generator(
    argv: list[str],
    *,
    snapshot: Path,
    render: Callable[[], str],
) -> int:
    """Write or check one generator's snapshot body.

    Args:
        argv: This process's `sys.argv`.
        snapshot: The frozen snapshot the body is compared against.
        render: Renders the complete snapshot body.

    Returns:
        The process exit status: `0` on a written body or a byte-identical
        comparison, `1` on a mismatch.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=DEFAULT_OUT_DIR / snapshot.name,
        help="where to write the regenerated snapshot body (default: %(default)s)",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="compare against the frozen snapshot instead of writing",
    )
    args = parser.parse_args(argv[1:])

    body = render()

    if args.check:
        frozen = snapshot.read_text()
        return _summarize(snapshot.name, body, frozen)

    if args.out.resolve() == snapshot.resolve():
        print(
            f"refusing to write over the frozen snapshot: {snapshot}",
            file=sys.stderr,
        )
        return 1

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(body)
    print(f"wrote {args.out} ({len(body.encode())} bytes, {_record_count(body)})")
    print(f"frozen snapshot untouched: {snapshot}")
    return 0


if __name__ == "__main__":
    sys.exit("import this module from a generator; it is not a program")
