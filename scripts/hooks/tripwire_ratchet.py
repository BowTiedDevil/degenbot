#!/usr/bin/env python3
"""The retirement-tripwire ratchet: a source scan for tripwire signatures.

# live-invariant gate (ADR-087 D2): watches the present test/gate surface (rust
# test files, tests/, justfile, prek.toml, scripts/, docs/) for the tripwire
# signatures ADR-087 D1 bans; not tripwire precedent.

Deleting a module is the complete retirement (ADR-087 D1). A test, gate, or prose
prohibition whose purpose is keeping a deliberately deleted thing gone is a
retirement tripwire and does not exist here. This scanner is the ban's mechanism
(D4): it reads the tree as it stands and fails on any artifact whose shape is a
resurrection guard rather than a test of present code.

It flags; it does not classify. A hit means the author must do one of exactly two
things, and both make the decision visible in the diff:

  delete the artifact   -- it is a tripwire; deletion is the retirement.
  label the artifact    -- it is a live-invariant gate; add the D2 marker below
                           naming the present code it watches.

The exemption marker is fixed (ADR-087 D2, as amended). A file carrying the exact
prefix

    live-invariant gate (ADR-087 D2): watches

is excused, because the marker's own requirement -- naming the present declaration,
manifest, or call site the gate guards -- is what separates the two species. The
exemption is file-granular on purpose: the marker is a statement about the artifact,
and a file carrying it has already answered the question this gate asks. There is no
allowlist, baseline, or waiver file, and there must never be one: a suppression list
would be a second, unreviewable gate list (ADR-074's argument) and would become prior
art for the next guard.

Scope is ADR-087 D4's test/gate surface. A signature outside it belongs to some other
rule (the comment-hygiene gate owns tracker citations, ruff and clippy own style), so
this scan stays narrow enough to keep its verdicts mechanical.

Exit codes:
  0   the surface is clean
  1   at least one unlabeled tripwire signature was found (each is printed)
  2   the scan produced no surface -- fails closed, so a broken enumeration can never
      report a green that examined nothing
"""

from __future__ import annotations

import argparse
import re
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

#: The D2 exemption marker (ADR-087 D2, canonical-marker amendment). The fixed part
#: ends at the colon after `watches`; the clause after it is the author's statement
#: of the present code the gate guards.
MARKER = "live-invariant gate (ADR-087 D2): watches"

# A quoted span is quoted because the sentence is *about* the phrase, not an instance
# of it: the ADR that defines a prohibition spells the prohibition once, in quotes.
# Backticks and double quotes are unambiguous delimiters. A single-quoted span is not
# -- `don't` and `it's` are apostrophes -- so it must sit against non-word characters
# at both ends. Spans may run across a line wrap, so `.` is used with DOTALL rather
# than a negated newline.
_BACKTICK = re.compile(r"`[^`]{0,400}?`", re.DOTALL)
_DQUOTE = re.compile(r'"[^"]{0,400}?"', re.DOTALL)
_SQUOTE = re.compile(r"(?<![A-Za-z0-9])'[^']{0,400}?'(?![A-Za-z0-9])", re.DOTALL)


def _blank(span: str) -> str:
    """Replace a quoted span with spaces, KEEPING its newlines.

    Offsets are what the line-number arithmetic reads, and so are the newlines: a
    span that swallowed them would shift every later finding onto the wrong line (a
    triple-quoted docstring at the top of a test file would misreport the whole file).
    """
    return "".join("\n" if char == "\n" else " " for char in span)


def _mask(text: str) -> str:
    """Blank out quoted spans, preserving both offsets and line structure."""
    for pattern in (_BACKTICK, _DQUOTE, _SQUOTE):
        text = pattern.sub(lambda m: _blank(m.group(0)), text)
    return text


