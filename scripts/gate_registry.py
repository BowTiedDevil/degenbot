"""The gate registry (ADR-102).

`scripts/gates.toml` declares each hook's facts: its id, the stage it runs at,
the command it runs, its file trigger and its flags. This module validates the
declarations and renders the hook lines of `prek.toml`, which stays the file git
actually loads. The prose above each hook line is hand-written and preserved
verbatim, so a rationale keeps its home next to the gate it explains.

    python3 scripts/gate_registry.py check    # drift gate; exit 1 on divergence
    python3 scripts/gate_registry.py regen    # rewrite the hook lines

An entry naming `just <recipe>` is validated by asking `just` itself
(scripts/just_recipe_index.py), never by reading justfile text: a missing `just`
or a justfile that will not parse refuses validation with exit 2.

A declaration is more than its wiring line. `check`/regen both validate the
gate's `scope` (against `pass_filenames`), every `data` path, its `docs`
heading and its `probe` node ids (by import-free `ast`), and exactly one CI
coverage form: `direct` must name the entry verbatim in the declared job,
`contains` must be reachable through the `just --dump` invocation graph,
`equivalent` must name the probe that pins the parallel implementation, and a
`ci_probe` must name a committed-tree test function. Presence in `ci.yml` alone
is never sufficient, and pytest is never collected or run here.

Prose belongs to the hook line that follows it: a block of comments explains the
gate below, including when the registry reorders or retires it. A stage's
`# --- <stage> ---` separator and the contiguous comment block under it are the
stage's own anchor, owned by no hook, so retiring the first hook of a stage never
deletes the stage header or its intro.

`check` and `regen` differ on exactly one case, and on purpose: a hook line the
registry does not declare is drift `check` refuses and `regen` retires (line and
prose together), which is what makes deleting a `[[gate]]` block the complete
retirement (ADR-087). Prose that no hook and no stage anchor owns is ambiguous in
either direction, so both commands fail on it rather than guess.

Adding a gate is one `[[gate]]` block plus `regen`; retiring one is deleting the
block plus `regen`. The `gate-registry` hook fails a commit whose `prek.toml`
does not match the registry, so the two cannot disagree in silence.
"""

from __future__ import annotations

import ast
import enum
import re
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import just_recipe_index

ROOT = Path(__file__).resolve().parents[1]
REGISTRY_PATH = "scripts/gates.toml"
PREK_PATH = "prek.toml"
JUSTFILE_PATH = "justfile"
CI_PATH = ".github/workflows/ci.yml"
DOCS_PATH = "docs/gates.md"

# What population a gate judges. `index` is the staged file list prek passes
# (so `pass_filenames` cannot be false); `staged` is the commit's own content
# the gate derives for itself; `tree` is any wider population -- the working
# tree, the workspace, or the outgoing push range.
GATE_SCOPES = ("index", "staged", "tree")

# How a hook's CI execution relates to it. `direct` runs the entry verbatim,
# `contains` reaches the hook's recipe through the `just` invocation graph,
# `equivalent` runs a parallel implementation pinned by a named probe, and a
# `ci_probe` names a committed-tree pytest node that executes in CI.
GATE_RELATIONS = ("direct", "contains", "equivalent")

# The stages in the order `prek.toml` presents them. Each stage's hooks are
# contiguous, so the file reads as three sections and a hook's insertion point
# is never ambiguous.
STAGE_ORDER = ("pre-commit", "commit-msg", "pre-push")

# prek's trigger keys, emitted in a fixed order so a regenerated line is stable.
TRIGGERS = ("types", "types_or", "files", "exclude")
FLAGS = ("always_run", "fail_fast", "skip_children")

HOOK_LINE = re.compile(r'^  \{ id = "(?P<id>[^"]+)"')

# A stage's separator plus the comment block under it: hand-written rationale for
# the stage itself, owned by no hook line.
STAGE_ANCHOR = re.compile(r"^\s*# --- (?P<stage>" + "|".join(STAGE_ORDER) + r") ---")


