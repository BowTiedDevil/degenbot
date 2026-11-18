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

Adding a gate is one `[[gate]]` block plus `regen`; retiring one is deleting the
block plus `regen`. The `gate-registry` hook fails a commit whose `prek.toml`
does not match the registry, so the two cannot disagree in silence.
"""

from __future__ import annotations

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

# The stages in the order `prek.toml` presents them. Each stage's hooks are
# contiguous, so the file reads as three sections and a hook's insertion point
# is never ambiguous.
STAGE_ORDER = ("pre-commit", "commit-msg", "pre-push")

# prek's trigger keys, emitted in a fixed order so a regenerated line is stable.
TRIGGERS = ("types", "types_or", "files", "exclude")
FLAGS = ("always_run", "fail_fast", "skip_children")

HOOK_LINE = re.compile(r'^  \{ id = "(?P<id>[^"]+)"')


class RegistryError(Exception):
    """A declaration that cannot be rendered or that contradicts the tree."""


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
            )
        )
    _validate(gates, root)
    return gates


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


def render_prek(gates: list[Gate], root: Path = ROOT) -> str:
    """Return `prek.toml` with the hook lines replaced by the registry's.

    Everything that is not a hook line survives verbatim: the file header, the
    prose block above each hook, the stage separators and the closing bracket.
    A gate keeps the comment block that precedes its line, so prose and line move
    together when the registry reorders them.
    """
    lines = (root / PREK_PATH).read_text(encoding="utf-8").split("\n")
    try:
        start = next(i for i, line in enumerate(lines) if line.strip() == "hooks = [")
        end = max(i for i, line in enumerate(lines) if line.strip() == "]")
    except StopIteration as exc:
        raise RegistryError(f"{PREK_PATH}: cannot find the hooks array") from exc
    if end < start:
        raise RegistryError(f"{PREK_PATH}: the hooks array is not closed after it opens")

    segments: dict[str, list[str]] = {}
    order: list[str] = []
    pending: list[str] = []
    tail: list[str] = []
    for line in lines[start + 1 : end]:
        match = HOOK_LINE.match(line)
        if match:
            segments[match.group("id")] = pending
            order.append(match.group("id"))
            pending = []
        else:
            pending.append(line)
    tail = pending

    declared = {gate.id for gate in gates}
    stale = [hook_id for hook_id in order if hook_id not in declared]
    if stale:
        raise RegistryError(
            f"{PREK_PATH}: hook(s) absent from {REGISTRY_PATH}: {stale} -- "
            "add the declaration or run `just gates-regen` to retire them"
        )

    body: list[str] = []
    for gate in gates:
        body.extend(segments.get(gate.id, ["\n"]))
        body.append(gate.render())
    body.extend(tail)

    return "\n".join([*lines[: start + 1], *body, *lines[end:]])


def main(argv: list[str] | None = None) -> int:
    command = (argv if argv is not None else sys.argv[1:]) or ["check"]
    if len(command) != 1 or command[0] not in {"check", "regen"}:
        print("usage: gate_registry.py [check|regen]", file=sys.stderr)
        return 2
    try:
        gates = load()
    except RegistryError as exc:
        print(f"gate-registry: {exc}", file=sys.stderr)
        print("fix: correct the declaration in scripts/gates.toml", file=sys.stderr)
        return 1
    except just_recipe_index.RecipeIndexRefusal as exc:
        print(f"gate-registry: {exc}", file=sys.stderr)
        print("fix: install `just` and make the justfile parse, then re-run", file=sys.stderr)
        return 2
    rendered = render_prek(gates)
    current = (ROOT / PREK_PATH).read_text(encoding="utf-8")
    if command[0] == "regen":
        if rendered == current:
            print(f"gate-registry: {PREK_PATH} already matches {REGISTRY_PATH}")
            return 0
        (ROOT / PREK_PATH).write_text(rendered, encoding="utf-8")
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