# --- the assertion signatures ----------------------------------------------
# Each rule is (label, pattern, file suffixes). A rule fires on the SHAPE of an
# assertion that answers "is this dead name still gone?" rather than "does this
# present code behave?". `tests/test_tripwire_ratchet.py` drives every rule against
# a synthetic red source and its green sibling, so a rule that stops firing is
# caught by the gate's own negative probe rather than discovered later.
#: Rules that read the RAW text rather than the quote-masked view. A Rust assert on
#: source text legitimately carries its needle as a string literal, so masking would
#: erase the very evidence the rule matches on.
_RAW_TEXT_RULES = frozenset({"retired-name grep (rust source text)"})

_SIG: list[tuple[str, re.Pattern[str], frozenset[str]]] = [
    # A name's absence from an object: `hasattr`, `any(... hasattr|vars)`, `dir()`,
    # `vars()`, `__all__`. The census's absent-api archetype.
    (
        "absent-api assertion",
        re.compile(r"\bassert\w*\s*\(?\s*not\s+hasattr\s*\("),
        frozenset({".py"}),
    ),
    (
        "absent-api assertion",
        re.compile(r"\bassert\w*\s+not\s+any\s*\([^)\n]*\b(?:hasattr|vars)\s*\("),
        frozenset({".py"}),
    ),
    (
        "absent-api assertion",
        re.compile(r"\bassert\w*\s+[^\n]*\bnot\s+in\s+(?:dir|vars)\s*\("),
        frozenset({".py"}),
    ),
    (
        "absent-api assertion",
        re.compile(r"\bassert\w*\s+[^\n]*\bnot\s+in\s+[\w.]+__all__\b"),
        frozenset({".py"}),
    ),
    # A test that reads a declaration list or a file's text and asserts an
    # identifier is ABSENT from it -- the census's name-text-grep archetype, and its
    # Rust `include_str!` spelling. Naming a source variable or a `field_names` /
    # `param_names` collection is the tell: the assertion is about a name's presence
    # in text, not about the behavior of a call.
    (
        "retired-name grep",
        re.compile(
            r"\bassert\w*\s+[^\n]*\bnot\s+in\s+"
            r"(?:src|source|src_text|source_text|body|contents|file_text|haystack"
            r"|blob|field_names|param_names)\b"
        ),
        frozenset({".py"}),
    ),
    (
        "retired-name grep",
        re.compile(r"\bassert\w*\s+not\s+re\s*\.\s*search\s*\("),
        frozenset({".py"}),
    ),
    (
        "retired-name grep (rust source text)",
        re.compile(
            # `include_str!`'s Rust spelling of a retired-name grep. The receiver is a
            # source-text constant (`SRC`, `SUBMIT_RS`, `MOD`, `HELP_TEXT`, ...) and the
            # needle must OPEN the `contains` argument as a string literal -- the house
            # shape is `assert!(\n    !SRC.contains(&["pub async fn ", retired].concat())`.
            #
            # The literal condition is what separates this from a live SINGLE-HOME rule
            # of the same shape: `assert!(!MOD.contains(&needle))` over a table of
            # present function names (`phase_seams_are_single_free_functions_in_their_
            # owning_module`) asks where LIVE code lives and flags nothing dead. A needle
            # the caller supplies by name is that live rule; grepping the text for a
            # missing literal or a retirement-worded binding is the tripwire.
            #
            # The gap crosses newlines because the assert wraps.
            r"\bassert!\s*\([\s\S]{0,80}?!\s*[A-Za-z0-9_]*"
            r"(?:SRC|SOURCE|TEXT|FILE|BODY|CODE|RS|MOD|PATH)[A-Za-z0-9_]*"
            r'\s*\.\s*contains\s*\(\s*&?\s*\[?\s*"'
        ),
        frozenset({".rs"}),
    ),
    # A collection NAMED for the revival (regrew / reappeared / reintroduced /
    # resurrected), asserted empty -- the census's regrowth archetype. The name may
    # BE the revival word or merely carry it as a suffix (`not regrown`,
    # `regrown_mirrors == []`), so the leading stem is optional.
    (
        "regrowth assertion",
        re.compile(
            r"\bassert\w*\s+(?:not\s+)?[a-z0-9_]*"
            r"(?:regrew|regrown|reintroduc\w*|reappear\w*|resurrect\w*|revived)\b"
        ),
        frozenset({".py", ".rs"}),
    ),
]