class RegistryError(Exception):
    """A declaration that cannot be rendered or that contradicts the tree."""


@dataclass(frozen=True)
class Coverage:
    """The one CI execution form a hook declares (ADR-102)."""

    job: str
    entry: str | None = None
    relation: str | None = None
    equivalent_probe: str | None = None
    probe: str | None = None


class UndeclaredHook(enum.Enum):
    """How a hook line the registry does not declare is treated.

    The policy is the command: `check` refuses the line as drift, `regen` retires
    it with the prose it owned (the declared way to delete a gate).
    """

    CHECK = "check"
    REGEN = "regen"


@dataclass
class Gate:
    id: str
    name: str
    stage: str
    entry: str
    triggers: dict[str, Any] = field(default_factory=dict)
    always_run: bool = False
    fail_fast: bool = False
    skip_children: bool = False
    pass_filenames: bool = True
    scope: str = "tree"
    data: tuple[str, ...] = ()
    docs: str = ""
    probe: tuple[str, ...] = ()
    coverage: Coverage | None = None

    def render(self) -> str:
        """The single-line inline table prek loads."""
        parts = [
            f'id = "{self.id}"',
            f'name = "{self.name}"',
            'language = "system"',
            f'entry = "{self.entry}"',
        ]
        for key in TRIGGERS:
            if key in self.triggers:
                parts.append(f"{key} = {_toml_value(self.triggers[key])}")
        parts.append(f'stages = ["{self.stage}"]')
        if self.always_run:
            parts.append("always_run = true")
        if not self.pass_filenames:
            parts.append("pass_filenames = false")
        if self.fail_fast:
            parts.append("fail_fast = true")
        if self.skip_children:
            parts.append("skip_children = true")
        return "  { " + ", ".join(parts) + " },"


def _toml_value(value: object) -> str:
    """A TOML scalar or array, preferring the literal quoting regexes want."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        # Literal quoting keeps a regex readable: no doubled backslashes. Only a
        # value carrying a backslash needs it, and only when it can be literal
        # (no single quote, no newline); everything else takes a basic string.
        if "\\" in value and "'" not in value and "\n" not in value:
            return "'" + value + "'"
        escaped = value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
        return '"' + escaped + '"'
    if isinstance(value, list):
        return "[" + ", ".join(_toml_value(item) for item in value) + "]"
    raise RegistryError(f"unsupported registry value: {value!r}")


def load(root: Path = ROOT) -> list[Gate]:
    """Parse `scripts/gates.toml` into declarations, rejecting bad shapes."""
    path = root / REGISTRY_PATH
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    unknown = set(data) - {"version", "gate"}
    if unknown:
        raise RegistryError(f"{REGISTRY_PATH}: unknown top-level key(s): {sorted(unknown)}")
    if data.get("version") != 1:
        raise RegistryError(f"{REGISTRY_PATH}: expected version = 1")
    gates: list[Gate] = []
    for raw in data.get("gate", []):
        allowed = {
            "id",
            "name",
            "stage",
            "entry",
            "types",
            "types_or",
            "files",
            "exclude",
            "always_run",
            "fail_fast",
            "skip_children",
            "pass_filenames",
            "scope",
            "data",
            "docs",
            "probe",
            "ci_job",
            "ci_entry",
            "ci_relation",
            "ci_equivalent_probe",
            "ci_probe",
        }
        extra = set(raw) - allowed
        if extra:
            raise RegistryError(
                f"{REGISTRY_PATH}: {raw.get('id')}: unknown key(s): {sorted(extra)}"
            )
        for required in ("id", "name", "stage", "entry"):
            if not raw.get(required):
                raise RegistryError(f"{REGISTRY_PATH}: a gate is missing {required!r}: {raw}")
        if raw["stage"] not in STAGE_ORDER:
            raise RegistryError(
                f"{REGISTRY_PATH}: {raw['id']}: stage {raw['stage']!r} is not one of {STAGE_ORDER}"
            )
        triggers = {key: raw[key] for key in TRIGGERS if key in raw}
        gates.append(
            Gate(
                id=raw["id"],
                name=raw["name"],
                stage=raw["stage"],
                entry=raw["entry"],
                triggers=triggers,
                always_run=bool(raw.get("always_run", False)),
                fail_fast=bool(raw.get("fail_fast", False)),
                skip_children=bool(raw.get("skip_children", False)),
                pass_filenames=bool(raw.get("pass_filenames", True)),
                scope=raw.get("scope", ""),
                data=tuple(raw.get("data", ())),
                docs=raw.get("docs", ""),
                probe=tuple(raw.get("probe", ())),
                coverage=_coverage(raw),
            )
        )
    _validate(gates, root)
    return gates


def _coverage(raw: dict[str, Any]) -> Coverage:
    """The gate's one CI coverage form, rejecting a missing or doubled one."""
    job = raw.get("ci_job")
    probe = raw.get("ci_probe")
    entry = raw.get("ci_entry")
    relation = raw.get("ci_relation")
    equivalent_probe = raw.get("ci_equivalent_probe")
    probe_form = probe is not None
    entry_form = any(value is not None for value in (entry, relation, equivalent_probe))
    if probe_form == entry_form:
        raise RegistryError(
            f"{REGISTRY_PATH}: {raw.get('id')}: declare exactly one CI coverage form -- "
            "ci_job + ci_entry + ci_relation [+ ci_equivalent_probe], or ci_job + ci_probe"
        )
    if not job:
        raise RegistryError(f"{REGISTRY_PATH}: {raw.get('id')}: CI coverage needs ci_job")
    if probe_form:
        return Coverage(job=job, probe=probe)
    if not entry or not relation:
        raise RegistryError(
            f"{REGISTRY_PATH}: {raw.get('id')}: CI coverage needs ci_entry and ci_relation"
        )
    return Coverage(job=job, entry=entry, relation=relation, equivalent_probe=equivalent_probe)


