#!/usr/bin/env python3
"""The worker-census spawn-site gate (docs/gates.md section 3).
Failure class: a thread or tokio-runtime spawn site that never registers a
`WorkerCensusEntry` is invisible to the `degenbot_worker_census` gauge, the one
boot dump, and the /proc comm cross-check. The registry module stated the rule
("if you add a spawn site you must register it") and documentation is not a
mechanism, so this gate is the mechanism.

**The rule.** A spawn site must sit in a function that registers. Concretely, a
site passes when its enclosing function does one of:
* calls `worker_census::register` itself, or
* calls a helper in the same file that does -- the one-hop delegation shape
  `OwnedPubsubRuntime::build` uses to publish its live count through `publish`.

A site the rule does not sanction is reported by file, line and function, and is
admitted only by a row in the inventory below, under a written reason.

**Scope.** Every `*.rs` file under `rust/crates` -- crate sources, integration
tests, benches and examples, inline `cfg(test)` modules included -- because a
test thread is as invisible to the /proc cross-check as a production one. The
scan reads the working tree (a brand-new untracked source file is examined the
moment it is written); `rust/target` is skipped.

**The four spawn shapes.** `thread::Builder::new` (a std thread behind a
builder), `thread::spawn` (a bare one), `Builder::new_multi_thread` and
`Builder::new_current_thread` (a tokio runtime). The tokio shapes are matched
without their `tokio::runtime::` path so an imported or aliased builder is still
a site; the two std shapes are matched with their `thread::` path, so
`std::thread::spawn` and an imported `thread::spawn` are both sites while the
many unrelated `*Builder::new()` constructors (`ProviderBuilder`,
`AnvilForkBuilder`, `tempfile::Builder`) are not.

**Inventory.** `scripts/hooks/worker-census-spawn-inventory.txt`, one
`path:function` row per unsanctioned site (a site's identity is its file and its
enclosing function, the same unit the registration rule speaks about, so a row
survives a line reflow). The reconciliation is bidirectional and fails closed: a
site with no row fails, a row with no live site fails, a row with no `#` reason
above it fails, and a missing inventory file fails instead of certifying an
empty census. The inventory is a ratchet: it grows only with a reviewed reason,
and a row is deleted in the change that registers (or removes) its site.

**Blind spots.** The matcher is lexical, not a parser: a spawn built behind a
re-spelled binding (`use tokio::runtime::Builder as B`) is not a site, nor is one
reached through a renamed std import (`use std::thread as t;`), and a
`rayon::ThreadPoolBuilder`, a `crossbeam` scope thread, or a capacity constant
that bounds concurrent execution is out of the rule's scope -- the rule covers
std threads and tokio runtimes. A site outside any function (a macro body, a
`const` initializer) is unsanctioned by construction, so it fails rather than
passing unseen. The one-hop delegation check is name-based within one file, so a
same-named helper can sanction a call it does not own.

Exit codes: 0 clean, 1 drift (each complaint on stderr), 2 the gate cannot
certify what it was pointed at.

**Red path, recorded as the two-run probe** (the shape the comment-hygiene gate
records, and the one `tests/test_worker_census_spawn_gate.py` pins): run one over
a tree holding a `std::thread::Builder::new()` in a function that never registers
fails, naming `path:line` and the row to add; run two, with that row in the
inventory, is green. The same probe was recorded for the bare shape when the
matcher grew its fourth token: a fixture holding `std::thread::spawn(|| {})` inside
`spawn_helper` failed naming `rust/crates/fixture-crate/src/spawn_site.rs:2` and
the row `rust/crates/fixture-crate/src/spawn_site.rs:spawn_helper`, and was green
once that row landed -- and the registering form of the same fixture, whose
function calls `worker_census::register` above the spawn, is green with no row at
all. `--rows` prints the rows the inventory does not hold yet, so adding an entry
is a paste-and-review edit.

live-invariant gate (ADR-087 D2): watches the live spawn sites of the crate tree against their registrations and the reviewed inventory; not tripwire precedent.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Sequence
_REPO_ROOT = Path(__file__).resolve().parents[2]
_SCAN_ROOT = "rust/crates"
_DEFAULT_INVENTORY = "scripts/hooks/worker-census-spawn-inventory.txt"
_SKIP_DIRS = frozenset({"target", "vendor"})
_SPAWN_TOKENS = (
    ("thread::Builder::new", "std thread"),
    ("thread::spawn", "bare std thread"),
    ("Builder::new_multi_thread", "tokio multi-thread runtime"),
    ("Builder::new_current_thread", "tokio current-thread runtime"),
)
_REGISTER_QUALIFIED = re.compile(r"worker_census::register\s*\(")
_REGISTER_MODULE_LOCAL = re.compile(r"(?<![A-Za-z0-9_:.])register\s*\(")
_OUTSIDE_A_FUNCTION = "<outside a function>"
_FN_DECL = re.compile(r"(?<![A-Za-z0-9_])fn\s+(?P<name>[A-Za-z0-9_]+)")


class GateError(Exception):
    """The gate cannot certify the tree or the inventory it was pointed at."""


@dataclass(frozen=True)
class Site:
    """One spawn site: where it is, the function that must register it, and its shape."""

    path: str
    line: int
    function: str
    kind: str

    @property
    def row(self) -> str:
        """The inventory row that admits this site."""
        return f"{self.path}:{self.function}"

    def report(self) -> str:
        return f"  fresh: {self.path}:{self.line}: {self.function} ({self.kind}) -> inventory row: {self.row}"


@dataclass(frozen=True)
class Function:
    """One `fn` item and the extent of its body."""

    name: str
    body_start: int
    body_end: int


def _is_ident(char: str) -> bool:
    return char.isalnum() or char == "_"


def _blank(chars: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if chars[index] != "\n":
            chars[index] = " "


def _plain_string_end(source: str, at: int) -> int:
    """The offset just past the string literal whose opening quote is at `at`."""
    index = at + 1
    while index < len(source):
        char = source[index]
        if char == "\\":
            index += 2
            continue
        if char == '"':
            return index + 1
        index += 1
    return len(source)


def _raw_string_end(source: str, at: int, hashes: int) -> int:
    closing = '"' + "#" * hashes
    found = source.find(closing, at)
    return len(source) if found < 0 else found + len(closing)


def _prefixed_literal_end(source: str, at: int) -> int | None:
    """The end of a raw or byte string literal starting at `at`, if one does."""
    for prefix in ("br", "rb", "r", "b"):
        if not source.startswith(prefix, at):
            continue
        cursor = at + len(prefix)
        hashes = 0
        while source[cursor : cursor + 1] == "#":
            hashes += 1
            cursor += 1
        if source[cursor : cursor + 1] != '"':
            continue
        if prefix == "b":
            return _plain_string_end(source, cursor)
        return _raw_string_end(source, cursor + 1, hashes)
    return None


def _char_literal_end(source: str, at: int) -> int | None:
    """The end of a char literal at `at`, or None when `at` opens a lifetime."""
    if source[at + 1 : at + 2] == "\\":
        cursor = at + 2
        while cursor < len(source) and source[cursor] not in {"'", "\n"}:
            cursor += 1
        return cursor + 1 if source[cursor : cursor + 1] == "'" else None
    if source[at + 2 : at + 3] == "'":
        return at + 3
    return None


def _mask(source: str) -> str:
    """`source` with comments and literal payloads read as spaces.
    Offsets and line breaks survive, so a token found here is a token in code: a
    module doc naming `thread::Builder::new` is prose, not a spawn site.
    """
    chars = list(source)
    length = len(source)
    index = 0
    while index < length:
        char = source[index]
        pair = source[index : index + 2]
        if pair == "//":
            end = source.find("\n", index)
            end = length if end < 0 else end
            _blank(chars, index, end)
            index = end
        elif pair == "/*":
            depth = 1
            cursor = index + 2
            while cursor < length and depth:
                if source.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif source.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            _blank(chars, index, cursor)
            index = cursor
        elif char == '"':
            end = _plain_string_end(source, index)
            _blank(chars, index, end)
            index = end
        elif char == "'":
            end = _char_literal_end(source, index)
            if end is None:
                index += 1
            else:
                _blank(chars, index, end)
                index = end
        elif char in "rb" and (index == 0 or not _is_ident(source[index - 1])):
            end = _prefixed_literal_end(source, index)
            if end is None:
                index += 1
            else:
                _blank(chars, index, end)
                index = end
        else:
            index += 1
    return "".join(chars)


def _brace_end(masked: str, at: int) -> int:
    """The offset just past the brace that closes the one at `at`."""
    depth = 0
    cursor = at
    while cursor < len(masked):
        char = masked[cursor]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return cursor + 1
        cursor += 1
    return len(masked)


def _body_span(masked: str, at: int) -> tuple[int, int] | None:
    """The body extent of the `fn` whose name ends at `at`, or None for a declaration."""
    depth = 0
    cursor = at
    limit = min(len(masked), at + 2000)
    while cursor < limit:
        char = masked[cursor]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth = max(0, depth - 1)
        elif depth == 0 and char == "{":
            return (cursor, _brace_end(masked, cursor))
        elif depth == 0 and char in ";}":
            return None
        cursor += 1
    return None


def _functions(masked: str) -> list[Function]:
    found: list[Function] = []
    for match in _FN_DECL.finditer(masked):
        span = _body_span(masked, match.end())
        if span is None:
            continue
        found.append(Function(match.group("name"), span[0], span[1]))
    return found


def _body_without_nested(masked: str, functions: list[Function], index: int) -> str:
    """One function's own code, with the bodies of the `fn` items it declares cut out."""
    function = functions[index]
    chunks: list[str] = []
    cursor = function.body_start
    for other_index, other in enumerate(functions):
        if other_index == index:
            continue
        if other.body_start <= function.body_start or other.body_end > function.body_end:
            continue
        if other.body_start < cursor:
            continue
        chunks.append(masked[cursor : other.body_start])
        cursor = other.body_end
    chunks.append(masked[cursor : function.body_end])
    return "".join(chunks)


