"""Print one top-level field of a `cargo metadata` payload read from stdin.

`cargo metadata` writes a large JSON document; a shell pipeline that needs one
value (the target directory, the workspace root) should not carry its own
interpreter invocation. Keeping the read here means the behaviour is linted,
formatted and unit-tested like every other script.

    cargo metadata --format-version 1 --no-deps | python3 scripts/cargo_metadata_field.py target_directory
"""

from __future__ import annotations

import json
import sys
from typing import Any


def field_value(metadata: dict[str, Any], name: str) -> str:
    """Return one top-level field of the payload as text.

    Args:
        metadata: A parsed `cargo metadata` payload.
        name: The top-level key to read.

    Returns:
        The field's value as a string; a JSON null becomes the empty string.
    """
    value = metadata.get(name)
    return "" if value is None else str(value)


def main(argv: list[str]) -> int:
    """Print the requested field for the payload on stdin.

    Args:
        argv: One argument: the field name.

    Returns:
        A process exit status; 2 when the field name is missing.
    """
    if len(argv) != 1:
        sys.stderr.write("usage: cargo_metadata_field.py <field>\n")
        return 2
    metadata = json.load(sys.stdin)
    sys.stdout.write(field_value(metadata, argv[0]) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
