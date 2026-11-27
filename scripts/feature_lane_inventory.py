#!/usr/bin/env python3
"""Feature-gated test lanes: one mechanical inventory, one verified filter.

A feature-gated test is a test the compiler only builds when a cargo feature is
on: a whole target behind `required-features` or a file-level `#![cfg(...)]`, or
an inline `#[test]`/`#[tokio::test]` item behind a feature `cfg`. Nothing in the
workspace lists those units, so a lane that \"enables the feature\" can pass green
over a target it never built, and a lane can silently drag a live probe or an
abort-by-design repro into an offline gate. This script is the one inventory and
the one filter:

* the inventory is mechanical -- cargo metadata names the targets and their
  `required-features`, and a module walker derives every inline item's full test
  path and effective gate. No hand-maintained list exists to fall out of date.
* `scripts/feature-lane-dispositions.txt` classifies the units that must not run
  in the offline lane (`live`, `ignored`, `measurement`), each with its owning
  ADR. Everything not named there is `offline`, so a newly added offline test is
  included without an edit. A disposition that names a unit the tree no longer
  has, a mechanically `#[ignore]`d unit that is not classified `ignored`, or a
  live-gated binary left `offline` fails the gate.
* the nextest filter is GENERATED from that classification, so the recipe cannot
  run a different selection than the one this gate verified.
* `verify` reconciles a `cargo nextest list --message-format json` capture
  against the inventory in both directions: every offline unit is listed and
  selected, every live/ignored/measurement unit exists in the build and is NOT
  selected. `--negative-probe` takes the UNFILTERED capture and requires the same
  reconciliation to FAIL, proving the exclusion is the filter's work and not a
  test that quietly stopped existing.

Scope: the lane's own feature set (`--features`, `<package>/<feature>` or bare
`<feature>`). A test gated on a feature the lane does not enable cannot appear in
that lane's binaries, so it is another lane's claim; the inventory prints those
units as out-of-lane instead of dropping them silently.

Exit codes: 0 reconciled, 1 findings, 2 refused to certify (bad input, or a
tree/metadata read that cannot answer the question).
"""

from __future__ import annotations

import argparse
import json
import platform
import re
import subprocess  # ruff: ignore[suspicious-subprocess-import] -- cargo is the target authority
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import Iterable, Sequence

_REPO_ROOT = Path(__file__).resolve().parents[1]
_WORKSPACE = Path("rust") / "Cargo.toml"
_DISPOSITIONS = Path("scripts") / "feature-lane-dispositions.txt"

CLASSES = ("offline", "live", "ignored", "measurement")
_NON_OFFLINE = frozenset({"live", "ignored", "measurement"})

# An env gate that skips a test body when the service is absent is a LIVE probe
# even when the item carries no `#[ignore]`: absence of the service must not
# turn into a green run. A binary that reads one of these must carry a `live`
# or `ignored` binary disposition.
_LIVE_GATE_ENV = re.compile(r"DEGENBOT_[A-Z0-9_]*(?:E2E|_LIVE)\b")

_ADR = re.compile(r"^ADR-\d{3}$")


# The lane is built and run on this host, so the platform predicates a gate may
# use are answered by the host itself. Anything outside this table is NOT
# modeled: a unit whose gate depends on one is a finding (fail closed), never a
# silent does-not-compile or compiles.
_HOST_OS = sys.platform
_TARGET_OS = {"linux": "linux", "darwin": "macos", "win32": "windows"}.get(_HOST_OS, _HOST_OS)
_TARGET_FAMILY = "windows" if _HOST_OS == "win32" else "unix"
_HOST_ARCH = platform.machine().lower()
_TARGET_ARCH = {"amd64": "x86_64", "arm64": "aarch64"}.get(_HOST_ARCH, _HOST_ARCH)
_TARGET_POINTER_WIDTH = "64" if sys.maxsize > 2**32 else "32"


def _host_predicate(name: str, literal: str) -> bool | None:
    """Answer one host predicate, or None when it is not modeled here."""
    if not literal:
        if name == "unix":
            return _TARGET_FAMILY == "unix"
        if name == "windows":
            return _TARGET_FAMILY == "windows"
        if name == "debug_assertions":
            return True
        return None
    if name == "target_os":
        return literal == _TARGET_OS
    if name == "target_family":
        return literal == _TARGET_FAMILY
    if name == "target_arch":
        return literal == _TARGET_ARCH
    if name == "target_pointer_width":
        return literal == _TARGET_POINTER_WIDTH
    return None