def _enclosing_index(functions: list[Function], position: int) -> int | None:
    enclosing = [
        index
        for index, function in enumerate(functions)
        if function.body_start <= position < function.body_end
    ]
    if not enclosing:
        return None
    return max(enclosing, key=lambda index: functions[index].body_start)


def _sanctioned(
    index: int,
    functions: list[Function],
    bodies: list[str],
    registering: dict[str, re.Pattern[str]],
    register_patterns: tuple[re.Pattern[str], ...],
) -> bool:
    body = bodies[index]
    if any(pattern.search(body) is not None for pattern in register_patterns):
        return True
    own_name = functions[index].name
    return any(
        pattern.search(body) is not None
        for name, pattern in registering.items()
        if name != own_name
    )


def _sites_in(relative: str, text: str) -> tuple[list[Site], int]:
    """The unsanctioned sites of one source, and how many sites it registers in place."""
    masked = _mask(text)
    functions = _functions(masked)
    bodies = [_body_without_nested(masked, functions, index) for index in range(len(functions))]
    register_patterns = [_REGISTER_QUALIFIED]
    if any(function.name == "register" for function in functions):
        register_patterns.append(_REGISTER_MODULE_LOCAL)
    registering = {
        function.name: re.compile(rf"\b{re.escape(function.name)}\s*\(")
        for index, function in enumerate(functions)
        if any(pattern.search(bodies[index]) is not None for pattern in register_patterns)
    }
    sites: list[Site] = []
    registered = 0
    for token, kind in _SPAWN_TOKENS:
        start = 0
        while (position := masked.find(token, start)) >= 0:
            start = position + len(token)
            index = _enclosing_index(functions, position)
            if index is not None and _sanctioned(
                index, functions, bodies, registering, tuple(register_patterns)
            ):
                registered += 1
                continue
            function = _OUTSIDE_A_FUNCTION if index is None else functions[index].name
            sites.append(
                Site(
                    path=relative,
                    line=text.count("\n", 0, position) + 1,
                    function=function,
                    kind=kind,
                )
            )
    return sites, registered