# --- the prose signature ---------------------------------------------------
# A revival PROHIBITION ("must not reappear", "do not revive / re-land / rebuild",
# "do not re-introduce"). The vocabulary alone is not enough: ADR-087's own
# considered-options section, the ADRs that record a retirement, and design notes that
# reject an approach all use the same words to describe or argue rather than to ban.
# Two further conditions make the test mechanical:
#
#   1. the prohibition and a RETIREMENT word (retired / deleted / removed / dissolved
#      / superseded / reverted / gone / obsolete / deprecated) must share a PARAGRAPH.
#      A prohibition about a design ("the proof must not rebuild the matrix") has no
#      retirement in scope and is a design note, not a guard on a deleted name.
#   2. the prohibition must not be quoted. Quoting is how a document names the
#      prohibition it is defining or repealing; ADR-087 and the GLOSSARY both do it,
#      and both are the sanctioned homes.
_FAILS_TEXT = re.compile(
    r"\b(?:do(?:n't)?|does(?:n't)?|did(?:n't)?|must(?:n't)?|should(?:n't)?|may|can)\s+not"
    r"\s+(?:ever\s+)?"
    r"(?:revive|resurrect|reappear|come\s+back|regrow|re-?land|rebuild|re-?introduce|re-?add)\b"
    r"|\b(?:must|should|will)\s+never\s+"
    r"(?:reappear|come\s+back|revive|resurrect|re-?land|rebuild|regrow|re-?introduce)\b"
    r"|\bnever\s+(?:reappear|come\s+back)\b",
    re.IGNORECASE,
)
_RETIREMENT = re.compile(
    r"\b(?:retir\w*|delet\w*|remov\w*|dissolv\w*|supersed\w*|revert\w*|gone|obsolete"
    r"|deprecat\w*)\b",
    re.IGNORECASE,
)
# ADR-087 D4 scopes the prose signature to docs. A code comment or doc-comment that
# says "do not re-add X" is a design note about the live shape it sits beside (the
# `seat_host/pump.rs` cordon-gate note is the standing example: it forbids a second
# hand-written gate in favour of the live class-derived consult), not the surviving
# prose half of a deleted guard. Those belong to review, per D1's compiler-blind rule.
_PROSE_SUFFIXES = frozenset({".md"})
_PARAGRAPH = re.compile(r"(?:[^\n]*\n?)+")


def _revival_prohibitions(masked: str) -> list[int]:
    """Offsets of revival prohibitions sitting in a paragraph with a retirement."""
    offsets = []
    position = 0
    for block in re.split(r"(\n[ \t]*\n)", masked):
        if _FAILS_TEXT.search(block) and _RETIREMENT.search(block):
            for match in _FAILS_TEXT.finditer(block):
                offsets.append(position + match.start())
        position += len(block)
    return offsets


def _suffix(path: str) -> str:
    return "" if path == "justfile" else Path(path).suffix


def is_rust_test_file(rel: str, text: str) -> bool:
    """Is this Rust source a TEST surface?

    Three independent tells, any of which is sufficient: a test-target directory, a
    test-named file, or an in-file `#[cfg(test)]` / `#[test]`. The third matters --
    the census found tripwires inside production modules' inline test modules (the
    `degenbot-workers/src/budget.rs` scan among them), so a `src` file joins the
    surface the moment it carries a test module.
    """
    if not rel.startswith("rust/") or not rel.endswith(".rs"):
        return False
    if "/tests/" in rel or rel.endswith(("_tests.rs", "_test.rs", "/tests.rs")):
        return True
    return "#[cfg(test)]" in text or "#[test]" in text


def in_surface(rel: str, text: str) -> bool:
    """ADR-087 D4's surface: rust test files, `tests/`, `justfile`, `prek.toml`,
    `scripts/`, `docs/`."""
    if rel in ("justfile", "prek.toml"):
        return True
    if rel.startswith(("tests/", "scripts/", "docs/")):
        return True
    return is_rust_test_file(rel, text)