class Refusal(Exception):
    """The script cannot examine what it needs: fail closed instead of passing."""


# --- cfg expressions ---------------------------------------------------------


@dataclass(frozen=True)
class Cfg:
    """One `cfg(...)` predicate tree, evaluated against a feature set."""

    op: str
    value: str = ""
    literal: str = ""
    args: tuple[Cfg, ...] = ()

    def evaluate(self, features: frozenset[str], *, test: bool) -> bool:
        """Evaluate the predicate against the active features (test cfg on)."""
        if self.op == "all":
            return all(arg.evaluate(features, test=test) for arg in self.args)
        if self.op == "any":
            return any(arg.evaluate(features, test=test) for arg in self.args)
        if self.op == "not":
            return not self.args[0].evaluate(features, test=test)
        if self.op == "feature":
            return self.value in features
        if self.op == "test":
            return test
        return _host_predicate(self.value, self.literal) is True

    def features(self) -> frozenset[str]:
        """Every feature name the predicate mentions, whatever its polarity."""
        if self.op == "feature":
            return frozenset({self.value})
        found: set[str] = set()
        for arg in self.args:
            found |= arg.features()
        return frozenset(found)

    def unmodeled_predicates(self) -> frozenset[str]:
        """Every unmodeled predicate name (`unix`, `target_os`, ...)."""
        found: set[str] = set()
        if self.op == "unknown":
            if _host_predicate(self.value, self.literal) is None:
                found.add(self.value)
            return frozenset(found)
        for arg in self.args:
            found |= arg.unmodeled_predicates()
        return frozenset(found)


_CFG_TOKEN = re.compile(
    r'\s*(?:(?P<ident>[A-Za-z_][A-Za-z0-9_]*)|(?P<string>"(?:[^"\\]|\\.)*")|(?P<punct>[(),=]))'
)


def _cfg_tokens(expr: str) -> list[tuple[str, str]]:
    tokens: list[tuple[str, str]] = []
    index = 0
    while index < len(expr):
        match = _CFG_TOKEN.match(expr, index)
        if match is None:
            message = f"unparseable cfg expression: {expr!r}"
            raise Refusal(message)
        index = match.end()
        if match.group("ident") is not None:
            tokens.append(("ident", match.group("ident")))
        elif match.group("string") is not None:
            tokens.append(("string", match.group("string")[1:-1]))
        else:
            tokens.append(("punct", match.group("punct")))
    return tokens


def parse_cfg(expr: str) -> Cfg:
    """Parse a `cfg(...)` argument list into a predicate tree."""
    tokens = _cfg_tokens(expr)
    cursor = 0

    def peek() -> tuple[str, str] | None:
        return tokens[cursor] if cursor < len(tokens) else None

    def take() -> tuple[str, str]:
        nonlocal cursor
        token = tokens[cursor]
        cursor += 1
        return token

    def parse() -> Cfg:
        kind, value = take()
        if kind != "ident":
            message = f"cfg expression does not start with a predicate: {expr!r}"
            raise Refusal(message)
        if peek() == ("punct", "="):
            take()
            string_kind, string_value = take()
            if string_kind != "string":
                message = f"cfg predicate {value!r} needs a string value: {expr!r}"
                raise Refusal(message)
            if value == "feature":
                return Cfg("feature", value=string_value)
            return Cfg("unknown", value=value, literal=string_value)
        if value == "test":
            return Cfg("test")
        if peek() != ("punct", "("):
            return Cfg("unknown", value=value)
        take()
        args: list[Cfg] = []
        if peek() != ("punct", ")"):
            args.append(parse())
            while peek() == ("punct", ","):
                take()
                args.append(parse())
        if peek() != ("punct", ")"):
            message = f"unbalanced cfg expression: {expr!r}"
            raise Refusal(message)
        take()
        if value in {"all", "any", "not"}:
            return Cfg(value, args=tuple(args))
        return Cfg("unknown", value=value)

    return parse()


def _matching_paren(text: str, open_index: int) -> int:
    depth = 0
    for index in range(open_index, len(text)):
        if text[index] == "(":
            depth += 1
        elif text[index] == ")":
            depth -= 1
            if depth == 0:
                return index
    message = f"unbalanced parenthesis in {text!r}"
    raise Refusal(message)


