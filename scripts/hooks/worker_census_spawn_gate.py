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

**Scope: production spawn sites.** The scan reads the crate sources a production
process compiles and deliberately leaves out the code only a test build compiles:
a `tests/` or `benches/` cargo target, an inline region a `test` cfg predicate
gates (`#[cfg(test)]`, `#[cfg(all(test, feature = ...))]`), and the external files
those regions declare (`#[cfg(test)] mod tests;` -> `tests.rs`, `tests/mod.rs`,
and the modules those declare in turn). The scan reads the working tree (a
brand-new untracked source file is examined the moment it is written);
`rust/target` is skipped.

**This reverses the rule this gate shipped with** -- that a test thread was "as
invisible to the /proc cross-check as a production one", so every test-only spawn
site needed an inventory row. The protected consumers are the three production
observers: the `degenbot_worker_census{resource=...}` gauge `degenbot-bot::instruments`
exports at metrics init, the one boot dump `emit_boot_table` fires from the
drivers' boot prelude, and the `/proc/<pid>/task/*/comm` histogram an operator
compares against the gauge on a LIVE bot. A test process exports no gauge, dumps
no boot table, and is never the process that cross-check reads, so a test-only
spawn site has no reader to be invisible to. Registering one is worse than
useless: `register` UPSERTS one process-global table, so a `cfg(test)` row lands
in the very table the same binary's production assertions read -- the
`REGISTERED_RESOURCE_IDS` closed set, the comm-histogram cross-check -- polluting a
production contract with an id no production process can publish. A
feature-gated module is never test-only: `#[cfg(feature = "auto-initialize")]
pub mod test_gil` compiles into a shipped binary, so it stays in scope, rows and
all.

**The four spawn shapes.** `thread::Builder::new` (a std thread behind a
builder), `thread::spawn` (a bare one), `Builder::new_multi_thread` and
`Builder::new_current_thread` (a tokio runtime). The tokio shapes are matched
without their `tokio::runtime::` path so an imported or aliased builder is still
a site; the two std shapes are matched with their `thread::` path, so
`std::thread::spawn` and an imported `thread::spawn` are both sites while the
many unrelated `*Builder::new()` constructors (`ProviderBuilder`,
`AnvilForkBuilder`, `tempfile::Builder`) are not.

**Inventory.** `scripts/hooks/worker-census-spawn-inventory.txt`, one
`path:function` row per unsanctioned production site (a site's identity is its
file and its enclosing function, the same unit the registration rule speaks
about, so a row survives a line reflow). The reconciliation is bidirectional and
fails closed: a site with no row fails, a row with no live site fails, a row with
no `#` reason above it fails, and a missing inventory file fails instead of
certifying an empty census. The inventory is a ratchet: it grows only with a
reviewed reason, and a row is deleted in the change that registers (or removes)
its site.

**Blind spots.** The matcher is lexical, not a parser: a spawn built behind a
re-spelled binding (`use tokio::runtime::Builder as B`) is not a site, nor is one
reached through a renamed std import (`use std::thread as t;`), and a
`rayon::ThreadPoolBuilder`, a `crossbeam` scope thread, or a capacity constant
that bounds concurrent execution is out of the rule's scope -- the rule covers
std threads and tokio runtimes. The test-scope matcher is lexical too: a module
reached only through `#[path = "..."]`, or a `cfg` predicate produced by a macro
expansion, is not seen, and the exclusion is scope, not permission -- a
production spawn site cannot borrow a test region's silence. A site outside any
function (a macro body, a `const` initializer) is unsanctioned by construction,
so it fails rather than passing unseen. The one-hop delegation check is name-based
within one file, so a same-named helper can sanction a call it does not own.

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
all. The scope's own red path is recorded in the same suite: a test target, an
inline `cfg(test)` region and an external test module are each silent under a
green gate next to a production site that is named, and a feature-only module is
named -- the controls that keep an exclusion from becoming a blind spot.
`--rows` prints the rows the inventory does not hold yet, so adding an entry is a
paste-and-review edit.

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
_TEST_TARGET_DIRS = frozenset({"tests", "benches"})
_CFG_ATTRIBUTE = re.compile(r"(?<![A-Za-z0-9_])cfg\s*\(")
_MOD_DECL = re.compile(r"(?<![A-Za-z0-9_])mod\s+([A-Za-z0-9_]+)\s*;")
_RAW_MOD_DECL = re.compile(r"(?<![A-Za-z0-9_])mod\s+[A-Za-z0-9_]+\s*;")
_MODULE_FILE_STEMS = frozenset({"mod.rs", "lib.rs", "main.rs"})
_MASK_TOKENS = re.compile(r"""//|/\*|br#*"|rb#*"|r#*"|b"|"|'""")
_BLOCK_DELIMITERS = re.compile(r"/\*|\*/")
_PLAIN_STRING = re.compile(r'"(?:\\.|[^"\\])*"', re.DOTALL)
_BRACES = re.compile(r"[{}]")
_ITEM_BREAKS = re.compile(r"[()\[\]{};,]")


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