def _validate(gates: list[Gate], root: Path) -> None:
    """Reject declarations the tree cannot satisfy, before anything renders."""
    seen: set[str] = set()
    for gate in gates:
        if gate.id in seen:
            raise RegistryError(f"{REGISTRY_PATH}: duplicate id {gate.id!r}")
        seen.add(gate.id)
        if "\n" in gate.render():
            raise RegistryError(f"{REGISTRY_PATH}: {gate.id}: the hook line must stay on one line")
    stages = [gate.stage for gate in gates]
    order = [STAGE_ORDER.index(stage) for stage in stages]
    if order != sorted(order):
        raise RegistryError(
            f"{REGISTRY_PATH}: gates must be grouped by stage in {STAGE_ORDER} order"
        )
    index = just_recipe_index.RecipeIndex(root / JUSTFILE_PATH)
    for gate in gates:
        if gate.entry.startswith("just "):
            recipe = gate.entry.split()[1]
            if not index.exists(recipe):
                raise RegistryError(
                    f"{REGISTRY_PATH}: {gate.id}: entry names `just {recipe}`, "
                    f"which {JUSTFILE_PATH} does not define"
                )
        elif gate.entry.startswith(("npx ", "uv ", "cargo ")):
            continue
        elif not (root / gate.entry.split()[0]).exists():
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: entry {gate.entry!r} is not a recipe "
                f"and its script does not exist"
            )
    _validate_identity(gates, root)
    _validate_coverage(gates, root, index)


def _validate_identity(gates: list[Gate], root: Path) -> None:
    """Reject a gate whose scope, data, docs or probes the tree cannot satisfy."""
    headings = _headings(root / DOCS_PATH)
    for gate in gates:
        if gate.scope not in GATE_SCOPES:
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: scope {gate.scope!r} is not one of {GATE_SCOPES}"
            )
        if (gate.scope == "index") != gate.pass_filenames:
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: scope {gate.scope!r} disagrees with "
                f"pass_filenames = {str(gate.pass_filenames).lower()} -- an index gate is "
                "handed the staged file list, a staged or tree gate sweeps for itself"
            )
        for path in gate.data:
            if not (root / path).exists():
                raise RegistryError(
                    f"{REGISTRY_PATH}: {gate.id}: data path {path!r} does not exist"
                )
        if gate.docs not in headings:
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: docs {gate.docs!r} is no heading in {DOCS_PATH}"
            )
        if not gate.probe:
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: declares no probe -- a gate lands with the "
                "test that keeps it honest (ADR-102 D4)"
            )
        for probe in gate.probe:
            _assert_test_named(root, probe, f"{gate.id}: probe")