def cfg_of_attribute(attribute: str) -> Cfg | None:
    """Extract the `cfg(...)` predicate from one attribute, if it carries one."""
    match = re.search(r"\bcfg\s*\(", attribute)
    if match is None:
        return None
    return parse_cfg(attribute[match.end() : _matching_paren(attribute, match.end() - 1)])


# --- Rust source walk --------------------------------------------------------

_TEST_ATTRIBUTE = re.compile(r"^#\[\s*(?:[A-Za-z_][A-Za-z0-9_]*::)*(?:test|rstest|test_case)\b")
_IGNORE_ATTRIBUTE = re.compile(r"^#\[\s*ignore\b")
_MOD_DECLARATION = re.compile(r"\bmod\s+([A-Za-z_][A-Za-z0-9_]*)")
_FN_DECLARATION = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")
_ITEM_KEYWORD = re.compile(
    r"\b(?:struct|enum|union|trait|impl|const|static|type|use|let|macro_rules)\b"
)
_PATH_ATTRIBUTE = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]')


def _raw_string_end(text: str, index: int) -> int | None:
    match = re.match(r'r(#*)"', text[index:])
    if match is None:
        return None
    closing = '"' + match.group(1)
    end = text.find(closing, index + match.end())
    if end < 0:
        return None
    return end + len(closing)


def _char_literal_end(text: str, index: int) -> int | None:
    match = re.match(r"'(?:\\.|\\u\{[0-9A-Fa-f_]+\}|[^\\'])'", text[index:])
    if match is None:
        return None
    return index + match.end()


def _blank(text: str, out: list[str], start: int, end: int) -> None:
    for offset in range(start, end):
        if text[offset] != "\n":
            out[offset] = " "


def _block_comment_end(text: str, index: int) -> int:
    depth = 1
    cursor = index + 2
    while cursor < len(text) and depth:
        if text.startswith("/*", cursor):
            depth += 1
            cursor += 2
        elif text.startswith("*/", cursor):
            depth -= 1
            cursor += 2
        else:
            cursor += 1
    return cursor


def _string_end(text: str, index: int) -> int:
    cursor = index + 1
    while cursor < len(text):
        if text[cursor] == "\\":
            cursor += 2
            continue
        cursor += 1
        if text[cursor - 1] == '"':
            break
    return cursor


def _sanitize(text: str) -> str:
    """Blank comments and literals, preserving offsets, so braces are reliable."""
    out = list(text)
    index = 0
    length = len(text)
    while index < length:
        char = text[index]
        if text.startswith("//", index):
            newline = text.find("\n", index)
            end = length if newline < 0 else newline
        elif text.startswith("/*", index):
            end = _block_comment_end(text, index)
        elif char == "r" and _raw_string_end(text, index) is not None:
            end = _raw_string_end(text, index) or index + 1
        elif char == '"':
            end = _string_end(text, index)
        elif char == "'" and _char_literal_end(text, index) is not None:
            end = _char_literal_end(text, index) or index + 1
        else:
            index += 1
            continue
        _blank(text, out, index, end)
        index = end
    return "".join(out)


@dataclass(frozen=True)
class Unit:
    """One feature-gated test item: a whole target's item or an inline item."""

    package: str
    binary: str
    item: str
    features: tuple[str, ...]
    gates: tuple[Cfg, ...]
    ignored: bool
    source: str

    @property
    def key(self) -> tuple[str, str]:
        """The nextest identity the unit must appear under."""
        return (self.binary, self.item)

    def unmodeled(self) -> frozenset[str]:
        """Every unmodeled predicate that can decide this unit's gate."""
        found: set[str] = set()
        for gate in self.gates:
            found |= gate.unmodeled_predicates()
        return frozenset(found)

    def enabled(self, features: frozenset[str]) -> bool:
        """Report whether this unit compiles with the given feature set."""
        return all(gate.evaluate(features, test=True) for gate in self.gates)


@dataclass
class _Frame:
    open_depth: int
    prefix: str
    gates: tuple[Cfg, ...]


@dataclass
class Snapshot:
    """The mechanical inventory: every feature-gated test item in the workspace."""

    units: list[Unit]
    sources: dict[str, set[Path]]


def _module_file(parent: Path, name: str, path_attribute: str | None) -> Path | None:
    if path_attribute is not None:
        candidate = parent / path_attribute
        return candidate if candidate.is_file() else None
    for candidate in (parent / f"{name}.rs", parent / name / "mod.rs"):
        if candidate.is_file():
            return candidate
    return None