@dataclass(frozen=True)
class Attribute:
    """One `#[...]` attribute: its extent, its form, and whether its cfg predicate names `test`."""

    start: int
    end: int
    inner: bool
    test: bool


def _blank(chars: list[str], start: int, end: int) -> None:
    chars[start:end] = [char if char == "\n" else " " for char in chars[start:end]]


def _plain_string_end(source: str, at: int) -> int:
    """The offset just past the string literal whose opening quote is at `at`."""
    found = _PLAIN_STRING.match(source, at)
    return len(source) if found is None else found.end()


def _raw_string_end(source: str, at: int, hashes: int) -> int:
    closing = '"' + "#" * hashes
    found = source.find(closing, at)
    return len(source) if found < 0 else found + len(closing)


def _comment_end(source: str, at: int) -> int:
    """The offset just past the block comment opening at `at`, nesting included."""
    depth = 0
    cursor = at
    while (found := _BLOCK_DELIMITERS.search(source, cursor)) is not None:
        depth += 1 if found.group() == "/*" else -1
        cursor = found.end()
        if depth == 0:
            return cursor
    return len(source)


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
    while (found := _MASK_TOKENS.search(source, index)) is not None:
        at = found.start()
        construct = found.group()
        if construct == "//":
            end = source.find("\n", at)
            end = length if end < 0 else end
        elif construct == "/*":
            end = _comment_end(source, at)
        elif construct.endswith('"'):
            quote = at + construct.index('"')
            end = (
                _raw_string_end(source, quote + 1, construct.count("#"))
                if construct.startswith(("r", "br", "rb"))
                else _plain_string_end(source, quote)
            )
        else:
            literal_end = _char_literal_end(source, at)
            if literal_end is None:
                index = at + 1
                continue
            end = literal_end
        _blank(chars, at, end)
        index = max(end, at + 1)
    return "".join(chars)


def _brace_end(masked: str, at: int) -> int:
    """The offset just past the brace that closes the one at `at`."""
    depth = 0
    cursor = at
    while (found := _BRACES.search(masked, cursor)) is not None:
        cursor = found.start()
        depth += 1 if masked[cursor] == "{" else -1
        cursor += 1
        if depth == 0:
            return cursor
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


def _cfg_predicate(attribute: str) -> str | None:
    """The text inside a `cfg(...)` attribute's parentheses, or None when it is not a cfg."""
    found = _CFG_ATTRIBUTE.search(attribute)
    if found is None:
        return None
    depth = 1
    cursor = found.end()
    while cursor < len(attribute):
        char = attribute[cursor]
        if char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return attribute[found.end() : cursor]
        cursor += 1
    return None


def _predicate_arguments(inner: str) -> list[str]:
    """The top-level arguments of a cfg combinator's argument list."""
    arguments: list[str] = []
    depth = 0
    start = 0
    for index, char in enumerate(inner):
        if char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
        elif char == "," and depth == 0:
            arguments.append(inner[start:index])
            start = index + 1
    arguments.append(inner[start:])
    return [argument.strip() for argument in arguments if argument.strip()]


