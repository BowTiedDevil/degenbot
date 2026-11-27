#!/usr/bin/env python3
"""The justfile recipe index (GLOSSARY: "Justfile recipe index").

`just` is the authority on whether a name resolves to an invocable recipe.
This module asks it -- per name, `just --show <name>` -- instead of parsing the
justfile with a second text grammar: private and parameterized recipes resolve,
variables and directives do not. `just --summary` is NOT the existence oracle
(it omits private recipes); it runs first only to validate that the justfile
parses, so an unknown name is only ever reported from a well-formed justfile.
A missing `just` executable or a justfile that will not parse refuses the
answer outright.

    python3 scripts/just_recipe_index.py --justfile justfile NAME

    exit 0   NAME resolves to an invocable recipe
    exit 1   NAME is unknown in a well-formed justfile
    exit 2   refusal: no `just` executable, or a justfile that will not parse

The adapter is silent on exists and unknown: the missing-recipe finding is the
caller's to word and print. Standard library only; no uv environment required.
Parse validation and name lookups are cached within each process.

The invocation graph asks the same `just` for `--dump --dump-format json`, so
a recipe's declared dependencies and the `just <recipe>` calls in its body come
from just's own parse, never from a second reading of justfile text. A prologue
string, a comment, or a recipe named only in prose is not a call; a call under
an `if`/`then`/`&&` chain is. `reaches()` answers whether one recipe's
execution can reach another's, which is what pairwise entry wiring is checked
against instead of an ordering of hand-kept lists.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterable, Sequence
    from typing import Any

EXIT_EXISTS = 0
EXIT_UNKNOWN = 1
EXIT_REFUSAL = 2

# A recipe name as just spells one: an identifier of letters, digits, `_` and
# `-` that cannot start with a digit. Anything else can never be a recipe name,
# so it is answered "unknown" without being handed to `just`, where a name
# shaped like `--version` would be read as a flag rather than a question.
_RECIPE_NAME = re.compile(r"^[a-zA-Z_][a-zA-Z0-9_-]*$")

# One command in a recipe line: a command starts at the beginning of the line or
# after a shell separator, so `echo "run just build"` is a word, not a call.
_COMMAND_SEPARATORS = re.compile(r"&&|\|\||[;|]|\$\(|\x60|[{}]")
_JUST_CALL = re.compile(
    r'^just\s+(?P<quote>["\']?)(?P<recipe>[A-Za-z_][A-Za-z0-9_-]*)(?P=quote)(?P<arguments>.*)$'
)
_COMMAND_PREFIXES = ("then", "else", "do", "while", "until", "if", "!")


@dataclass(frozen=True)
class Invocation:
    """One `just <recipe> [arguments]` call a recipe's execution makes."""

    recipe: str
    arguments: str
    text: str


class RecipeIndexRefusal(Exception):
    """The index cannot answer: `just` is missing or the justfile will not parse."""


