#!/usr/bin/env python3
"""Tool-catalog reconciliation gate.

``rust/tools/README.md`` is the living catalog of the workspace's
development-tool surface: one row per public example, per lab ``[[bin]]``, and
per owning-crate integration test, each naming its source path, its feature
lane, and the ``just`` recipe that runs it. This gate reconciles that catalog
against the two executable authorities -- ``cargo metadata`` and the current
``justfile`` -- in both directions:

- every Public examples row resolves to exactly one live example target
  outside the tool packages, by source path and target name, and every such
  live target is covered by exactly one row;
- every current tool package -- discovered from the metadata, not from the
  README -- has exactly one Lab section naming it (whose manifest must live
  under ``rust/tools``), its rows resolve to exactly that package's
  ``[[bin]]``s, every ``[[bin]]`` of every current tool package is covered
  exactly once (a section present or not), and a row's documented primary
  Lane equals the target's ``required-features`` (an explicitly documented
  secondary build shape after the primary lane is ignored);
- the two integration tests the explicit current-source rule selects
  (``degenbot-ingestion``'s ``headless_boot``, ``degenbot-bot``'s
  ``config_smoke_boot``) are reconciled in both directions: a live selected
  target without a row, a selected row without its live target, and an
  integration row outside the rule all fail; a target the rule once selected
  whose row and target are both gone has left the current surface and is not
  asserted (no revival tripwire);
- every ``just <recipe>`` a row references names a recipe in the current
  justfile;
- a source link must resolve to an existing file inside the repository, and
  to the target's own ``src_path``.

Structural defects and metadata failures fail loudly with a diagnostic naming
the row: a cell count that disagrees with its header, a duplicate target
identity, a declared row count that disagrees with its table, a missing or
nonlocal source link, a missing catalog section, a ``cargo metadata`` failure.

Scope: the catalog's population is not merely what the README declares. The
current tool packages are discovered from the metadata -- every package whose
manifest lives under ``rust/tools`` must have exactly one Lab section here,
and its ``[[bin]]``s are reconciled whether or not a section exists yet, so a
new tool package joins by gaining a section and cannot pass unwatched. The
two integration targets are selected by the explicit current-source rule, not
by the README's presence. There is no fixed row arithmetic and no removed-name
list: a target that leaves the manifests together with its row simply leaves
the catalog's surface. The README's human prose (purpose, inputs, write
behavior, gate semantics) is authored there and never generated from metadata,
which cannot express it.

Exit codes: 0 reconciled, 1 findings, 2 refused to certify.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

_REPO_ROOT = Path(__file__).resolve().parents[1]

_README = Path("rust") / "tools" / "README.md"
_JUSTFILE = Path("justfile")

_EXAMPLES_SECTION = "Public examples"
_LAB_SECTION = "Lab binaries"
_INTEGRATION_SECTION = "Owning-crate integration tests"
_KNOWN_SECTIONS = (_EXAMPLES_SECTION, _LAB_SECTION, _INTEGRATION_SECTION)

# The explicit current-source rule: the integration tests this catalog
# reconciles, by owning package and test-target name. Everything else --
# including a test the rule once named whose row and target are both gone --
# is outside the catalog surface here.
_CURRENT_INTEGRATION_TESTS: tuple[tuple[str, str], ...] = (
    ("degenbot-ingestion", "headless_boot"),
    ("degenbot-bot", "config_smoke_boot"),
)

_EXAMPLE_ROW_KIND = "example"
_LAB_ROW_KIND = "lab"
_INTEGRATION_ROW_KIND = "integration"
_SECTION_KINDS = {
    _EXAMPLES_SECTION: _EXAMPLE_ROW_KIND,
    _LAB_SECTION: _LAB_ROW_KIND,
    _INTEGRATION_SECTION: _INTEGRATION_ROW_KIND,
}

_EXAMPLE_TARGET_KIND = "example"
_BIN_TARGET_KIND = "bin"
_TEST_TARGET_KIND = "test"

_LAB_ROW_CELLS = 7
_LANE_CELL = 3
_DEFAULT_LANES = ("default", "`default`")

_HEADING_COUNT = re.compile(r"\((\d+) rows?\)")
_HEADING_BASE = re.compile(r"\s*\((\d+) rows?\)\s*$")
_LAB_HEADING = re.compile(r"^`([^`]+)`\s*\((\d+) rows?\)")
_LINK = re.compile(r"\[([^\]]*)\]\(([^)\s]+)\)")
_CODE_SPAN = re.compile(r"`([^`]+)`")
_JUST_REF = re.compile(r"\bjust\s+([a-zA-Z0-9][a-zA-Z0-9_-]*)")
_RECIPE_NAME = re.compile(r"^[a-zA-Z_][a-zA-Z0-9_-]*$")
# A recipe's colon is never the ``:=`` of a variable assignment: the ``(?!=)``
# keeps ``name :=`` / ``name:=`` from reading as a recipe definition even when
# a variable's value itself contains a colon.
_RECIPE_DEF = re.compile(r"^([a-zA-Z_][a-zA-Z0-9_-]*)(?:\s[^:]*)?:(?!=)")
_VARIABLE_DEF = re.compile(r"^[a-zA-Z_][a-zA-Z0-9_-]*\s*:=")
# Just's top-level directives are not recipes: ``set shell := [...]`` must not
# register a recipe named ``set``.
_JUST_DIRECTIVE = re.compile(r"^(?:set|alias|export|unexport|import|include|mod)\b")
_SEPARATOR_CELL = re.compile(r"^:?-+:?$")

if TYPE_CHECKING:
    from collections.abc import Sequence

# cargo metadata / the metadata JSON dump are dynamic payloads; no field is
# validated beyond the ones the reconciliation reads, so they stay Any-typed
# through the views built from them (the publication plan's `Json` alias).
Json = dict[str, Any]


class CatalogRefusal(Exception):
    """The gate cannot examine what it needs: fail closed instead of passing."""


@dataclass(frozen=True)
class TargetView:
    """One Cargo target the metadata reports."""

    name: str
    kinds: frozenset[str]
    src_path: Path
    required_features: frozenset[str]


@dataclass(frozen=True)
class PackageView:
    """One workspace package as the reconciliation sees it."""

    name: str
    manifest: Path
    is_tool: bool
    targets: tuple[TargetView, ...]


@dataclass(frozen=True)
class CatalogRow:
    """One parsed catalog table row."""

    kind: str
    section: str
    line: int
    name: str
    source: Path | None
    lane: str | None
    just_refs: tuple[str, ...]


@dataclass
class Catalog:
    """The parsed catalog: its rows and the row counts its headings declare."""

    examples: list[CatalogRow] = field(default_factory=list)
    labs: dict[str, list[CatalogRow]] = field(default_factory=dict)
    integration: list[CatalogRow] = field(default_factory=list)
    declared: dict[str, int] = field(default_factory=dict)

    def rows(self) -> list[CatalogRow]:
        """Collect every catalog row in document order.

        Returns:
            Every row of the three sections, examples first.

        """
        rows = list(self.examples)
        for lab_rows in self.labs.values():
            rows.extend(lab_rows)
        rows.extend(self.integration)
        return rows


@dataclass
class _ParseState:
    """The section context the README line walk carries between lines."""

    readme_dir: Path
    root: Path
    section: str | None = None
    owner: str | None = None
    lab_owner_ok: bool | None = None


def _display(path: Path, root: Path) -> str:
    """Format a path for diagnostics.

    Args:
        path: The path to display.
        root: The repository root it is displayed relative to.

    Returns:
        The repository-relative form when the path lives inside the root, the
        absolute form otherwise.

    """
    try:
        return str(path.resolve().relative_to(root))
    except ValueError:
        return str(path)


def _label(row: CatalogRow) -> str:
    """Locate a row in the README for one diagnostic line.

    Args:
        row: The catalog row to describe.

    Returns:
        A ``line N [section] 'name'`` prefix.

    """
    return f"line {row.line} [{row.section}] {row.name!r}"


def _split_cells(line: str) -> list[str] | None:
    """Split a markdown table line into its cells.

    A ``|`` inside a code span is data, not a cell separator.

    Args:
        line: One README line.

    Returns:
        The cell texts, or None when the line is not a table row.

    """
    body = line.strip()
    if not (body.startswith("|") and body.endswith("|")):
        return None
    cells: list[str] = []
    buf: list[str] = []
    in_code = False
    for char in body[1:-1]:
        if char == "`":
            in_code = not in_code
        if char == "|" and not in_code:
            cells.append("".join(buf).strip())
            buf = []
        else:
            buf.append(char)
    cells.append("".join(buf).strip())
    return cells


def _resolve_link(link: str, state: _ParseState) -> tuple[Path | None, str | None]:
    """Resolve one source link against the README's directory.

    A source link must be a repository-relative path to an existing file; a
    URL, an absolute path, a link that escapes the repository root, or a link
    to a missing file is a finding, not a silent skip.

    Args:
        link: The raw link target from the markdown.
        state: The parse state carrying the README's directory and the root.

    Returns:
        The resolved path and the finding message; the path is None when the
        link is unusable, and the message is None when it resolves.

    """
    if "://" in link:
        return None, f"source link {link!r} is not a repository-relative path"
    if link.startswith("/"):
        return None, f"source link {link!r} is absolute"
    resolved = (state.readme_dir / link).resolve()
    try:
        resolved.relative_to(state.root)
    except ValueError:
        return None, f"source link {link!r} escapes the repository root"
    if not resolved.is_file():
        shown = _display(resolved, state.root)
        return None, f"source link {link!r} resolves to {shown}, which does not exist"
    return resolved, None


def _parse_row(
    kind: str,
    section: str,
    no: int,
    cells: list[str],
    state: _ParseState,
    findings: list[str],
) -> CatalogRow | None:
    """Turn one table row into a CatalogRow.

    Records the row's own structural findings (a missing name, a missing or
    nonlocal source link); a row that failed those checks is not returned and
    the reconciliation skips it.

    Args:
        kind: The row kind: example, lab, or integration.
        section: The section that owns the row.
        no: The row's 1-based README line number.
        cells: The row's cells.
        state: The parse state carrying the README's directory and the root.
        findings: The accumulated findings to append to.

    Returns:
        The parsed row, or None when the row carries no resolvable identity.

    """
    prefix = f"line {no} [{section}]"
    name: str | None
    link: str | None
    if kind == _LAB_ROW_KIND:
        link_match = _LINK.search(cells[0])
        if link_match is None:
            findings.append(f"{prefix}: the Binary cell carries no source link")
            name, link = None, None
        else:
            name = link_match.group(1).strip("` ")
            link = link_match.group(2)
        lane = cells[_LANE_CELL] if len(cells) > _LANE_CELL else None
    else:
        name_match = _CODE_SPAN.search(cells[0])
        if name_match is None:
            findings.append(f"{prefix}: the first cell names no target in a code span")
            name, link = None, None
        else:
            name = name_match.group(1)
            link_match = _LINK.search(cells[1])
            if link_match is None:
                findings.append(f"{prefix}: the Source cell carries no link")
                link = None
            else:
                link = link_match.group(2)
        lane = None
    # ``just <recipe>`` references are read from Markdown code spans only:
    # prose that happens to say "just any word" is not a recipe reference.
    just_refs = tuple(
        ref
        for cell in cells
        for span in _CODE_SPAN.findall(cell)
        for ref in _JUST_REF.findall(span)
    )
    if name is None or link is None:
        return None
    source: Path | None = None
    if link:
        source, link_finding = _resolve_link(link, state)
        if link_finding is not None:
            findings.append(f"{prefix}: {link_finding}")
    return CatalogRow(
        kind=kind,
        section=section,
        line=no,
        name=name,
        source=source,
        lane=lane,
        just_refs=just_refs,
    )


def _take_table(
    table: list[tuple[int, str]],
    state: _ParseState,
    catalog: Catalog,
    findings: list[str],
) -> None:
    """Consume one accumulated table under a known section, or ignore it.

    A table under a section the catalog does not own is human-authored prose
    and is left alone; a malformed table under a catalog section is a finding.

    Args:
        table: The consecutive table lines with their line numbers.
        state: The parse state: the owning section, the lab owner package, and
            the paths the source links resolve against.
        catalog: The catalog under construction.
        findings: The accumulated findings to append to.

    """
    if not table or state.section is None:
        return
    header = _split_cells(table[0][1])
    separator = _split_cells(table[1][1])
    if (
        header is None
        or separator is None
        or not all(_SEPARATOR_CELL.match(cell) for cell in separator)
    ):
        findings.append(
            f"line {table[0][0]}: the table under {state.section!r} lacks a header and "
            "separator row"
        )
        return
    kind = _SECTION_KINDS[state.section]
    if state.section == _LAB_SECTION:
        if state.lab_owner_ok is None:
            findings.append(
                f"line {table[0][0]}: a table under {state.section!r} names no tool-package section"
            )
            return
        if state.lab_owner_ok is False:
            return
        assert state.owner is not None  # lab_owner_ok True implies an owner
        rows = catalog.labs.setdefault(state.owner, [])
        label = state.owner
    else:
        rows = catalog.examples if state.section == _EXAMPLES_SECTION else catalog.integration
        label = state.section
    for no, line in table[2:]:
        cells = _split_cells(line)
        if cells is None or len(cells) != len(header):
            width = len(cells) if cells is not None else 0
            findings.append(
                f"line {no}: row under {label!r} has {width} cells, the header declares "
                f"{len(header)}"
            )
            continue
        row = _parse_row(kind, label, no, cells, state, findings)
        if row is not None:
            rows.append(row)


def _heading_section(
    heading: str,
    no: int,
    state: _ParseState,
    catalog: Catalog,
    findings: list[str],
    seen_sections: set[str],
) -> None:
    """Apply one ``## `` heading to the parse state.

    A heading of a catalog section starts that section and its declared row
    count; any other heading ends catalog parsing until the next catalog
    section.

    Args:
        heading: The heading text after the ``## `` marker.
        no: The heading's 1-based README line number.
        state: The parse state to update.
        catalog: The catalog under construction.
        findings: The accumulated findings to append to.
        seen_sections: The catalog sections already seen.

    """
    state.owner = None
    state.lab_owner_ok = None
    base = _HEADING_BASE.sub("", heading).strip()
    if base not in _KNOWN_SECTIONS:
        state.section = None
        return
    if base in seen_sections:
        findings.append(f"line {no}: section {base!r} appears twice; the structure is ambiguous")
    seen_sections.add(base)
    state.section = base
    count = _HEADING_COUNT.search(heading)
    if count is None:
        findings.append(f"line {no}: section {base!r} declares no row count")
    else:
        catalog.declared[base] = int(count.group(1))


def _lab_section_heading(
    heading: str,
    no: int,
    state: _ParseState,
    catalog: Catalog,
    findings: list[str],
    seen_labs: set[str],
) -> None:
    """Apply one ``### `` heading under the Lab binaries section.

    The heading must name the tool package that owns the rows under it and the
    row count its table holds; anything else is a structural finding and the
    heading owns no tables.

    Args:
        heading: The heading text after the ``### `` marker.
        no: The heading's 1-based README line number.
        state: The parse state to update.
        catalog: The catalog under construction.
        findings: The accumulated findings to append to.
        seen_labs: The lab-section owners already seen.

    """
    match = _LAB_HEADING.match(heading)
    if match is None:
        state.owner = None
        state.lab_owner_ok = False
        findings.append(
            f"line {no}: lab section heading {heading!r} does not name a tool "
            "package and its row count"
        )
        return
    state.owner = match.group(1)
    state.lab_owner_ok = True
    if state.owner in seen_labs:
        findings.append(
            f"line {no}: lab section {state.owner!r} appears twice; every tool package "
            "must have exactly one Lab section"
        )
    seen_labs.add(state.owner)
    catalog.declared[state.owner] = int(match.group(2))


def parse_catalog(text: str, readme_dir: Path, root: Path) -> tuple[Catalog, list[str]]:
    """Parse the catalog's three known sections out of the README.

    Only the three known sections are read; the README's other sections (the
    feature-isolation tables, the boundaries prose) are human-authored and
    are not catalog rows. The line walk is deliberately small hardening, not
    a general Markdown parser: fenced code blocks are skipped wholesale (a
    fenced sketch of a table or heading is not a catalog row), and a
    ``###`` subsection outside the Lab section ends catalog parsing the same
    way (its tables are prose, not rows). Findings cover the structural
    defects; a missing known section is a refusal, because a catalog missing
    a population cannot be certified at all.

    Args:
        text: The README's text.
        readme_dir: The directory the README's relative links resolve against.
        root: The repository root a source link must stay inside.

    Returns:
        The parsed catalog and its structural findings.

    Raises:
        CatalogRefusal: A known section is missing from the README.

    """
    catalog = Catalog()
    findings: list[str] = []
    seen_sections: set[str] = set()
    seen_labs: set[str] = set()
    state = _ParseState(readme_dir=readme_dir, root=root)
    table: list[tuple[int, str]] = []

    def flush() -> None:
        nonlocal table
        _take_table(table, state, catalog, findings)
        table = []

    in_fence = False
    for no, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if in_fence:
            if stripped.startswith("```"):
                in_fence = False
            continue
        if stripped.startswith("```"):
            flush()
            in_fence = True
            continue
        if stripped.startswith("## "):
            flush()
            _heading_section(stripped[3:].strip(), no, state, catalog, findings, seen_sections)
        elif stripped.startswith("### "):
            flush()
            if state.section == _LAB_SECTION:
                _lab_section_heading(stripped[4:].strip(), no, state, catalog, findings, seen_labs)
            else:
                state.section = None
        elif stripped.startswith("|"):
            table.append((no, line))
        else:
            flush()
    flush()

    for required in _KNOWN_SECTIONS:
        if required not in catalog.declared:
            msg = f"the catalog has no {required!r} section"
            raise CatalogRefusal(msg)
    for owner_name, rows in catalog.labs.items():
        declared = catalog.declared[owner_name]
        if declared != len(rows):
            findings.append(
                f"lab section {owner_name!r} declares {declared} rows, its table holds {len(rows)}"
            )
    actual = {
        _EXAMPLES_SECTION: len(catalog.examples),
        _LAB_SECTION: sum(len(rows) for rows in catalog.labs.values()),
        _INTEGRATION_SECTION: len(catalog.integration),
    }
    for section_name, count in actual.items():
        declared = catalog.declared[section_name]
        if declared != count:
            findings.append(
                f"section {section_name!r} declares {declared} rows, its tables hold {count}"
            )
    return catalog, findings


def _justfile_recipes(text: str) -> frozenset[str]:
    """Collect the recipe names the current justfile defines.

    Args:
        text: The justfile's text.

    Returns:
        The defined recipe names.

    Raises:
        CatalogRefusal: The justfile yields no recipe names.

    """
    names: set[str] = set()
    for line in text.splitlines():
        if line.startswith(("#", " ", "\t")):
            continue
        if _JUST_DIRECTIVE.match(line):
            continue
        if _VARIABLE_DEF.match(line):
            continue
        match = _RECIPE_DEF.match(line)
        if match is not None and _RECIPE_NAME.match(match.group(1)):
            names.add(match.group(1))
    if not names:
        msg = "the justfile yields no recipe names"
        raise CatalogRefusal(msg)
    return frozenset(names)


def _load_metadata(root: Path) -> Json:
    """Run cargo metadata for the workspace.

    Args:
        root: The repository root holding ``rust/Cargo.toml``.

    Returns:
        The parsed metadata payload.

    Raises:
        CatalogRefusal: Cargo cannot run, fails, or prints unparseable output.

    """
    command = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--locked",
        "--offline",
        "--manifest-path",
        str(root / "rust" / "Cargo.toml"),
    ]
    try:
        proc = subprocess.run(
            command,
            cwd=root,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        msg = f"cargo metadata could not run: {error}"
        raise CatalogRefusal(msg) from error
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout).strip()
        msg = f"cargo metadata failed (exit {proc.returncode}): {detail[-1500:]}"
        raise CatalogRefusal(msg)
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError as error:
        msg = f"cargo metadata printed unparseable JSON: {error}"
        raise CatalogRefusal(msg) from error
    return payload


def _target_view(raw: Json) -> TargetView:
    """Convert one metadata target entry.

    Args:
        raw: The metadata target object.

    Returns:
        The target as the reconciliation sees it.

    Raises:
        CatalogRefusal: The entry cannot be attributed: it is not a JSON
            object, or it carries no 'name' or 'src_path', or a 'kind' or
            'required-features' that is not a list of strings.

    """
    if not isinstance(raw, dict):
        msg = "a cargo metadata target entry is not a JSON object"
        raise CatalogRefusal(msg)
    name = raw.get("name")
    if not isinstance(name, str) or not name:
        msg = "a cargo metadata target entry carries no 'name'"
        raise CatalogRefusal(msg)
    kinds = raw.get("kind", [])
    if isinstance(kinds, str):
        kinds = [kinds]
    if not isinstance(kinds, list) or not all(isinstance(kind, str) for kind in kinds):
        msg = f"the cargo metadata target {name!r} carries a 'kind' that is not a list of strings"
        raise CatalogRefusal(msg)
    src_path = raw.get("src_path")
    if not isinstance(src_path, str):
        msg = f"the cargo metadata target {name!r} carries no 'src_path'"
        raise CatalogRefusal(msg)
    features = raw.get("required-features") or []
    if not isinstance(features, list) or not all(isinstance(f, str) for f in features):
        msg = (
            f"the cargo metadata target {name!r} carries a 'required-features' "
            "that is not a list of strings"
        )
        raise CatalogRefusal(msg)
    return TargetView(
        name=name,
        kinds=frozenset(kinds),
        src_path=Path(src_path).resolve(),
        required_features=frozenset(features),
    )


def _catalog_view(metadata: Json, root: Path) -> dict[str, PackageView]:
    """Convert the metadata package list into per-package target views.

    Args:
        metadata: The parsed metadata payload.
        root: The repository root a tool package's manifest must live under.

    Returns:
        One PackageView per package, keyed by package name.

    Raises:
        CatalogRefusal: The payload carries no package list.

    """
    if not isinstance(metadata.get("packages"), list):
        msg = "the cargo metadata carries no package list"
        raise CatalogRefusal(msg)
    view: dict[str, PackageView] = {}
    for pkg in metadata["packages"]:
        if not isinstance(pkg, dict):
            msg = "a cargo metadata package entry is not a JSON object"
            raise CatalogRefusal(msg)
        name = pkg.get("name")
        if not isinstance(name, str) or not name:
            msg = "a cargo metadata package entry carries no 'name'"
            raise CatalogRefusal(msg)
        manifest = pkg.get("manifest_path")
        if not isinstance(manifest, str):
            msg = f"the cargo metadata package {name!r} carries no 'manifest_path'"
            raise CatalogRefusal(msg)
        targets = pkg.get("targets", [])
        if not isinstance(targets, list):
            msg = f"the cargo metadata package {name!r} carries a 'targets' that is not a list"
            raise CatalogRefusal(msg)
        manifest = Path(manifest).resolve()
        try:
            relative = manifest.relative_to(root)
        except ValueError:
            relative = None
        is_tool = relative is not None and relative.parts[:2] == ("rust", "tools")
        view[name] = PackageView(
            name=name,
            manifest=manifest,
            is_tool=is_tool,
            targets=tuple(_target_view(raw) for raw in targets),
        )
    return view


def _lane_features(cell: str | None) -> tuple[frozenset[str], str | None]:
    """Read the documented primary Lane's features out of one Lane cell.

    Everything from the first `` (`` onward is an explicitly documented
    secondary build shape and is ignored; ``default`` means no features.

    Args:
        cell: The row's Lane cell, or None for non-lab rows.

    Returns:
        The primary lane's features and the finding message; the features are
        an empty frozenset and the message None for a clean ``default`` lane.

    """
    if cell is None or not cell.strip():
        return frozenset[str](), "the Lane cell is empty"
    primary = cell.split(" (", 1)[0].strip()
    if primary in _DEFAULT_LANES:
        return frozenset[str](), None
    names = _CODE_SPAN.findall(primary)
    if not names:
        return frozenset[str](), f"the Lane cell {cell!r} names no feature in a code span"
    if "default" in names:
        return frozenset[str](), f"the Lane cell {cell!r} mixes 'default' with features"
    return frozenset[str](names), None


def _bin_owner(packages: dict[str, PackageView], src_path: Path) -> str | None:
    """Find the package whose ``[[bin]]`` lives at a source path.

    Args:
        packages: The metadata package views.
        src_path: The bin source path to attribute.

    Returns:
        The owning package's name, or None when no cataloged ``[[bin]]`` is
        built from that path.

    """
    for pkg in packages.values():
        if any(
            _BIN_TARGET_KIND in target.kinds and target.src_path == src_path
            for target in pkg.targets
        ):
            return pkg.name
    return None


def _reconcile_examples(
    packages: dict[str, PackageView],
    catalog: Catalog,
    root: Path,
    findings: list[str],
    seen: dict[Path, str],
) -> None:
    """Reconcile the examples section against the live example targets.

    Args:
        packages: The metadata package views.
        catalog: The parsed catalog.
        root: The repository root paths are displayed relative to.
        findings: The accumulated findings to append to.
        seen: The target source paths other populations have cataloged, updated
            in place.

    """
    live = {
        target.src_path: (pkg.name, target)
        for pkg in packages.values()
        if not pkg.is_tool
        for target in pkg.targets
        if _EXAMPLE_TARGET_KIND in target.kinds
    }
    matched: set[Path] = set()
    for row in catalog.examples:
        if row.source is None:
            continue
        label = _label(row)
        if row.source in seen:
            findings.append(
                f"{label}: {_display(row.source, root)} is already cataloged by {seen[row.source]}"
            )
            continue
        seen[row.source] = label
        entry = live.get(row.source)
        if entry is None:
            findings.append(
                f"{label}: no live example target lives at {_display(row.source, root)}"
            )
            continue
        pkg_name, target = entry
        if target.name != row.name:
            msg = (
                f"{label}: the example target at this source is named {target.name!r} "
                f"(package {pkg_name}), not {row.name!r}"
            )
            findings.append(msg)
            continue
        matched.add(row.source)
    for src, (pkg_name, target) in live.items():
        if src not in matched:
            findings.append(
                f"live example target {pkg_name}#{target.name} at {_display(src, root)} "
                "has no catalog row"
            )


def _reconcile_lab_rows(
    owner: str,
    rows: list[CatalogRow],
    bins: dict[Path, TargetView],
    packages: dict[str, PackageView],
    root: Path,
    findings: list[str],
    seen: dict[Path, str],
) -> set[Path]:
    """Reconcile one lab section's rows against its owning package's ``[[bin]]``s.

    Args:
        owner: The section's tool package.
        rows: The section's catalog rows.
        bins: The package's live ``[[bin]]``s by source path.
        packages: The metadata package views (for the drift hint).
        root: The repository root paths are displayed relative to.
        findings: The accumulated findings to append to.
        seen: The target source paths other populations have cataloged, updated
            in place.

    Returns:
        The source paths of the ``[[bin]]``s the section's rows matched.

    """
    matched: set[Path] = set()
    for row in rows:
        if row.source is None:
            continue
        label = _label(row)
        if row.source in seen:
            findings.append(
                f"{label}: {_display(row.source, root)} is already cataloged by {seen[row.source]}"
            )
            continue
        seen[row.source] = label
        target = bins.get(row.source)
        if target is None:
            other = _bin_owner(packages, row.source)
            hint = f" (it is a [[bin]] of {other})" if other else ""
            findings.append(
                f"{label}: no live [[bin]] of {owner} lives at {_display(row.source, root)}{hint}"
            )
            continue
        if target.name != row.name:
            msg = f"{label}: the [[bin]] at this source is named {target.name!r}, not {row.name!r}"
            findings.append(msg)
            continue
        matched.add(row.source)
        if row.lane is None:
            continue
        lane_features, lane_finding = _lane_features(row.lane)
        if lane_finding is not None:
            findings.append(f"{label}: {lane_finding}")
        elif lane_features != target.required_features:
            msg = (
                f"{label}: the documented primary Lane {sorted(lane_features)} does not "
                f"match the [[bin]]'s required-features {sorted(target.required_features)}"
            )
            findings.append(msg)
    return matched


def _reconcile_labs(
    packages: dict[str, PackageView],
    catalog: Catalog,
    root: Path,
    findings: list[str],
    seen: dict[Path, str],
) -> None:
    """Reconcile every current tool package's ``[[bin]]``s against its Lab section.

    Discovery is metadata-driven: the current tool packages are the packages
    whose manifests live under ``rust/tools``, not the sections the README
    happens to declare. Each must have exactly one Lab section, and its
    ``[[bin]]``s are reconciled in both directions whether or not a section
    names it yet. A catalog section naming an absent or non-tool package is
    still a finding.

    Args:
        packages: The metadata package views.
        catalog: The parsed catalog.
        root: The repository root paths are displayed relative to.
        findings: The accumulated findings to append to.
        seen: The target source paths other populations have cataloged, updated
            in place.

    """
    for pkg in packages.values():
        if not pkg.is_tool:
            continue
        rows = catalog.labs.get(pkg.name)
        if rows is None:
            findings.append(
                f"current tool package {pkg.name} ({_display(pkg.manifest, root)}) has no "
                f"{_LAB_SECTION!r} section in the catalog"
            )
            rows = []
        bins = {
            target.src_path: target for target in pkg.targets if _BIN_TARGET_KIND in target.kinds
        }
        matched = _reconcile_lab_rows(pkg.name, rows, bins, packages, root, findings, seen)
        for src, target in bins.items():
            if src not in matched:
                findings.append(
                    f"{pkg.name}'s [[bin]] {target.name} at {_display(src, root)} "
                    "has no catalog row"
                )
    for owner in catalog.labs:
        pkg = packages.get(owner)
        if pkg is None:
            findings.append(f"lab section {owner!r}: no such workspace package")
        elif not pkg.is_tool:
            findings.append(
                f"lab section {owner!r}: {_display(pkg.manifest, root)} is not under rust/tools"
            )


def _reconcile_integration(
    packages: dict[str, PackageView],
    catalog: Catalog,
    root: Path,
    findings: list[str],
    seen: dict[Path, str],
) -> None:
    """Reconcile the current-source rule's integration tests in both directions.

    The rule selects exactly two live test targets. A live selected target
    without a row, a selected row without its live target, and a row outside
    the rule all fail. A target the rule once selected whose row and target
    are both gone is a removed current target, not a revival tripwire: nothing
    is asserted about it.

    Args:
        packages: The metadata package views.
        catalog: The parsed catalog.
        root: The repository root paths are displayed relative to.
        findings: The accumulated findings to append to.
        seen: The target source paths other populations have cataloged, updated
            in place.

    """
    live = {
        (pkg.name, target.name): target
        for pkg in packages.values()
        for target in pkg.targets
        if _TEST_TARGET_KIND in target.kinds
    }
    rule_names = frozenset(name for _, name in _CURRENT_INTEGRATION_TESTS)
    selected = {
        name: (pkg_name, live[pkg_name, name])
        for pkg_name, name in _CURRENT_INTEGRATION_TESTS
        if (pkg_name, name) in live
    }
    matched: set[str] = set()
    for row in catalog.integration:
        if row.source is None:
            continue
        label = _label(row)
        if row.source in seen:
            findings.append(
                f"{label}: {_display(row.source, root)} is already cataloged by {seen[row.source]}"
            )
            continue
        seen[row.source] = label
        if row.name not in rule_names:
            findings.append(
                f"{label}: is not part of this catalog surface; the current-source rule "
                f"selects only {sorted(rule_names)}"
            )
            continue
        selection = selected.get(row.name)
        if selection is None:
            findings.append(
                f"{label}: no live integration-test target lives at {_display(row.source, root)}"
            )
            continue
        pkg_name, target = selection
        if target.src_path != row.source:
            msg = (
                f"{label}: the {row.name!r} test target lives at "
                f"{_display(target.src_path, root)} (package {pkg_name}), not here"
            )
            findings.append(msg)
            continue
        matched.add(row.name)
    for pkg_name, name in _CURRENT_INTEGRATION_TESTS:
        target = live.get((pkg_name, name))
        if name not in matched and target is not None:
            findings.append(
                f"the current-source rule's live integration-test target {pkg_name}#{name} "
                f"at {_display(target.src_path, root)} has no catalog row"
            )


def _reconcile_recipes(catalog: Catalog, recipes: frozenset[str], findings: list[str]) -> None:
    """Check every ``just <recipe>`` a row references against the justfile.

    Args:
        catalog: The parsed catalog.
        recipes: The recipe names the current justfile defines.
        findings: The accumulated findings to append to.

    """
    findings.extend(
        f"{_label(row)}: references `just {ref}`, which the justfile does not define"
        for row in catalog.rows()
        for ref in row.just_refs
        if ref not in recipes
    )


def reconcile(
    packages: dict[str, PackageView],
    catalog: Catalog,
    recipes: frozenset[str],
    root: Path,
) -> list[str]:
    """Reconcile the parsed catalog against the metadata view and recipes.

    Args:
        packages: The metadata package views.
        catalog: The parsed catalog.
        recipes: The recipe names the current justfile defines.
        root: The repository root paths are displayed relative to.

    Returns:
        Every finding, each naming the row it concerns.

    """
    findings: list[str] = []
    seen: dict[Path, str] = {}
    _reconcile_examples(packages, catalog, root, findings, seen)
    _reconcile_labs(packages, catalog, root, findings, seen)
    _reconcile_integration(packages, catalog, root, findings, seen)
    _reconcile_recipes(catalog, recipes, findings)
    return findings


def _read(path: Path) -> str:
    """Read one UTF-8 input.

    Args:
        path: The file to read.

    Returns:
        The file's text.

    Raises:
        CatalogRefusal: The file cannot be read.

    """
    try:
        return path.read_text(encoding="utf-8")
    except OSError as error:
        shown = _display(path, _REPO_ROOT)
        msg = f"unreadable input {shown}: {error}"
        raise CatalogRefusal(msg) from error


def _load_inputs(
    root: Path,
    readme: Path,
    justfile: Path,
    metadata_arg: str | None,
) -> tuple[Json, str, str]:
    """Load the gate's three inputs.

    Args:
        root: The repository root.
        readme: The catalog's path.
        justfile: The justfile's path.
        metadata_arg: A metadata JSON dump's path, or None to run cargo.

    Returns:
        The metadata payload, the README text, and the justfile text.

    Raises:
        CatalogRefusal: Any input is missing, unreadable, or unparseable.

    """
    if metadata_arg is not None:
        try:
            metadata = json.loads(_read(Path(metadata_arg)))
        except json.JSONDecodeError as error:
            msg = f"the metadata JSON dump is unparseable: {error}"
            raise CatalogRefusal(msg) from error
    else:
        metadata = _load_metadata(root)
    if not isinstance(metadata, dict):
        msg = "the metadata payload is not a JSON object"
        raise CatalogRefusal(msg)
    return metadata, _read(readme), _read(justfile)


def main(argv: Sequence[str] | None = None) -> int:
    """Run the gate.

    Args:
        argv: The command-line arguments, without the program name.

    Returns:
        The process exit code: 0 reconciled, 1 findings, 2 refused to certify.

    """
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else None)
    parser.add_argument("--root", default=str(_REPO_ROOT), help="the repository root")
    parser.add_argument(
        "--readme", default=None, help="the catalog (default: <root>/rust/tools/README.md)"
    )
    parser.add_argument("--justfile", default=None, help="the justfile (default: <root>/justfile)")
    parser.add_argument(
        "--metadata",
        default=None,
        help="a cargo metadata JSON dump; the default runs cargo metadata itself",
    )
    args = parser.parse_args(argv)

    root = Path(args.root).resolve()
    readme = Path(args.readme) if args.readme is not None else root / _README
    justfile = Path(args.justfile) if args.justfile is not None else root / _JUSTFILE

    try:
        metadata, readme_text, justfile_text = _load_inputs(root, readme, justfile, args.metadata)
        catalog, findings = parse_catalog(readme_text, readme.parent, root)
        recipes = _justfile_recipes(justfile_text)
        packages = _catalog_view(metadata, root)
    except CatalogRefusal as error:
        print(f"tool-catalog: {error}", file=sys.stderr)
        print("tool-catalog: refusing to certify a catalog it could not examine", file=sys.stderr)
        return 2

    findings.extend(reconcile(packages, catalog, recipes, root))
    if findings:
        shown = _display(readme, root)
        print(f"tool-catalog: {len(findings)} finding(s) in {shown}", file=sys.stderr)
        for finding in findings:
            print(f"  {finding}", file=sys.stderr)
        return 1

    lab_rows = sum(len(rows) for rows in catalog.labs.values())
    print(
        f"tool-catalog: {len(catalog.examples)} public example rows, {lab_rows} lab rows across "
        f"{len(catalog.labs)} tool package(s), and {len(catalog.integration)} integration rows "
        "reconcile against cargo metadata and the justfile"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
