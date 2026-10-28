"""Every declared config key names a provable owner.

`SCHEMA` is the single operator surface, but a key can be honoured by the Rust
core, by the Python driver shell, or only by test infrastructure. The owner is
*derived from where the field is read*, not asserted: the censuses in
`degenbot-config` must equal what the sources actually say, so a key that loses
its last core reader cannot keep claiming `owner = core`.

Classification, per declared key:

- **core** — the field name is read in `rust/crates` outside `degenbot-config`,
  ignoring test paths and `#[cfg(test)]` files.
- **shell** — read in `src/` (the Python driver) instead.
- **test_only** — read only from test code (`tests/`, `*/tests/`, `*tests*.rs`).

A key read in none of those places is the failure this gate exists for: the core
declared it and nobody honours it.
"""

from __future__ import annotations

import re
from pathlib import Path

from tests.source_scan import identifiers

ROOT = Path(__file__).resolve().parents[1]
SCHEMA_RS = ROOT / "rust" / "crates" / "foundation" / "degenbot-config" / "src" / "schema.rs"

_SECTION = re.compile(r"^\s{4}(\w+)\s+(\w+)\s*\{")
_KEY = re.compile(r'^\s*(\w+)\s+\[[^\]]+\]\s*=.*?env = "([A-Z0-9_]+)"')
_TEST_PATH = re.compile(r"(/tests/|tests\.rs$|_test\.rs$|^tests/)")


def _declared_keys() -> list[tuple[str, str]]:
    """The declared `(toml_path, field)` pairs, in schema order."""
    keys: list[tuple[str, str]] = []
    section: str | None = None
    for line in SCHEMA_RS.read_text(encoding="utf-8").splitlines():
        section_match = _SECTION.match(line)
        if section_match:
            section = section_match.group(1)
            continue
        key_match = _KEY.match(line)
        if key_match and section:
            keys.append((f"{section}.{key_match.group(1)}", key_match.group(1)))
    assert keys, "no keys parsed from schema.rs — the declaration shape moved"
    return keys


def _census(name: str) -> set[str]:
    text = SCHEMA_RS.read_text(encoding="utf-8")
    start = text.index(f"pub const {name}: &[&str] = &[")
    body = text[start : text.index("];", start)]
    return set(re.findall(r'"([a-z0-9_.]+)"', body))


def _owners() -> dict[str, str]:
    """The owner each declared key's read sites prove."""
    keys = _declared_keys()
    fields = sorted({field for _, field in keys})
    hits = identifiers(fields, ["rust/crates", "src", "tests"])

    def classify(path: str, field: str) -> str:
        files = hits[field]
        core = {
            f
            for f in files
            if f.startswith("rust/crates/")
            and "degenbot-config/" not in f
            and not _TEST_PATH.search(f)
        }
        if core:
            return "core"
        if any(f.startswith("src/") and not _TEST_PATH.search(f) for f in files):
            return "shell"
        return "test_only"

    return {path: classify(path, field) for path, field in keys}


# live-invariant gate (ADR-087 D2): watches the live `SHELL_OWNED_KEYS` /
# `TEST_ONLY_KEYS` censuses in `degenbot-config/src/schema.rs` against the declared
# keys' read sites; not tripwire precedent.
def test_every_declared_key_has_a_provable_owner() -> None:
    keys = _declared_keys()
    owners = _owners()
    # `SCHEMA` already pins one declaration per path; the parser can see a facet
    # body twice (once as the facet struct, once as its flattened keys), so the
    # gate compares path SETS rather than counting lines.
    assert set(owners) == {path for path, _ in keys}

    shell = {path for path, owner in owners.items() if owner == "shell"}
    test_only = {path for path, owner in owners.items() if owner == "test_only"}

    assert shell == _census("SHELL_OWNED_KEYS"), (
        "the shell census and the source disagree: "
        f"declared-only {sorted(_census('SHELL_OWNED_KEYS') - shell)}, "
        f"source-only {sorted(shell - _census('SHELL_OWNED_KEYS'))}"
    )
    assert test_only == _census("TEST_ONLY_KEYS"), (
        "the test-only census and the source disagree: "
        f"declared-only {sorted(_census('TEST_ONLY_KEYS') - test_only)}, "
        f"source-only {sorted(test_only - _census('TEST_ONLY_KEYS'))}"
    )


def test_a_key_read_nowhere_is_reported() -> None:
    """The negative probe: an unread key must not classify as core."""
    unread = identifiers(["definitely_unread_fixture_key"], ["rust/crates", "src"])
    assert unread["definitely_unread_fixture_key"] == set()


def test_the_scan_reads_a_word_the_sources_certainly_hold() -> None:
    """The positive control: the probe above only means something if the scan sees.

    An `identifiers` scan that read no file would report every declared key as
    unread, blaming the sources for a broken scan.
    """
    found = identifiers(["SHELL_OWNED_KEYS"], ["rust/crates"])
    assert found["SHELL_OWNED_KEYS"], "the source scan found nothing at all"