def _validate_coverage(gates: list[Gate], root: Path, index: just_recipe_index.RecipeIndex) -> None:
    """Reject a CI claim the workflow and the invocation graph do not support."""
    jobs = _ci_jobs(root)
    for gate in gates:
        coverage = gate.coverage
        if coverage is None:
            raise RegistryError(f"{REGISTRY_PATH}: {gate.id}: declares no CI coverage")
        if coverage.job not in jobs:
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: ci_job {coverage.job!r} is no job in {CI_PATH}"
            )
        if coverage.probe is not None:
            _assert_test_named(root, coverage.probe, f"{gate.id}: ci_probe")
            continue
        if not _job_runs(jobs[coverage.job], coverage.entry):
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: ci_job {coverage.job!r} never runs "
                f"ci_entry {coverage.entry!r}"
            )
        if coverage.relation not in GATE_RELATIONS:
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: ci_relation {coverage.relation!r} is not one "
                f"of {GATE_RELATIONS}"
            )
        if coverage.relation == "direct":
            if coverage.entry != gate.entry:
                raise RegistryError(
                    f"{REGISTRY_PATH}: {gate.id}: direct coverage must name the hook entry "
                    f"{gate.entry!r} verbatim, not {coverage.entry!r}"
                )
            continue
        if coverage.equivalent_probe is not None and coverage.relation != "equivalent":
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: ci_equivalent_probe belongs to equivalent "
                f"coverage, not {coverage.relation!r}"
            )
        if coverage.relation == "equivalent":
            if coverage.equivalent_probe is None:
                raise RegistryError(
                    f"{REGISTRY_PATH}: {gate.id}: equivalent coverage needs "
                    "ci_equivalent_probe naming the test that pins the two implementations"
                )
            _assert_test_named(root, coverage.equivalent_probe, f"{gate.id}: ci_equivalent_probe")
            continue
        start_recipe, _start_arguments = _recipe_entry(coverage.entry)
        target_recipe, target_arguments = _recipe_entry(gate.entry)
        if start_recipe is None or target_recipe is None:
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: contains coverage needs both entries to be "
                "`just <recipe>` invocations"
            )
        if not index.reaches(start_recipe, target_recipe, arguments=target_arguments):
            raise RegistryError(
                f"{REGISTRY_PATH}: {gate.id}: ci_entry {coverage.entry!r} does not reach "
                f"{gate.entry!r} through {JUSTFILE_PATH} dependencies and body calls"
            )


def _recipe_entry(entry: str | None) -> tuple[str | None, str]:
    """Split `just <recipe> [arguments]` into its recipe and its arguments."""
    words = (entry or "").split()
    if len(words) < 2 or words[0] != "just":
        return None, ""
    return words[1], " ".join(words[2:])


def _headings(path: Path) -> set[str]:
    """The text of every Markdown heading in `path`."""
    if not path.is_file():
        raise RegistryError(f"{DOCS_PATH}: does not exist")
    headings = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("#"):
            headings.add(line.lstrip("#").strip())
    return headings


_CI_JOB = re.compile(r"^  (?P<id>[A-Za-z0-9_-]+):$")
_NODE_ID = re.compile(r"^(?P<path>[^:]+\.py)::(?P<test>[A-Za-z_][A-Za-z0-9_]*)$")


