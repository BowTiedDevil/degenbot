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
"""

from __future__ import annotations

import argparse
import re
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Sequence

EXIT_EXISTS = 0
EXIT_UNKNOWN = 1
EXIT_REFUSAL = 2

# A recipe name as just spells one: an identifier of letters, digits, `_` and
# `-` that cannot start with a digit. Anything else can never be a recipe name,
# so it is answered "unknown" without being handed to `just`, where a name
# shaped like `--version` would be read as a flag rather than a question.
_RECIPE_NAME = re.compile(r"^[a-zA-Z_][a-zA-Z0-9_-]*$")


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