def enumerate_files(root: Path) -> list[str]:
    """Repo-relative paths in the working tree, tracked AND untracked.

    `--others` matters: an index-bound enumeration certifies a tree it never looked
    at, so a brand-new untracked test file must be examined the moment it is written.
    """
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        ["git", "-C", str(root), "ls-files", "--cached", "--others", "--exclude-standard"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(
            f"tripwire-ratchet: git ls-files failed -- refusing to pass\n{result.stderr}",
            file=sys.stderr,
        )
        sys.exit(2)
    return [line for line in result.stdout.splitlines() if line]


def _line_at(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def scan(root: Path, paths: list[str]) -> tuple[list[str], int, int]:
    """Return (findings, files_in_surface, files_scanned_for_hits).

    The two counts are what makes the fail-closed check mean something: a tree whose
    entire surface is labeled still HAS a surface, so it is a green -- while a tree
    with no surface at all (a broken enumeration, a scan pointed at the wrong path) is
    not. Collapsing them would let a mis-pointed `--root` exit 2 with a message about
    labels, which is a diagnosis that sends the reader the wrong way.
    """
    findings: list[str] = []
    in_surface_count = 0
    scanned = 0
    for rel in paths:
        absolute = root / rel
        if not absolute.is_file():
            continue
        try:
            text = absolute.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        if not in_surface(rel, text):
            continue
        in_surface_count += 1
        if MARKER in text:
            continue  # labeled live-invariant gate: the author named what it watches
        scanned += 1
        suffix = _suffix(rel)
        masked = _mask(text)
        lines = text.splitlines()
        for raw_label, pattern, suffixes in _SIG:
            if suffix not in suffixes:
                continue
            label = raw_label.removesuffix(" (rust source text)")
            haystack = text if raw_label in _RAW_TEXT_RULES else masked
            for match in pattern.finditer(haystack):
                lineno = _line_at(haystack, match.start())
                source = lines[lineno - 1].strip() if lineno - 1 < len(lines) else ""
                findings.append(f"{rel}:{lineno}: {label}: {source}")
        if suffix in _PROSE_SUFFIXES:
            for offset in _revival_prohibitions(masked):
                lineno = _line_at(masked, offset)
                source = lines[lineno - 1].strip() if lineno - 1 < len(lines) else ""
                findings.append(f"{rel}:{lineno}: revival prohibition: {source}")
    return findings, in_surface_count, scanned


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Fail on unlabeled retirement tripwires (ADR-087 D4).",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=REPO_ROOT,
        help=(
            "tree to scan (default: this checkout). The negative probe uses it to run "
            "the gate against a throwaway tree holding one synthetic tripwire."
        ),
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()
    paths = enumerate_files(root)
    findings, in_surface_count, scanned = scan(root, paths)
    if in_surface_count == 0:
        print(
            "tripwire-ratchet: enumerated 0 files in the D4 surface -- refusing to "
            "certify a scan that examined nothing",
            file=sys.stderr,
        )
        return 2
    if not findings:
        labeled = in_surface_count - scanned
        print(
            f"tripwire-ratchet: clean ({in_surface_count} files in the D4 surface, "
            f"{scanned} scanned, {labeled} labeled live-invariant; "
            f"{len(paths)} paths enumerated)"
        )
        return 0
    print(
        "tripwire-ratchet: retirement tripwire(s) found (ADR-087 D1).\n"
        "  Deleting code is the complete retirement: a test, gate, or prose ban whose\n"
        "  purpose is keeping a deleted thing gone does not exist here.\n"
        "\n"
        "  Delete the artifact -- or, if it guards PRESENT code, label it:\n"
        f"      {MARKER} <the live declaration/manifest/call site it guards>;\n"
        "      not tripwire precedent.\n",
        file=sys.stderr,
    )
    for finding in findings:
        print(f"  {finding}", file=sys.stderr)
    print(
        f"\ntripwire-ratchet: {len(findings)} hit(s) in {scanned} scanned files",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