def _ci_jobs(root: Path) -> dict[str, str]:
    """Each `jobs:` entry of the workflow, keyed by job id, as its block text."""
    path = root / CI_PATH
    if not path.is_file():
        raise RegistryError(f"{CI_PATH}: does not exist")
    jobs: dict[str, list[str]] = {}
    body: list[str] | None = None
    in_jobs = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if line == "jobs:":
            in_jobs = True
            continue
        if not in_jobs:
            continue
        if line and not line.startswith(" "):
            in_jobs = False
            body = None
            continue
        match = _CI_JOB.match(line)
        if match is not None:
            body = jobs.setdefault(match.group("id"), [])
            continue
        if body is not None:
            body.append(line)
    return {job: "\n".join(lines) for job, lines in jobs.items()}


def _job_runs(block: str, entry: str | None) -> bool:
    """Does the job block run `entry` as a command, not merely mention it?"""
    if not entry:
        return False
    pattern = re.compile(r"(?<![\w./-])" + re.escape(entry) + r"(?![\w/-])")
    return any(
        pattern.search(line) for line in block.splitlines() if not line.strip().startswith("#")
    )


def _assert_test_named(root: Path, node_id: str, label: str) -> None:
    """`node_id` must name a test function the committed tree defines."""
    match = _NODE_ID.match(node_id)
    if match is None:
        raise RegistryError(
            f"{REGISTRY_PATH}: {label} {node_id!r} is not a pytest node id (path.py::test_name)"
        )
    path = root / match.group("path")
    if not path.is_file():
        raise RegistryError(f"{REGISTRY_PATH}: {label} names {path}, which does not exist")
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except SyntaxError as error:
        raise RegistryError(
            f"{REGISTRY_PATH}: {label} names a file that will not parse: {error}"
        ) from error
    defined = {
        node.name
        for node in ast.walk(tree)
        if isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef)
    }
    if match.group("test") not in defined:
        raise RegistryError(
            f"{REGISTRY_PATH}: {label} {node_id!r} names no test function "
            f"{match.group('test')!r} in {path}"
        )


@dataclass
class HookSection:
    """`prek.toml`'s hooks array, split into what the registry can own."""

    anchors: dict[str, list[str]]
    prose: dict[str, list[str]]
    order: list[str]
    tail: list[str]


def _take_anchors(lines: list[str], anchors: dict[str, list[str]]) -> list[str]:
    """Split `lines` into stage anchors and the prose a hook line owns."""
    owned: list[str] = []
    index = 0
    while index < len(lines):
        match = STAGE_ANCHOR.match(lines[index])
        if match is None:
            owned.append(lines[index])
            index += 1
            continue
        block: list[str] = []
        while owned and not owned[-1].strip():
            block.insert(0, owned.pop())
        block.append(lines[index])
        index += 1
        while (
            index < len(lines)
            and _is_comment(lines[index])
            and not STAGE_ANCHOR.match(lines[index])
        ):
            block.append(lines[index])
            index += 1
        stage = match.group("stage")
        if stage in anchors:
            raise RegistryError(f"{PREK_PATH}: more than one `# --- {stage} ---` stage anchor")
        anchors[stage] = block
    return owned


def _is_comment(line: str) -> bool:
    return line.strip().startswith("#")


def _split_hook_section(lines: list[str], start: int, end: int) -> HookSection:
    anchors: dict[str, list[str]] = {}
    prose: dict[str, list[str]] = {}
    order: list[str] = []
    pending: list[str] = []
    for line in lines[start + 1 : end]:
        match = HOOK_LINE.match(line)
        if match is None:
            pending.append(line)
            continue
        prose[match.group("id")] = _take_anchors(pending, anchors)
        order.append(match.group("id"))
        pending = []
    tail = _take_anchors(pending, anchors)
    return HookSection(anchors=anchors, prose=prose, order=order, tail=tail)