def _test_only_predicate(predicate: str) -> bool:
    """Whether a cfg predicate holds ONLY in test builds.

    `test` does; `all(..., test)` does (every conjunct must hold); `any(..., test)`
    does only when every alternative is itself test-only, which is what keeps
    `cfg(any(test, feature = "writer"))` a shipped module; `not(...)` never does.
    """
    head, _, tail = predicate.strip().partition("(")
    name = head.strip()
    if not name:
        return False
    if not tail:
        return name == "test"
    inner = tail.rstrip()
    arguments = _predicate_arguments(inner.removesuffix(")"))
    if name == "all":
        return any(_test_only_predicate(argument) for argument in arguments)
    if name == "any":
        return bool(arguments) and all(_test_only_predicate(argument) for argument in arguments)
    return False


def _is_test_predicate(attribute: str) -> bool:
    """Whether a `cfg` attribute gates its item to test builds only."""
    predicate = _cfg_predicate(attribute)
    return predicate is not None and _test_only_predicate(predicate)


def _attribute(masked: str, at: int) -> Attribute | None:
    """The attribute whose `#` is at `at`, or None when that `#` opens something else."""
    cursor = at + 1
    while cursor < len(masked) and masked[cursor] in " \t":
        cursor += 1
    inner = masked[cursor : cursor + 1] == "!"
    cursor += 1 if inner else 0
    if masked[cursor : cursor + 1] != "[":
        return None
    depth = 0
    while cursor < len(masked):
        char = masked[cursor]
        if char == "[":
            depth += 1
        elif char == "]":
            depth -= 1
            if depth == 0:
                return Attribute(
                    start=at,
                    end=cursor + 1,
                    inner=inner,
                    test=_is_test_predicate(masked[at : cursor + 1]),
                )
        cursor += 1
    return None


def _item_extent(masked: str, at: int) -> tuple[int, int]:
    """The extent of the item beginning at `at`: its brace block, or its `;`-ended head.

    A `}`, `;` or `,` at depth zero means the item is a field, an arm or an
    expression inside a block: the extent stops there, so a `#[cfg(test)]` struct
    field cannot swallow the spawn sites that follow it in the same block.
    """
    depth = 0
    cursor = at
    while (found := _ITEM_BREAKS.search(masked, cursor)) is not None:
        cursor = found.start()
        char = masked[cursor]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth = max(0, depth - 1)
        elif depth == 0 and char == "{":
            return (at, _brace_end(masked, cursor))
        elif depth == 0 and char in ";,":
            return (at, cursor + 1)
        elif depth == 0 and char == "}":
            return (at, cursor)
        cursor += 1
    return (at, len(masked))


def _enclosing_block(masked: str, at: int) -> tuple[int, int]:
    """The brace block enclosing `at` -- the whole source when no block does."""
    stack: list[int] = []
    cursor = 0
    while (found := _BRACES.search(masked, cursor)) is not None:
        cursor = found.start()
        if cursor >= at:
            break
        if masked[cursor] == "{":
            stack.append(cursor)
        elif stack:
            stack.pop()
        cursor += 1
    if not stack:
        return (0, len(masked))
    return (stack[-1], _brace_end(masked, stack[-1]))


def _test_regions(masked: str) -> tuple[tuple[int, int], ...]:
    """The spans a `test` cfg predicate gates: an item, a module, or an inner region."""
    attributes: list[Attribute] = []
    cursor = 0
    while (at := masked.find("#", cursor)) >= 0:
        cursor = at + 1
        found = _attribute(masked, at)
        if found is None:
            continue
        attributes.append(found)
        cursor = found.end
    regions: list[tuple[int, int]] = []

    def flush(run: list[Attribute]) -> None:
        if not run or not any(attribute.test for attribute in run):
            return
        last = run[-1]
        regions.append(
            _enclosing_block(masked, last.start) if last.inner else _item_extent(masked, last.end)
        )

    run: list[Attribute] = []
    for attribute in attributes:
        if run and masked[run[-1].end : attribute.start].strip():
            flush(run)
            run = []
        run.append(attribute)
    flush(run)
    return tuple(regions)


def _within(position: int, spans: tuple[tuple[int, int], ...]) -> bool:
    return any(start <= position < end for start, end in spans)


def _module_files(relative: str, name: str) -> tuple[str, str]:
    """The two files `mod <name>;` in `relative` can name, both crate-relative."""
    parts = relative.split("/")
    stem = parts[-1]
    directory = parts[:-1] if stem in _MODULE_FILE_STEMS else [*parts[:-1], stem[:-3]]
    base = "/".join(directory)
    return (f"{base}/{name}.rs", f"{base}/{name}/mod.rs")


