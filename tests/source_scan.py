"""In-process source scanning for the grep-shaped gates.

Two gates ask the tree grep-shaped questions — which files mention these
identifiers, and where is this literal spelled — and both answer in-process.
A gate whose verdict depends on a ripgrep binary being installed reports the
environment when the tool is absent rather than the code under test, so the scan
is ordinary Python over the tracked sources.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterable, Iterator, Sequence

ROOT = Path(__file__).resolve().parents[1]

# Build output, caches, and vendored trees never carry gate evidence.
_SKIP_DIRS = frozenset({
    "__pycache__",
    ".git",
    ".pytest_cache",
    ".ruff_cache",
    ".venv",
    "node_modules",
    "site-packages",
    "target",
})

# The word shape every config key and Rust/Python identifier has.
_TOKEN = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def iter_text_files(
    roots: Sequence[str], suffixes: Sequence[str] | None = None
) -> Iterator[tuple[str, str]]:
    """Yield `(repo-relative path, text)` for each text file under `roots`.

    `suffixes` narrows the walk (the identifier gates read only Rust and Python);
    `None` walks everything, which is what a literal gate needs so a pin spelled
    in a data or config file cannot hide. Binary files are skipped and the walk is
    sorted, so a failure message is stable across runs.
    """
    for root in roots:
        base = ROOT / root
        candidates = [base] if base.is_file() else sorted(base.rglob("*"))
        for path in candidates:
            if not path.is_file():
                continue
            if suffixes is not None and path.suffix not in suffixes:
                continue
            relative = path.relative_to(ROOT)
            if _SKIP_DIRS.intersection(relative.parts[:-1]):
                continue
            data = path.read_bytes()
            if b"\0" in data:
                continue
            yield relative.as_posix(), data.decode("utf-8", errors="replace")


def identifiers(
    fields: Iterable[str], roots: Sequence[str], suffixes: Sequence[str] = (".rs", ".py")
) -> dict[str, set[str]]:
    """Map each identifier to the files under `roots` that mention it as a word.

    Every requested field is a key of the result, so "read nowhere" is an empty
    set rather than a missing entry.
    """
    hits: dict[str, set[str]] = {field: set() for field in fields}
    for path, text in iter_text_files(roots, suffixes):
        for token in _TOKEN.finditer(text):
            word = token.group(0)
            if word in hits:
                hits[word].add(path)
    return hits


def literal_hits(literal: str, roots: Sequence[str]) -> list[str]:
    """Every `path:line:text` under `roots` that spells `literal` verbatim."""

    hits: list[str] = []
    for path, text in iter_text_files(roots):
        if literal not in text:
            continue
        for number, line in enumerate(text.splitlines(), start=1):
            if literal in line:
                hits.append(f"{path}:{number}:{line}")
    return hits