def _word_char(char: str) -> bool:
    return char.isalnum() or char == "_"


def _skip_whitespace(text: str, index: int) -> int:
    while index < len(text) and text[index].isspace():
        index += 1
    return index


def _matching_bracket(text: str, open_index: int) -> int:
    depth = 0
    for index in range(open_index, len(text)):
        if text[index] == "[":
            depth += 1
        elif text[index] == "]":
            depth -= 1
            if depth == 0:
                return index
    message = f"unbalanced attribute bracket near {text[open_index : open_index + 80]!r}"
    raise Refusal(message)


def _features_of(gates: Iterable[Cfg]) -> frozenset[str]:
    found: set[str] = set()
    for gate in gates:
        found |= gate.features()
    return frozenset(found)


def _walk_file(
    path: Path,
    *,
    prefix: str,
    gates: tuple[Cfg, ...],
    binary: str,
    package: str,
    units: list[Unit],
    sources: dict[str, set[Path]],
    root: Path,
    visited: set[Path],
) -> None:
    """Record every feature-gated test item in one module file and its children."""
    path = path.resolve()
    if path in visited:
        return
    visited.add(path)
    sources.setdefault(binary, set()).add(path)
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        message = f"cannot read {path}: {error}"
        raise Refusal(message) from error
    sanitized = _sanitize(text)
    relative = path.relative_to(root).as_posix()
    file_gates: list[Cfg] = []
    pending: list[str] = []
    frames: list[_Frame] = []
    depth = 0
    index = 0
    length = len(sanitized)

    def current_prefix() -> str:
        return frames[-1].prefix if frames else prefix

    def current_gates() -> tuple[Cfg, ...]:
        inherited = tuple(gates) + tuple(file_gates)
        return inherited + (frames[-1].gates if frames else ())

    while index < length:
        if sanitized.startswith("#[", index) or sanitized.startswith("#![", index):
            inner = sanitized.startswith("#![", index)
            close = _matching_bracket(sanitized, index + 1)
            attribute = text[index : close + 1]
            index = close + 1
            gate = cfg_of_attribute(attribute)
            if inner:
                if gate is not None:
                    file_gates.append(gate)
            else:
                pending.append(attribute)
            continue
        char = sanitized[index]
        if char == "{":
            depth += 1
            pending.clear()
            index += 1
            continue
        if char == "}":
            depth -= 1
            while frames and frames[-1].open_depth > depth:
                frames.pop()
            pending.clear()
            index += 1
            continue
        mod_match = _MOD_DECLARATION.match(sanitized, index)
        if mod_match is not None and (index == 0 or not _word_char(sanitized[index - 1])):
            name = mod_match.group(1)
            cursor = _skip_whitespace(sanitized, mod_match.end())
            declared = tuple(
                gate for attribute in pending if (gate := cfg_of_attribute(attribute)) is not None
            )
            path_attribute = next(
                (
                    match.group(1)
                    for attribute in pending
                    if (match := _PATH_ATTRIBUTE.match(attribute)) is not None
                ),
                None,
            )
            child_prefix = f"{current_prefix()}{name}::"
            if sanitized[cursor : cursor + 1] == "{":
                depth += 1
                frames.append(_Frame(open_depth=depth, prefix=child_prefix, gates=declared))
                pending.clear()
                index = cursor + 1
                continue
            pending.clear()
            index = cursor + 1
            child = _module_file(path.parent, name, path_attribute)
            if child is not None:
                _walk_file(
                    child,
                    prefix=child_prefix,
                    gates=current_gates() + declared,
                    binary=binary,
                    package=package,
                    units=units,
                    sources=sources,
                    root=root,
                    visited=visited,
                )
            continue
        fn_match = _FN_DECLARATION.match(sanitized, index)
        if fn_match is not None and (index == 0 or not _word_char(sanitized[index - 1])):
            name = fn_match.group(1)
            item_gates = tuple(
                gate for attribute in pending if (gate := cfg_of_attribute(attribute)) is not None
            )
            is_test = any(_TEST_ATTRIBUTE.match(attribute) for attribute in pending)
            ignored = any(_IGNORE_ATTRIBUTE.match(attribute) for attribute in pending)
            effective = current_gates() + item_gates
            pending.clear()
            index = fn_match.end()
            if is_test and _features_of(effective):
                units.append(
                    Unit(
                        package=package,
                        binary=binary,
                        item=f"{current_prefix()}{name}",
                        features=tuple(sorted(_features_of(effective))),
                        gates=effective,
                        ignored=ignored,
                        source=relative,
                    )
                )
            continue
        keyword_match = _ITEM_KEYWORD.match(sanitized, index)
        if keyword_match is not None and (index == 0 or not _word_char(sanitized[index - 1])):
            pending.clear()
            index = keyword_match.end()
            continue
        if not char.isspace():
            pending.clear()
        index += 1