class Sources:
    """The tree's crate sources, each masked and read for its test regions once."""

    def __init__(self, texts: dict[str, str], root: Path) -> None:
        self._texts = texts
        self._root = root
        self._masks: dict[str, str] = {}
        self._regions: dict[str, tuple[tuple[int, int], ...]] = {}

    def __contains__(self, relative: str) -> bool:
        return relative in self._texts

    def text(self, relative: str) -> str:
        return self._texts[relative]

    def masked(self, relative: str) -> str:
        mask = self._masks.get(relative)
        if mask is None:
            mask = _mask(self._texts[relative])
            self._masks[relative] = mask
        return mask

    def regions(self, relative: str) -> tuple[tuple[int, int], ...]:
        """The spans of `relative` that only a test build compiles."""
        regions = self._regions.get(relative)
        if regions is None:
            regions = _test_regions(self.masked(relative))
            self._regions[relative] = regions
        return regions

    def test_target(self, relative: str) -> bool:
        """Whether `relative` sits in a cargo test or bench target directory."""
        parts = relative.split("/")
        return any(
            part in _TEST_TARGET_DIRS and self._crate(parts[:index])
            for index, part in enumerate(parts[:-1])
        )

    def test_only(self) -> frozenset[str]:
        """The test targets, and every module a test region declares, transitively."""
        excluded = {relative for relative in self._texts if self.test_target(relative)}
        for relative in self._texts:
            text = self._texts[relative]
            if relative in excluded or _RAW_MOD_DECL.search(text) is None:
                continue
            for start, end in self.regions(relative):
                excluded.update(self._declared(relative, self.masked(relative)[start:end]))
        frontier = list(excluded)
        while frontier:
            relative = frontier.pop()
            for candidate in self._declared(relative, self.masked(relative)):
                if candidate not in excluded:
                    excluded.add(candidate)
                    frontier.append(candidate)
        return frozenset(excluded)

    def _crate(self, directory: list[str]) -> bool:
        return (self._root / Path(*directory) / "Cargo.toml").is_file()

    def _declared(self, relative: str, region: str) -> set[str]:
        """The existing sources the `mod x;` declarations of `region` name."""
        return {
            candidate
            for name in _MOD_DECL.findall(region)
            for candidate in _module_files(relative, name)
            if candidate in self._texts
        }


def _sites_in(relative: str, sources: Sources) -> tuple[list[Site], int]:
    """The unsanctioned production sites of one source, and how many register in place."""
    text = sources.text(relative)
    masked = sources.masked(relative)
    regions = sources.regions(relative)
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
            if _within(position, regions):
                continue
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
    """One tree walk: the unsanctioned production sites, the registered ones, the sources read."""

    sites: tuple[Site, ...]
    registered: int
    examined: int
    excluded: int


def _scan(root: Path) -> Scan:
    scan_root = root / _SCAN_ROOT
    if not scan_root.is_dir():
        raise GateError(f"nothing to scan: {scan_root} is not a directory")
    texts: dict[str, str] = {}
    for path in sorted(scan_root.rglob("*.rs")):
        if _SKIP_DIRS.intersection(path.parts):
            continue
        try:
            texts[path.relative_to(root).as_posix()] = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as err:
            raise GateError(f"cannot read {path}: {err}") from err
    sources = Sources(texts, root)
    excluded = sources.test_only()
    sites: list[Site] = []
    registered = 0
    for relative, text in texts.items():
        if relative in excluded:
            continue
        if not any(token in text for token, _ in _SPAWN_TOKENS):
            continue
        found, in_place = _sites_in(relative, sources)
        sites.extend(found)
        registered += in_place
    return Scan(
        sites=tuple(sites),
        registered=registered,
        examined=len(texts),
        excluded=len(excluded),
    )


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
        f"worker-census-spawn: {len(scan.sites) + scan.registered} production spawn sites over "
        f"{scan.examined} sources ({scan.excluded} test-only sources out of scope): "
        f"{scan.registered} register in their own function, "
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