class RecipeIndex:
    """Recipe-existence answers for one justfile, cached within this process."""

    def __init__(self, justfile: Path, *, just: str = "just") -> None:
        """Target `justfile`, answered by the `just` executable.

        Args:
            justfile: The justfile to index.
            just: The `just` executable to ask.

        """
        self._justfile = justfile
        self._just = just
        self._names: tuple[str, ...] | None = None
        self._lookups: dict[str, bool] = {}
        self._dump: dict[str, Any] | None = None
        self._invocations: dict[str, tuple[Invocation, ...]] = {}

    def names(self) -> tuple[str, ...]:
        """The names the parse-validation pass lists.

        `just --summary` reports public recipes only, so this is never an
        existence oracle -- `exists()` is. A caller reads it only to notice a
        justfile that yields no recipe names at all.

        Returns:
            The public recipe names the justfile defines.

        Raises:
            RecipeIndexRefusal: `just` cannot run, or the justfile will not parse.

        """
        if self._names is None:
            proc = self._just_run("--summary")
            if proc.returncode != 0:
                msg = f"refusing to index {self._justfile}: {_reason(proc)}"
                raise RecipeIndexRefusal(msg)
            self._names = tuple(proc.stdout.split())
        return self._names

    def exists(self, name: str) -> bool:
        """Does `name` resolve to an invocable recipe? Private recipes count.

        Args:
            name: One recipe name.

        Returns:
            True when `just` resolves the name; False when it is unknown in
            this (validated) justfile.

        Raises:
            RecipeIndexRefusal: `just` cannot run, the justfile will not parse,
                or `just` failed to answer.

        """
        if name not in self._lookups:
            self.names()  # validate the justfile before reading a miss as unknown
            if _RECIPE_NAME.match(name) is None:
                self._lookups[name] = False
                return self._lookups[name]
            proc = self._just_run("--show", name)
            if proc.returncode == 0:
                self._lookups[name] = True
            elif proc.returncode == 1:
                self._lookups[name] = False
            else:
                msg = f"refusing to answer for {name!r}: {_reason(proc)}"
                raise RecipeIndexRefusal(msg)
        return self._lookups[name]

    def dump(self) -> dict[str, Any]:
        """The justfile's own parse, as `just --dump --dump-format json`.

        Returns:
            The parsed dump, with recipes, dependencies, bodies and assignments.

        Raises:
            RecipeIndexRefusal: `just` cannot run, the justfile will not parse,
                or the dump is not JSON.

        """
        if self._dump is None:
            proc = self._just_run("--dump", "--dump-format", "json")
            if proc.returncode != 0:
                msg = f"refusing to index {self._justfile}: {_reason(proc)}"
                raise RecipeIndexRefusal(msg)
            try:
                self._dump = json.loads(proc.stdout)
            except json.JSONDecodeError as error:
                msg = f"refusing to index {self._justfile}: {error}"
                raise RecipeIndexRefusal(msg) from error
        return self._dump

    def invocations(self, recipe: str) -> tuple[Invocation, ...]:
        """The `just` calls executing `recipe` makes, declared or in its body.

        Args:
            recipe: The recipe whose execution is followed.

        Returns:
            One invocation per declared dependency and per `just <recipe>`
            command in the body; an unknown recipe has none.

        """
        if recipe not in self._invocations:
            self._invocations[recipe] = self._extract_invocations(recipe)
        return self._invocations[recipe]

    def body(self, recipe: str) -> tuple[str, ...]:
        """One recipe's rendered body lines, comments included.

        Args:
            recipe: The recipe whose body is read.

        Returns:
            Each body line with its `{{variable}}` splices rendered; an unknown
            recipe has none.

        """
        entry = self.dump().get("recipes", {}).get(recipe)
        if entry is None:
            return ()
        return tuple(
            _render(line if isinstance(line, list) else [line]) for line in entry.get("body") or []
        )

    def reaches(self, start: str, target: str, *, arguments: str = "") -> bool:
        """Can `start`'s execution reach an invocation of `target`?

        Args:
            start: The recipe an entry the tree claims to run begins with.
            target: The recipe that entry must reach.
            arguments: When non-empty, the reaching invocation must carry
                exactly these arguments (`just gen-stubs --check` is not
                `just gen-stubs`).

        Returns:
            True when a chain of dependencies and body calls arrives at
            `target`, carrying `arguments` when they were named.

        """
        wanted = _normalize_arguments(arguments)
        pending = [(start, not wanted)]
        seen = set(pending)
        while pending:
            recipe, matched = pending.pop()
            if recipe == target and matched:
                return True
            for invocation in self.invocations(recipe):
                satisfied = matched or (
                    invocation.recipe == target
                    and _normalize_arguments(invocation.arguments) == wanted
                )
                state = (invocation.recipe, satisfied)
                if state not in seen:
                    seen.add(state)
                    pending.append(state)
        return False

    def _extract_invocations(self, recipe: str) -> tuple[Invocation, ...]:
        """Collect one recipe's declared and body-level calls."""
        entry = self.dump().get("recipes", {}).get(recipe)
        if entry is None:
            return ()
        found: list[Invocation] = []
        for dependency in entry.get("dependencies") or []:
            arguments = _normalize_arguments(_render(dependency.get("arguments") or []))
            found.append(_invocation(dependency["recipe"], arguments))
        for line in self.body(recipe):
            found.extend(_body_invocations(line))
        return tuple(found)

    def _just_run(self, *args: str) -> subprocess.CompletedProcess[str]:
        """Run `just` against the indexed justfile.

        Args:
            *args: The `just` arguments, after the `--justfile` selector.

        Returns:
            The completed process.

        Raises:
            RecipeIndexRefusal: The `just` executable cannot be run at all.

        """
        try:
            return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
                [self._just, "--justfile", str(self._justfile), *args],
                check=False,
                capture_output=True,
                text=True,
            )
        except OSError as error:
            msg = f"refusing to answer: `just` cannot run: {error}"
            raise RecipeIndexRefusal(msg) from error