def render_prek(
    gates: list[Gate],
    root: Path = ROOT,
    *,
    undeclared: UndeclaredHook = UndeclaredHook.CHECK,
) -> str:
    """Return `prek.toml` with the hook lines replaced by the registry's.

    Everything that is not a hook line survives verbatim: the file header, the
    stage anchors, the prose block above each hook and the closing bracket. A
    gate keeps the comment block that precedes its line, so prose and line move
    together when the registry reorders them; a stage keeps the header and intro
    above its first hook, so retiring that hook leaves them in place.
    """
    lines = (root / PREK_PATH).read_text(encoding="utf-8").split("\n")
    try:
        start = next(i for i, line in enumerate(lines) if line.strip() == "hooks = [")
        end = max(i for i, line in enumerate(lines) if line.strip() == "]")
    except StopIteration as exc:
        raise RegistryError(f"{PREK_PATH}: cannot find the hooks array") from exc
    if end < start:
        raise RegistryError(f"{PREK_PATH}: the hooks array is not closed after it opens")

    section = _split_hook_section(lines, start, end)
    declared = {gate.id for gate in gates}
    stale = [hook_id for hook_id in section.order if hook_id not in declared]
    if stale and undeclared is UndeclaredHook.CHECK:
        raise RegistryError(
            f"{PREK_PATH}: hook(s) absent from {REGISTRY_PATH}: {stale} -- "
            "declare them, or run `just gates-regen` to retire the lines and their prose"
        )

    gate_stages = {gate.stage for gate in gates}
    orphan_anchors = [stage for stage in section.anchors if stage not in gate_stages]
    if orphan_anchors:
        raise RegistryError(
            f"{PREK_PATH}: the `# --- {orphan_anchors[0]} ---` stage anchor introduces no "
            "declared gate -- declare one for the stage, or delete its intro by hand"
        )
    orphan_tail = [line for line in section.tail if _is_comment(line)]
    if orphan_tail:
        raise RegistryError(
            f"{PREK_PATH}: prose after the last hook line explains no gate: "
            f"{orphan_tail[0].strip()!r}"
        )

    body: list[str] = []
    stage = ""
    for gate in gates:
        if gate.stage != stage:
            stage = gate.stage
            body.extend(section.anchors.get(stage, []))
        body.extend(section.prose.get(gate.id, ["\n"]))
        body.append(gate.render())
    body.extend(section.tail)

    return "\n".join([*lines[: start + 1], *body, *lines[end:]])


def main(argv: list[str] | None = None, root: Path = ROOT) -> int:
    command = (argv if argv is not None else sys.argv[1:]) or ["check"]
    if len(command) != 1 or command[0] not in {"check", "regen"}:
        print("usage: gate_registry.py [check|regen]", file=sys.stderr)
        return 2
    try:
        gates = load(root)
    except RegistryError as exc:
        print(f"gate-registry: {exc}", file=sys.stderr)
        print("fix: correct the declaration in scripts/gates.toml", file=sys.stderr)
        return 1
    except just_recipe_index.RecipeIndexRefusal as exc:
        print(f"gate-registry: {exc}", file=sys.stderr)
        print("fix: install `just` and make the justfile parse, then re-run", file=sys.stderr)
        return 2
    try:
        rendered = render_prek(gates, root, undeclared=UndeclaredHook(command[0]))
    except RegistryError as exc:
        print(f"gate-registry: {exc}", file=sys.stderr)
        return 1
    current = (root / PREK_PATH).read_text(encoding="utf-8")
    if command[0] == "regen":
        if rendered == current:
            print(f"gate-registry: {PREK_PATH} already matches {REGISTRY_PATH}")
            return 0
        (root / PREK_PATH).write_text(rendered, encoding="utf-8")
        print(f"gate-registry: rewrote the hook lines of {PREK_PATH}")
        return 0
    if rendered == current:
        print(f"gate-registry: {PREK_PATH} matches {REGISTRY_PATH} ({len(gates)} gates)")
        return 0
    import difflib

    print(f"gate-registry: {PREK_PATH} does not match {REGISTRY_PATH}", file=sys.stderr)
    diff = difflib.unified_diff(
        current.split("\n"), rendered.split("\n"), PREK_PATH, f"{PREK_PATH} (registry)", lineterm=""
    )
    print("\n".join(diff), file=sys.stderr)
    print("fix: just gates-regen", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