@dataclass(frozen=True)
class Scan:
    """What one tree walk found: the unsanctioned sites, the registered ones, the sources read."""

    sites: tuple[Site, ...]
    registered: int
    examined: int


def _scan(root: Path) -> Scan:
    scan_root = root / _SCAN_ROOT
    if not scan_root.is_dir():
        raise GateError(f"nothing to scan: {scan_root} is not a directory")
    sites: list[Site] = []
    registered = 0
    examined = 0
    for path in sorted(scan_root.rglob("*.rs")):
        if _SKIP_DIRS.intersection(path.parts):
            continue
        examined += 1
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as err:
            raise GateError(f"cannot read {path}: {err}") from err
        if not any(token in text for token, _ in _SPAWN_TOKENS):
            continue
        found, in_place = _sites_in(path.relative_to(root).as_posix(), text)
        sites.extend(found)
        registered += in_place
    return Scan(sites=tuple(sites), registered=registered, examined=examined)


@dataclass(frozen=True)
class Inventory:
    """The reviewed exception list: rows that admit a site, and rows that explain nothing."""

    rows: frozenset[str]
    unreasoned: tuple[str, ...]


def _parse_inventory(path: Path) -> Inventory:
    if not path.exists():
        raise GateError(f"inventory missing: {path} (an absent census cannot certify a clean tree)")
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as err:
        raise GateError(f"cannot read {path}: {err}") from err
    rows: set[str] = set()
    unreasoned: list[str] = []
    reasoned = False
    for raw in lines:
        line = raw.strip()
        if not line:
            reasoned = False
            continue
        if line.startswith("#"):
            reasoned = True
            continue
        rows.add(line)
        if not reasoned:
            unreasoned.append(line)
        reasoned = True
    return Inventory(rows=frozenset(rows), unreasoned=tuple(unreasoned))