# --- workspace inventory -----------------------------------------------------


def _checked_metadata(payload: object) -> dict[str, Any]:
    if not isinstance(payload, dict):
        message = "the cargo metadata is not a JSON object"
        raise Refusal(message)
    if not isinstance(payload.get("packages"), list):
        message = "the cargo metadata carries no package list"
        raise Refusal(message)
    return payload


def load_metadata(root: Path, metadata_path: Path | None) -> dict[str, Any]:
    """Read the workspace metadata from cargo (or a JSON dump)."""
    if metadata_path is not None:
        try:
            payload = json.loads(metadata_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            message = f"cannot read metadata dump {metadata_path}: {error}"
            raise Refusal(message) from error
        return _checked_metadata(payload)
    command = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--locked",
        "--offline",
        "--manifest-path",
        str(root / _WORKSPACE),
    ]
    try:
        proc = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] -- literal cargo argv
            command, cwd=root, capture_output=True, text=True, check=False
        )
    except OSError as error:
        message = f"cargo metadata could not run: {error}"
        raise Refusal(message) from error
    if proc.returncode != 0:
        message = f"cargo metadata failed (exit {proc.returncode}): {proc.stderr[-1500:]}"
        raise Refusal(message)
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError as error:
        message = f"cargo metadata printed unparseable JSON: {error}"
        raise Refusal(message) from error
    return _checked_metadata(payload)


def _binary_id(package: str, target: dict[str, Any]) -> str | None:
    """Nextest's binary id for a lib/test target, or None for another kind."""
    kinds = target.get("kind")
    if not isinstance(kinds, list):
        message = f"target {target.get('name')!r} carries no kind list"
        raise Refusal(message)
    if any(kind in kinds for kind in ("lib", "rlib", "proc-macro", "dylib")):
        return package
    if "test" in kinds or "bench" in kinds:
        return f"{package}::{target.get('name')}"
    return None


def enumerate_units(root: Path, metadata: dict[str, Any]) -> Snapshot:
    """Walk every workspace crate and record its feature-gated test items."""
    units: list[Unit] = []
    sources: dict[str, set[Path]] = {}
    rust_root = (root / "rust").resolve()
    packages = metadata.get("packages")
    assert isinstance(packages, list)
    for package in sorted(packages, key=lambda entry: entry.get("name", "")):
        if not isinstance(package, dict):
            message = "a cargo metadata package entry is not a JSON object"
            raise Refusal(message)
        name = package.get("name")
        manifest = package.get("manifest_path")
        targets = package.get("targets")
        if (
            not isinstance(name, str)
            or not isinstance(manifest, str)
            or not isinstance(targets, list)
        ):
            message = "a cargo metadata package entry is missing name/manifest_path/targets"
            raise Refusal(message)
        manifest_path = Path(manifest).resolve()
        if rust_root not in manifest_path.parents or "vendor" in manifest_path.parts:
            continue
        for target in targets:
            if not isinstance(target, dict):
                message = f"a target of {name!r} is not a JSON object"
                raise Refusal(message)
            binary = _binary_id(name, target)
            if binary is None:
                continue
            src_path = target.get("src_path")
            if not isinstance(src_path, str):
                message = f"target {target.get('name')!r} of {name!r} carries no src_path"
                raise Refusal(message)
            required = target.get("required-features") or []
            if not isinstance(required, list) or not all(
                isinstance(item, str) for item in required
            ):
                message = f"target {target.get('name')!r} carries a malformed required-features"
                raise Refusal(message)
            target_gates = tuple(Cfg("feature", value=feature) for feature in required)
            _walk_file(
                Path(src_path),
                prefix="",
                gates=target_gates,
                binary=binary,
                package=name,
                units=units,
                sources=sources,
                root=root,
                visited=set(),
            )
    unique = {(unit.key, unit.features, unit.ignored): unit for unit in units}
    return Snapshot(
        units=sorted(unique.values(), key=lambda unit: unit.key),
        sources=sources,
    )