def _invocation(recipe: str, arguments: str) -> Invocation:
    """One invocation, with its canonical text."""
    text = f"just {recipe}" + (f" {arguments}" if arguments else "")
    return Invocation(recipe=recipe, arguments=arguments, text=text)


def _normalize_arguments(arguments: str) -> str:
    """Collapse whitespace so `--check` and `  --check ` agree."""
    return " ".join(arguments.split())


def _render(fragments: Iterable[Any]) -> str:
    """Render a dump fragment list: text, a variable splice, or a nested list."""
    parts: list[str] = []
    for fragment in fragments:
        if isinstance(fragment, str):
            parts.append(fragment)
        elif isinstance(fragment, list):
            if len(fragment) == 2 and fragment[0] == "variable" and isinstance(fragment[1], str):
                parts.append("{{" + fragment[1] + "}}")
            else:
                parts.append(_render(fragment))
    return "".join(parts)


def _body_invocations(line: str) -> tuple[Invocation, ...]:
    """The `just` calls one rendered body line runs."""
    stripped = line.strip()
    if not stripped or stripped.startswith("#"):
        return ()
    calls: list[Invocation] = []
    for segment in _COMMAND_SEPARATORS.split(stripped):
        command = segment.strip()
        for prefix in _COMMAND_PREFIXES:
            if command.startswith(prefix + " "):
                command = command[len(prefix) + 1 :].lstrip()
        match = _JUST_CALL.match(command)
        if match is not None:
            calls.append(
                _invocation(match.group("recipe"), _normalize_arguments(match.group("arguments")))
            )
    return tuple(calls)


def _reason(proc: subprocess.CompletedProcess[str]) -> str:
    """Summarize a failed `just` run by its first diagnostic line.

    Args:
        proc: The failed process.

    Returns:
        The first stderr line, or the exit code when `just` printed nothing.

    """
    lines = [line for line in proc.stderr.splitlines() if line.strip()]
    return lines[0] if lines else f"`just` exited {proc.returncode}"


def main(argv: Sequence[str] | None = None) -> int:
    """Answer one name.

    Args:
        argv: The command-line arguments, without the program name.

    Returns:
        0 when the name resolves, 1 when it is unknown, 2 on refusal.

    """
    parser = argparse.ArgumentParser(description="Ask `just` whether one name is a recipe.")
    parser.add_argument("--justfile", default="justfile", help="the justfile (default: justfile)")
    parser.add_argument("name", help="one recipe name")
    args = parser.parse_args(argv)
    index = RecipeIndex(Path(args.justfile))
    try:
        known = index.exists(args.name)
    except RecipeIndexRefusal as error:
        print(f"just-recipe-index: {error}", file=sys.stderr)
        return EXIT_REFUSAL
    return EXIT_EXISTS if known else EXIT_UNKNOWN


if __name__ == "__main__":
    sys.exit(main())
