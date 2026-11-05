#!/usr/bin/env python3
"""The `clippy::too_many_lines` waiver ratchet.

# live-invariant gate (ADR-087 D2): watches the live `clippy::too_many_lines` waiver
# sites in the Rust tree against scripts/hooks/too-many-lines-waiver-census.txt; not
# tripwire precedent.

A long function is a readability symptom, and the waiver that keeps it long is a
deliberate decision -- worth exactly as much as the reason recorded beside it. An
unexplained waiver is, in a later review, indistinguishable from a function nobody got
round to splitting. This gate makes the population of waivers a ratchet: it can only
shrink, every site is reconciled against the census in both directions, and every
reason a site states is mirrored into the census so a stated reason cannot rot.

Index: docs/gates.md section 6 "Lint-waiver census" (the sibling ratchets live there too).

How a site is keyed
-------------------
A site is a lint attribute whose list names `clippy::too_many_lines`, spelled either
as an outer attribute (`#[expect(...)]`, on the item that follows it) or as an inner
attribute (`#![expect(...)]`, a module-level waiver over the whole file). The key is

    <repo-relative-path>::<scope>

where the scope is the annotated item's name -- its declaration keyword plus the
identifier (`fn`, `mod`, `struct`, `impl`, ...) -- or `!module` for an inner attribute.
Line numbers are deliberately absent: a waiver row has to survive every reflow above
it, and it disappears only when the waiver does. Two same-named items in one file (the
nested `walk` helpers are the standing case) are disambiguated by an occurrence suffix:
`...::walk`, `...::walk#2`.

The rules
---------
1. A waiver site with no census row fails. A new waiver has to state its reason in the
   source (`reason = "..."`, or a trailing `//` comment on the attribute) and mirror it
   into the census; the shorter path is to strip the waiver and split the function.
2. A census row with no waiver site fails: a swept site deletes its row in the same
   change.
3. A row's reason must equal the reason its site states, in both directions -- a stated
   reason with no row entry fails (the row must record it), and a row entry the source
   no longer states fails (the census cannot carry prose the code dropped).
4. A missing census, an unreadable header, or a scan that found no Rust files at all
   fails closed: this gate never certifies a tree it did not examine.

The header number `reasonless-high-water: N` is the one piece of the census that is not
a row. It freezes the count of waiver sites that state no reason: that count moves only
downward, so adding a reasonless waiver fails even when its row is added. Every shrink
lowers the number in the same change.

Exit codes:
  0   every site and row reconcile
  1   at least one finding (fresh site, stale row, reason mismatch, ceiling breached)
  2   refused to certify (missing census, unusable header, empty scan)

Regenerate the rows after a sweep (the header, including the ceiling, is by hand):

    just check-too-many-lines-waivers --rows

Scope. Every `*.rs` file in the working tree, tracked or not (an index-bound
enumeration certifies a tree it never looked at). That covers `rust/crates/**`, the
workspace examples and tests, and `rust/examples/**`. Two shapes are out of scope by
construction: a waiver written as `#[allow(...)]` (the workspace denies `allow` at the
lint root, so the expectation form is the only live spelling), the same lint waived
inside a macro expansion, which no source scan can see, and an attribute block that
neither balances its brackets on one line nor closes on a bracket of its own: that block
is read as a runaway, so its site surfaces with a `?` scope and fails loudly instead of
passing unrowed.
"""

from __future__ import annotations

import argparse
import re
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
CENSUS_REL = "scripts/hooks/too-many-lines-waiver-census.txt"
LINT = "clippy::too_many_lines"
CEILING_LABEL = "reasonless-high-water"
SKIP_DIRS = frozenset({".git", ".hg", ".svn", "target", "node_modules", ".venv", "venv", "_build"})

_ATTR_START = re.compile(r"^[ \t]*#(!?)\[")
_ATTR_END = re.compile(r"^[ \t]*\)?\]")
_REASON_KV = re.compile(r'reason\s*=\s*"((?:[^"\\]|\\.)*)"', re.DOTALL)
_DECL = re.compile(
    r"^[ \t]*(?:pub[ \t]*(?:\([^)]*\))?[ \t]+)?"
    r"(?:(?:const|async|unsafe|default|extern(?:[ \t]+\"[^\"]*\")?)[ \t]+)*"
    r"(fn|mod|struct|enum|trait|union|type|const|static|impl|macro_rules[ \t]*!)[ \t]+(.+)$"
)
_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_CEILING = re.compile(r"^#[ \t]*" + CEILING_LABEL + r":[ \t]*([0-9]+)[ \t]*$")

# An attribute block is bounded by its brackets. `#[expect(` opens a block that ends
# at the first `)]`; the single-line form closes on its own line. Bracket counting
# runs over a string-masked view, so a bracket inside a quoted reason cannot close the
# block early.