def split_features(raw: str) -> tuple[tuple[str, ...], frozenset[str]]:
    """Split the `--features` argument into cargo specs and bare feature names."""
    specs = tuple(piece.strip() for piece in raw.split(",") if piece.strip())
    if not specs:
        message = "--features needs at least one <package>/<feature> or <feature> spec"
        raise Refusal(message)
    names = frozenset(spec.split("/", 1)[-1] for spec in specs)
    return specs, names


# --- dispositions ------------------------------------------------------------


@dataclass(frozen=True)
class Disposition:
    """One classified unit: a whole binary (`*`) or one test item."""

    binary: str
    item: str
    label: str
    adr: str
    rationale: str

    @property
    def line(self) -> str:
        """The tabular form used in findings."""
        return f"{self.binary}|{self.item}|{self.label}|{self.adr}"


def load_dispositions(path: Path) -> list[Disposition]:
    """Parse the dispositions table, rejecting malformed rows."""
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        message = f"cannot read dispositions {path}: {error}"
        raise Refusal(message) from error
    rows: list[Disposition] = []
    for number, line in enumerate(lines, start=1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        cells = [cell.strip() for cell in line.split("|")]
        if len(cells) != 5:
            message = f"{path}:{number}: expected binary|item|class|ADR|rationale"
            raise Refusal(message)
        binary, item, label, adr, rationale = cells
        if label not in CLASSES:
            message = f"{path}:{number}: unknown class {label!r}"
            raise Refusal(message)
        if not _ADR.match(adr):
            message = f"{path}:{number}: class {label!r} needs an owning ADR-NNN, got {adr!r}"
            raise Refusal(message)
        rows.append(
            Disposition(binary=binary, item=item, label=label, adr=adr, rationale=rationale)
        )
    return rows


@dataclass
class Lane:
    """The lane's in-scope units, their classes, and the generated filter."""

    units: list[Unit]
    out_of_lane: list[Unit]
    classes: dict[tuple[str, str], str]
    wildcard_binaries: set[str]
    findings: list[str]

    def excluded(self) -> list[Unit]:
        """Every in-scope unit the offline lane must not run."""
        return [unit for unit in self.units if self.classes[unit.key] in _NON_OFFLINE]

    def filter_expression(self) -> str:
        """The nextest `-E` expression selecting exactly this lane's offline set.

        Every term is scoped by the exact binary id: a bare `test(=name)`
        filters the name GLOBALLY, so it would also suppress an offline twin
        with the same name in another binary.
        """
        terms = {f"binary_id(={binary})" for binary in self.wildcard_binaries}
        terms |= {
            f"(binary_id(={unit.binary}) and test(={unit.item}))"
            for unit in self.excluded()
            if unit.binary not in self.wildcard_binaries
        }
        if not terms:
            return "all()"
        ordered = sorted(terms, key=lambda term: (not term.startswith("binary_id("), term))
        return "not (" + " or ".join(ordered) + ")"


def reconcile(
    snapshot: Snapshot,
    features: frozenset[str],
    dispositions: Sequence[Disposition],
) -> Lane:
    """Classify the in-scope units and reconcile them against the dispositions."""
    findings: list[str] = []
    # Every feature-relevant unit is checked BEFORE the enabled filter: a gate
    # that reads an unmodeled predicate can evaluate false and quietly demote a
    # unit to out-of-lane -- the silent exclusion this gate exists to prevent.
    relevant = [unit for unit in snapshot.units if set(unit.features) & features]
    for unit in relevant:
        unmodeled = unit.unmodeled()
        if unmodeled:
            findings.append(
                f"{unit.binary}::{unit.item}: gate depends on an unmodeled predicate "
                f"({', '.join(sorted(unmodeled))})"
            )
    in_scope = [unit for unit in relevant if unit.enabled(features)]
    in_scope_keys = {unit.key for unit in in_scope}
    out_of_lane = [unit for unit in snapshot.units if unit.key not in in_scope_keys]

    items_by_binary: dict[str, set[str]] = {}
    for unit in in_scope:
        items_by_binary.setdefault(unit.binary, set()).add(unit.item)

    wildcards: dict[str, Disposition] = {}
    exact: dict[tuple[str, str], Disposition] = {}
    for row in dispositions:
        if row.item == "*":
            if row.binary not in items_by_binary:
                findings.append(f"disposition names unknown binary: {row.line}")
            elif row.binary in wildcards:
                findings.append(f"duplicate binary disposition: {row.line}")
            else:
                wildcards[row.binary] = row
            continue
        if row.binary not in items_by_binary or row.item not in items_by_binary[row.binary]:
            findings.append(f"disposition names unknown unit: {row.line}")
        elif (row.binary, row.item) in exact:
            findings.append(f"duplicate unit disposition: {row.line}")
        else:
            exact[row.binary, row.item] = row

    classes: dict[tuple[str, str], str] = {}
    for unit in in_scope:
        row = exact.get(unit.key) or wildcards.get(unit.binary)
        label = row.label if row is not None else "offline"
        if unit.ignored and label != "ignored":
            findings.append(
                f"{unit.binary}::{unit.item}: mechanically #[ignore]d but classified {label!r}"
            )
        classes[unit.key] = label

    for binary, items in items_by_binary.items():
        if binary in wildcards:
            continue
        live_gated = any(
            _LIVE_GATE_ENV.search(path.read_text(encoding="utf-8", errors="replace"))
            for path in snapshot.sources.get(binary, set())
        )
        if not live_gated:
            continue
        # A live-gated binary's items must be looked at one by one: defaulting
        # an item to offline because nobody wrote a row is exactly the false
        # green this gate exists to prevent.
        named = {item for (row_binary, item) in exact if row_binary == binary}
        missing = sorted(items - named)
        if missing:
            findings.append(
                f"{binary}: live-gated binary; every item needs an explicit disposition, "
                f"missing: {', '.join(missing)}"
            )
        elif all(exact[binary, item].label == "offline" for item in items):
            findings.append(
                f"{binary}: reads a live env gate but every item is classified offline "
                f"(classify the live probes live or ignored)"
            )

    return Lane(
        units=in_scope,
        out_of_lane=sorted(out_of_lane, key=lambda unit: unit.key),
        classes=classes,
        wildcard_binaries=set(wildcards),
        findings=findings,
    )


# --- nextest capture reconciliation -----------------------------------------


@dataclass(frozen=True)
class Case:
    """One listed test case: whether it is ignored and whether the filter selects it."""

    ignored: bool
    selected: bool


def load_capture(path: Path) -> dict[tuple[str, str], Case]:
    """Read a `cargo nextest list --message-format json` capture."""
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        message = f"cannot read nextest capture {path}: {error}"
        raise Refusal(message) from error
    if not isinstance(payload, dict) or not isinstance(payload.get("rust-suites"), dict):
        message = f"{path}: not a nextest list capture (no rust-suites)"
        raise Refusal(message)
    cases: dict[tuple[str, str], Case] = {}
    for key, suite in payload["rust-suites"].items():
        if not isinstance(suite, dict):
            message = f"{path}: suite {key!r} is not a JSON object"
            raise Refusal(message)
        binary = suite.get("binary-id") or key
        testcases = suite.get("testcases")
        if not isinstance(testcases, dict):
            continue
        for name, case in testcases.items():
            if not isinstance(case, dict):
                message = f"{path}: test case {name!r} is not a JSON object"
                raise Refusal(message)
            match = case.get("filter-match")
            status = match.get("status") if isinstance(match, dict) else None
            cases[binary, name] = Case(
                ignored=bool(case.get("ignored")),
                selected=status != "mismatch",
            )
    return cases


def verify(
    lane: Lane,
    cases: dict[tuple[str, str], Case],
    *,
    filtered: bool,
) -> list[str]:
    """Reconcile one capture against the lane in both directions.

    A `binary(...)` exclusion removes the whole suite from a filtered capture,
    so an excluded unit may be absent there; an item-level `test(=...)`
    exclusion leaves its case listed as a mismatch. An UNFILTERED capture must
    still carry every excluded unit as selected -- that is the negative probe.
    """
    findings: list[str] = []
    for unit in lane.units:
        case = cases.get(unit.key)
        label = lane.classes[unit.key]
        if label in _NON_OFFLINE:
            if filtered and unit.binary in lane.wildcard_binaries:
                if case is not None and case.selected:
                    findings.append(
                        f"excluded unit is selected: {unit.binary}::{unit.item} ({label})"
                    )
                continue
            if case is None:
                findings.append(
                    f"excluded unit is absent from the build: {unit.binary}::{unit.item} ({label})"
                )
            elif case.selected:
                findings.append(f"excluded unit is selected: {unit.binary}::{unit.item} ({label})")
            continue
        if case is None:
            findings.append(f"offline unit is absent from the build: {unit.binary}::{unit.item}")
        elif not case.selected:
            findings.append(f"offline unit is not selected: {unit.binary}::{unit.item}")
    return findings


# --- rendering ---------------------------------------------------------------


def render_inventory(lane: Lane) -> str:
    """Render the lane's units as a tab-separated table."""
    lines = ["class\tbinary\titem\tgate\tignored\tsource"]
    for unit in lane.units:
        lines.append(
            "\t".join((
                lane.classes[unit.key],
                unit.binary,
                unit.item,
                ",".join(unit.features),
                "yes" if unit.ignored else "no",
                unit.source,
            ))
        )
    return "\n".join(lines)


def _report(findings: Sequence[str]) -> int:
    if findings:
        for finding in findings:
            print(f"feature-lane: {finding}", file=sys.stderr)
        print(
            "feature-lane FAILED: reconcile the dispositions, the inventory, and the nextest capture.",
            file=sys.stderr,
        )
        return 1
    return 0


# --- CLI ---------------------------------------------------------------------


def main(argv: Sequence[str] | None = None) -> int:
    """Run one inventory mode."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=str(_REPO_ROOT), help="the repository root")
    parser.add_argument("--metadata", default=None, help="a cargo metadata JSON dump")
    parser.add_argument(
        "--dispositions",
        default=None,
        help=f"the dispositions table (default: <root>/{_DISPOSITIONS})",
    )
    parser.add_argument(
        "--features",
        required=True,
        help="the lane's feature set, comma separated (<package>/<feature> or <feature>)",
    )
    parser.add_argument(
        "mode",
        choices=("check", "inventory", "filter", "verify"),
        nargs="?",
        default="check",
        help="check (default): reconcile tree and dispositions; inventory: print the table; "
        "filter: print the nextest -E expression; verify: reconcile a nextest capture",
    )
    parser.add_argument("--list", default=None, help="the filtered nextest list capture (verify)")
    parser.add_argument(
        "--negative-probe",
        default=None,
        help="an UNFILTERED capture that the same reconciliation must fail (verify)",
    )
    args = parser.parse_args(argv)
    try:
        return _run_mode(args)
    except Refusal as error:
        print(f"feature-lane: refusing to certify: {error}", file=sys.stderr)
        return 2


def _run_mode(args: argparse.Namespace) -> int:
    root = Path(args.root).resolve()
    _, feature_names = split_features(args.features)
    metadata = load_metadata(root, Path(args.metadata) if args.metadata else None)
    snapshot = enumerate_units(root, metadata)
    dispositions = load_dispositions(
        Path(args.dispositions) if args.dispositions else root / _DISPOSITIONS
    )
    lane = reconcile(snapshot, feature_names, dispositions)
    if args.mode == "inventory":
        print(render_inventory(lane))
        print(
            f"# {len(lane.units)} in-lane units, {len(lane.out_of_lane)} out-of-lane "
            f"units (features outside the lane), {len(lane.excluded())} excluded",
            file=sys.stderr,
        )
        return _report(lane.findings)
    if args.mode == "filter":
        print(lane.filter_expression())
        return _report(lane.findings)
    if args.mode == "verify":
        return _verify_mode(args, lane)
    if lane.findings:
        return _report(lane.findings)
    print(
        f"feature-lane: {len(lane.units)} in-lane units reconciled against "
        f"{len(dispositions)} dispositions; "
        f"{len(lane.excluded())} excluded, {len(lane.out_of_lane)} out of lane."
    )
    return 0


def _verify_mode(args: argparse.Namespace, lane: Lane) -> int:
    if args.list is None:
        print("feature-lane: --list is required for verify", file=sys.stderr)
        return 2
    findings = list(lane.findings)
    findings.extend(verify(lane, load_capture(Path(args.list)), filtered=True))
    if args.negative_probe is not None:
        probe = verify(lane, load_capture(Path(args.negative_probe)), filtered=False)
        if probe:
            print(
                f"feature-lane: negative probe failed as required "
                f"({len(probe)} excluded unit(s) would run unfiltered; "
                f"first: {probe[0]})"
            )
        else:
            findings.append(
                "negative probe: the unfiltered capture also reconciles clean -- "
                "the filter is not what excludes the non-offline units"
            )
    return _report(findings)


if __name__ == "__main__":
    sys.exit(main())