def _reconcile(
    sites: tuple[Site, ...], inventory: Inventory
) -> tuple[list[Site], list[str], list[str]]:
    listed = inventory.rows
    fresh = [site for site in sites if site.row not in listed]
    live = {site.row for site in sites}
    stale = sorted(row for row in listed if row not in live)
    return fresh, stale, list(inventory.unreasoned)


def _relative(path: Path, base: Path) -> str:
    try:
        return path.relative_to(base).as_posix()
    except ValueError:
        return str(path)


def _report(
    fresh: list[Site],
    stale: list[str],
    unreasoned: list[str],
    inventory: Path,
    base: Path,
) -> None:
    if fresh:
        failed = "\n".join(site.report() for site in fresh)
        print(
            "worker-census-spawn: a spawn site with no census registration in its function",
            file=sys.stderr,
        )
        print(failed, file=sys.stderr)
    if stale:
        print(
            "worker-census-spawn: an inventory row with no live spawn site",
            file=sys.stderr,
        )
        for row in stale:
            print(f"  stale: {row} -> delete this row", file=sys.stderr)
    if unreasoned:
        print(
            "worker-census-spawn: an inventory row with no reviewed reason above it",
            file=sys.stderr,
        )
        for row in unreasoned:
            print(f"  unreasoned: {row}", file=sys.stderr)
    print(
        "  fix: register the worker at the spawn site "
        "(degenbot_core::worker_census::register), or add a reviewed row to "
        f"{_relative(inventory, base)} (docs/gates.md section 3).",
        file=sys.stderr,
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else None)
    parser.add_argument(
        "--root",
        help="tree to scan (default: the repository root); the negative-probe test seam",
    )
    parser.add_argument(
        "--inventory",
        help=f"inventory file to reconcile against (default: {_DEFAULT_INVENTORY})",
    )
    parser.add_argument(
        "--rows",
        action="store_true",
        help="print the candidate inventory rows for the sites the rule does not sanction",
    )
    return parser


def _candidate_rows(inventory_path: Path, scan: Scan) -> int:
    """Print the rows the inventory does not hold yet, for a developer to review."""
    listed = _parse_inventory(inventory_path).rows if inventory_path.exists() else frozenset()
    for row in sorted({site.row for site in scan.sites if site.row not in listed}):
        print(row)
    return 0


def _certify(base: Path, inventory_path: Path, *, rows_only: bool) -> int:
    """The gate body: scan, reconcile, report. Raises `GateError` when it cannot."""
    scan = _scan(base)
    if scan.examined == 0:
        msg = f"nothing examined under {base / _SCAN_ROOT}"
        raise GateError(msg)
    if not scan.sites and not scan.registered:
        msg = (
            "no spawn site found in the scanned tree: the matcher has gone blind, "
            "refusing to certify"
        )
        raise GateError(msg)
    if rows_only:
        return _candidate_rows(inventory_path, scan)
    inventory = _parse_inventory(inventory_path)
    fresh, stale, unreasoned = _reconcile(scan.sites, inventory)
    if fresh or stale or unreasoned:
        _report(fresh, stale, unreasoned, inventory_path, base)
        return 1
    print(
        f"worker-census-spawn: {len(scan.sites) + scan.registered} spawn sites over "
        f"{scan.examined} sources: {scan.registered} register in their own function, "
        f"{len(scan.sites)} admitted by {len(inventory.rows)} reviewed inventory rows"
    )
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    """Run the gate; see the module docs for the exit-code contract."""
    args = _parser().parse_args(argv)
    base = Path(args.root).resolve() if args.root else _REPO_ROOT
    inventory_path = Path(args.inventory) if args.inventory else base / _DEFAULT_INVENTORY
    try:
        return _certify(base, inventory_path, rows_only=args.rows)
    except GateError as err:
        print(f"worker-census-spawn: {err}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