def _mask_strings(text: str) -> str:
    """Replace quoted spans with spaces, keeping length and line structure."""
    out: list[str] = []
    index = 0
    length = len(text)
    while index < length:
        char = text[index]
        if char == '"':
            out.append(" ")
            index += 1
            while index < length:
                if text[index] == "\\":
                    out.append(" ")
                    index += 2
                    continue
                if text[index] == '"':
                    out.append(" ")
                    index += 1
                    break
                out.append(" ")
                index += 1
            continue
        if char == "'":
            literal = re.match(r"'(?:\\.|[^'\\])'", text[index:])
            if literal:
                out.append(" " * len(literal.group(0)))
                index += len(literal.group(0))
                continue
        out.append(char)
        index += 1
    return "".join(out)


_ESCAPES = {"n": " ", "t": " ", "r": " ", '"': '"', "'": "'", "\\": "\\"}


def _squash(raw: str) -> str:
    r"""Collapse a reason onto one line.

    Rust's ``\``-at-end-of-line continuation joins first; the remaining escapes become
    the characters they stand for (``\n`` -> a space, so a two-sentence reason stays
    one census row).
    """
    joined = raw.replace("\\\n", " ")
    unescaped = re.sub(
        r"\\(.)",
        lambda match: _ESCAPES.get(match.group(1), match.group(0)),
        joined,
        flags=re.DOTALL,
    )
    return " ".join(unescaped.split())


def _block_end(lines: list[str], start: int) -> int:
    """Index of the last line of the attribute block opening at `start`."""
    if _mask_strings(lines[start]).count("[") - _mask_strings(lines[start]).count("]") <= 0:
        return start
    index = start + 1
    while index < len(lines):
        if _ATTR_END.match(_mask_strings(lines[index])):
            return index
        index += 1
    return len(lines) - 1


def _reason(block: list[str]) -> str:
    """The reason a site states: `reason = "..."` first, else a trailing comment."""
    joined = "\n".join(block)
    keyed = _REASON_KV.search(joined)
    if keyed:
        return _squash(keyed.group(1))
    closing = _mask_strings(block[-1])
    marker = closing.find("//")
    if marker >= 0:
        return _squash(block[-1][marker + 2 :])
    return ""


def _decl_name(kind: str, rest: str) -> str:
    """The name of the item an attribute annotates."""
    if kind.startswith("macro_rules"):
        named = _NAME.search(rest)
        return f"macro_rules! {named.group(0)}" if named else "macro_rules!"
    if kind == "impl":
        # `impl<T: Into<U>> Foo<T> for Bar where ...` -> the last named type.
        text = re.sub(r"^<[^<>]*>[ \t]*", "", rest)
        text = re.sub(r"^<.*?>[ \t]*", "", text)
        text = text.split(" where ")[0].split("{")[0]
        segments = [segment.strip() for segment in text.split(" for ")]
        tail = segments[-1].split("::")[-1]
        named = _NAME.match(tail.strip())
        return f"impl {named.group(0)}" if named else "impl"
    named = _NAME.match(rest.strip())
    return named.group(0) if named else kind


def _annotated_scope(lines: list[str], index: int) -> str:
    """The name of the item declared at or after `index`, skipping attributes."""
    cursor = index
    while cursor < len(lines):
        line = lines[cursor]
        stripped = line.strip()
        if not stripped or stripped.startswith("//"):
            cursor += 1
            continue
        if _ATTR_START.match(line):
            cursor = _block_end(lines, cursor) + 1
            continue
        decl = _DECL.match(line)
        if decl:
            return _decl_name(decl.group(1), decl.group(2))
        return "?" + " ".join(stripped.split())
    return "?"


@dataclass(frozen=True)
class Site:
    path: str
    lineno: int
    scope: str
    reason: str

    @property
    def key(self) -> str:
        return f"{self.path}::{self.scope}"


def _sites_in(path: str, text: str) -> list[Site]:
    """Every `too_many_lines` waiver site in one Rust source."""
    lines = text.splitlines()
    sites: list[Site] = []
    index = 0
    while index < len(lines):
        opened = _ATTR_START.match(lines[index])
        if not opened:
            index += 1
            continue
        start = index
        end = _block_end(lines, start)
        block = lines[start : end + 1]
        index = end + 1
        if LINT not in "\n".join(block):
            continue
        scope = "!module" if opened.group(1) == "!" else _annotated_scope(lines, index)
        sites.append(Site(path, start + 1, scope, _reason(block)))
    return sites


def _disambiguate(sites: list[Site]) -> list[Site]:
    """Suffix repeated scopes in one file so every row names exactly one site."""
    seen: dict[str, int] = {}
    out: list[Site] = []
    for site in sites:
        count = seen.get(site.key, 0) + 1
        seen[site.key] = count
        out.append(
            site
            if count == 1
            else Site(site.path, site.lineno, f"{site.scope}#{count}", site.reason)
        )
    return out


def enumerate_sources(root: Path) -> list[Path]:
    """Every `*.rs` file in the tree, tracked or untracked, in a stable order.

    `git ls-files --cached --others --exclude-standard` is the view: it sees an
    untracked file the moment it is written (an index-bound enumeration has certified
    trees it never looked at) and it leaves ignored build output -- the sphinx copies
    of these same sources under `docs/_build` -- out of the population. A root that is
    not a git checkout (the negative probe's throwaway tree) falls back to a walk that
    skips the directories build output and virtualenvs land in by name.
    """
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [
            "git",
            "-C",
            str(root),
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.rs",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode == 0:
        return sorted(root / name for name in result.stdout.split("\0") if name)
    found: list[Path] = []
    stack = [root]
    while stack:
        current = stack.pop()
        try:
            entries = list(current.iterdir())
        except OSError:
            continue
        for entry in entries:
            if entry.is_dir():
                if entry.name not in SKIP_DIRS:
                    stack.append(entry)
            elif entry.suffix == ".rs":
                found.append(entry)
    return sorted(found)


def scan(root: Path) -> list[Site]:
    """Every waiver site under `root`, keyed repo-relative."""
    sites: list[Site] = []
    for path in enumerate_sources(root):
        rel = path.relative_to(root).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        sites.extend(_disambiguate(_sites_in(rel, text)))
    return sites


@dataclass(frozen=True)
class Census:
    ceiling: int
    rows: dict[str, str]


def read_census(path: Path) -> tuple[Census | None, str]:
    """Parse the census, or return the reason it cannot be trusted."""
    if not path.is_file():
        return None, f"census missing: {path}"
    rows: dict[str, str] = {}
    ceiling: int | None = None
    duplicates: list[str] = []
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw.rstrip()
        if not line.strip():
            continue
        if line.startswith("#"):
            matched = _CEILING.match(line)
            if matched:
                ceiling = int(matched.group(1))
            continue
        key, _, reason = line.partition("\t")
        key = key.strip()
        if not key:
            continue
        if key in rows:
            duplicates.append(f"{key} (line {number})")
            continue
        rows[key] = " ".join(reason.split())
    if ceiling is None:
        return None, f"census carries no `# {CEILING_LABEL}: <n>` header: {path}"
    if duplicates:
        return None, "census rows name the same site twice: " + ", ".join(duplicates)
    return Census(ceiling, rows), ""


def _report(findings: list[str]) -> None:
    print(
        "too-many-lines-waivers: the waiver census is out of sync with the tree\n"
        "  (docs/gates.md section 6). 'fresh' is a waiver with no row: strip it and\n"
        "  split the function, or state its reason in the source and add the row.\n"
        "  'stale' is a row whose waiver is gone: delete the row. 'reason' is a\n"
        "  stated reason the census and the source disagree about.",
        file=sys.stderr,
    )
    for finding in findings:
        print(f"  {finding}", file=sys.stderr)
    print(f"\ntoo-many-lines-waivers: {len(findings)} finding(s)", file=sys.stderr)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Reconcile too_many_lines waivers with their census."
    )
    parser.add_argument(
        "--root", type=Path, default=REPO_ROOT, help="tree to scan (default: this checkout)"
    )
    parser.add_argument(
        "--rows", action="store_true", help="print the census rows the tree implies"
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()

    sites = scan(root)
    if not sites and not args.rows:
        print(
            f"too-many-lines-waivers: no waiver sites found under {root} -- refusing to "
            "certify a scan that examined nothing",
            file=sys.stderr,
        )
        return 2

    if args.rows:
        for site in sorted(sites, key=lambda site: site.key):
            print(f"{site.key}\t{site.reason}" if site.reason else site.key)
        stated = sum(1 for site in sites if site.reason)
        print(
            f"# {len(sites)} sites, {stated} with a stated reason, "
            f"{len(sites) - stated} reasonless (the ceiling)",
            file=sys.stderr,
        )
        return 0

    census, problem = read_census(root / CENSUS_REL)
    if census is None:
        print(f"too-many-lines-waivers: {problem} -- refusing to certify", file=sys.stderr)
        return 2

    by_key: dict[str, Site] = {}
    for site in sites:
        by_key.setdefault(site.key, site)

    findings: list[str] = []
    for site in sorted(sites, key=lambda site: site.key):
        if site.key not in census.rows:
            findings.append(
                f"fresh: {site.path}:{site.lineno}: {site.key} -> "
                f"census row: {site.key}{chr(9) + site.reason if site.reason else ''}"
            )
            continue
        listed = census.rows[site.key]
        if site.reason and listed != site.reason:
            findings.append(
                f'reason: {site.path}:{site.lineno}: the source states "{site.reason}" '
                f'but the row says "{listed}" -> record the stated reason'
            )
        elif listed and not site.reason:
            findings.append(
                f'reason: {site.path}:{site.lineno}: the row records "{listed}" but the '
                "source states none -> restore the reason or drop the row's text"
            )
    for key in sorted(census.rows):
        if key not in by_key:
            findings.append(f"stale: {key} -> delete this row")

    reasonless = sum(1 for site in sites if not site.reason)
    if reasonless > census.ceiling:
        findings.append(
            f"reasonless: {reasonless} waiver sites state no reason, the census ceiling "
            f"is {census.ceiling} -> a new waiver needs a stated reason to land"
        )

    if findings:
        _report(findings)
        return 1

    stated = len(sites) - reasonless
    print(
        f"too-many-lines-waivers: clean ({len(sites)} waiver sites, {stated} with a "
        f"stated reason, {reasonless} reasonless at the ceiling of {census.ceiling})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
