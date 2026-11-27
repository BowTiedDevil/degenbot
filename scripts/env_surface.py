"""The repository's environment surface, read from source instead of grepped.

One module answers the two questions the configuration discipline asks about
sources, so the Rust test tier and the Python suite read the same answers:

* **which environment names does the Python companion read?** ADR-062 D7 makes
  ``degenbot-config`` the one env-reading owner; the companion's sanctioned
  reads are the ``PYTHON_ALLOWLIST`` below, and every other read is a finding.
* **which ``DEGENBOT_*`` names exist in the Rust tree?** The schema parity gate
  diffs that inventory against the declared keys, so a key cannot arrive (or
  disappear) without the schema following.

The read scan is a syntax tree, not a token walk: an import alias
(``import os as o``, ``from os import environ``) resolves to the name it binds,
and a source that does not parse is a REFUSAL rather than silence. A scanner
whose silence can mean "the file did not parse" certifies nothing, so every
surface it cannot judge -- an unparseable source, a missing or empty tree, an
empty inventory sweep -- exits nonzero with the reason named.

Known limits, so a green run is read with them in mind: the scan reports NAMES,
never values; a mapping handed to something else (``dict(os.environ)``) is
``<computed>``; and a read reached only through ``getattr(os, "environ")`` is a
name no syntax tree decides. Each is a read the scan reports as computed or not
at all -- never one it silently calls sanctioned.

Run as the Rust adapters do:

    python3 scripts/env_surface.py reads --root src/degenbot
    python3 scripts/env_surface.py inventory
"""

from __future__ import annotations

import argparse
import ast
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

ROOT = Path(__file__).resolve().parents[1]

COMPUTED = "<computed>"

#: The companion's sanctioned env reads, keyed by companion-relative path.
#: Every entry is a name the typed schema cannot own, a read the cascade cannot
#: reach, or tooling plumbing the cascade does not express -- the same three
#: classes the Rust half's allowlist states. A NEW companion read fails the
#: scan, and its repair is either routing the value through ``degenbot-config``
#: or an enumerated, justified row here.
PYTHON_ALLOWLIST: dict[str, tuple[str, ...]] = {
    # ``name`` is the element of the closed retired-knob list
    # ``src/degenbot/runner/config.py::_RETIRED_SHELL_KNOBS`` walked for
    # PRESENCE (the load refuses any member, and nothing consumes a value), and
    # ``_RETIRED_INJECTION_KEY`` is the retired bare spelling of the injection
    # stance, refused everywhere; the honored spelling is the declared
    # ``simulation.inject_executor_code`` key, which arrives through the verdict.
    "src/degenbot/runner/config.py": ("name", "_RETIRED_INJECTION_KEY"),
    # The console level for the Python side of the log pipeline, the same class
    # as the Rust ``RUST_LOG`` tooling signal: output plumbing, not config. It
    # is read by ``base_log_level()`` when the base level is applied -- at
    # import, and again for any caller that re-applies it -- rather than frozen
    # into a module constant.
    "src/degenbot/logging.py": ("DEGENBOT_DEBUG",),
}

_ENV_READ_ATTRS = frozenset({"environ", "environb", "getenv"})

#: Directories whose Python sources are copies, caches, or another project
#: (the same population the repository's other source scans skip).
_SKIP_DIRS = frozenset({
    "__pycache__",
    "node_modules",
    "site-packages",
    "target",
})

#: The Rust tree the schema inventory sweeps, and the one exclusion: the config
#: crate's own tests carry negative-probe literals that are not real keys.
_INVENTORY_ROOTS = ("rust/crates",)
_INVENTORY_EXCLUSIONS = frozenset({"rust/crates/foundation/degenbot-config/tests"})
_INVENTORY_SKIP_DIRS = frozenset({"target", "node_modules"})
_INVENTORY_NAME = re.compile(r"DEGENBOT_[A-Z_]+")


class NamedRefusal(Exception):
    """A surface the scan could not judge, named so the caller can act."""


@dataclass(frozen=True)
class Read:
    """One environment read: where it is, and the name it denotes."""

    path: str
    line: int
    name: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.name}"


def python_sources(root: Path) -> list[Path]:
    """Every Python source under ``root``, sorted, skipping caches and copies."""
    if root.is_file():
        return [root] if root.suffix == ".py" else []
    return [
        path
        for path in sorted(root.rglob("*.py"))
        if path.is_file()
        and not _SKIP_DIRS.intersection(path.relative_to(root).parts[:-1])
        and not any(part.startswith(".") for part in path.relative_to(root).parts[:-1])
    ]


def scan_source(source: str, path: str = "<string>") -> list[Read]:
    """Every environment read in one source, in report order.

    Args:
        source: The Python source text.
        path: The name this source is reported under.

    Raises:
        NamedRefusal: The source does not parse, so nothing about its reads can
            honestly be reported.

    Returns:
        The reads, ordered by line and name.
    """
    try:
        tree = ast.parse(source, filename=path)
    except SyntaxError as broken:
        msg = (
            f"{path}:{broken.lineno}: the env-read scan reads Python as a syntax tree "
            f"and this source does not parse: {broken.msg}"
        )
        raise NamedRefusal(msg) from broken
    reader = _ReadFinder(path)
    return reader.reads(tree)


def scan_tree(
    roots: Sequence[str | Path],
    root: Path = ROOT,
    allow: Mapping[str, Sequence[str]] = PYTHON_ALLOWLIST,
) -> list[Read]:
    """Every unsanctioned environment read under ``roots``.

    Args:
        roots: Directories (or single files) to scan, each relative to ``root``
            unless absolute.
        root: The repository root the reported paths are relative to.
        allow: The sanctioned names per repo-relative path.

    Raises:
        NamedRefusal: A root is missing, or the walk reaches no Python source
            (an empty surface's silence is not evidence), or a source does not
            parse, or a source is not UTF-8.

    Returns:
        The violations, ordered by path, line, and name.
    """
    if not roots:
        msg = "no scan surface was named, so there is nothing to judge"
        raise NamedRefusal(msg)
    scanned: list[Path] = []
    for entry in roots:
        named = Path(entry)
        base = named if named.is_absolute() else root / named
        if not base.exists():
            msg = f"{named}: no such file or directory to scan"
            raise NamedRefusal(msg)
        sources = python_sources(base)
        if not sources:
            msg = f"{named}: the scan reached no Python source, so its silence would mean nothing"
            raise NamedRefusal(msg)
        scanned.extend(sources)
    violations: list[Read] = []
    for path in scanned:
        relative = _report_path(path, root)
        try:
            source = path.read_text(encoding="utf-8")
        except UnicodeDecodeError as undecodable:
            msg = f"{relative}: not UTF-8 Python source: {undecodable}"
            raise NamedRefusal(msg) from undecodable
        sanctioned = set(allow.get(relative, ()))
        violations.extend(
            read for read in scan_source(source, relative) if read.name not in sanctioned
        )
    return sorted(violations, key=lambda read: (read.path, read.line, read.name))


def inventory_names(root: Path = ROOT) -> list[str]:
    """The sorted, deduplicated ``DEGENBOT_*`` names in the Rust tree.

    Args:
        root: The repository root the sweep reads.

    Raises:
        NamedRefusal: The sweep root is missing, or the sweep found no name (a
            sweep that reached nothing cannot certify a schema).

    Returns:
        The names, sorted.
    """
    names: set[str] = set()
    for entry in _INVENTORY_ROOTS:
        base = root / entry
        if not base.is_dir():
            msg = f"{entry}: no such directory to sweep"
            raise NamedRefusal(msg)
        for path in sorted(base.rglob("*")):
            relative = path.relative_to(root).as_posix()
            if not path.is_file():
                continue
            if any(
                relative == skip or relative.startswith(f"{skip}/")
                for skip in _INVENTORY_EXCLUSIONS
            ):
                continue
            parts = path.relative_to(root).parts[:-1]
            if _INVENTORY_SKIP_DIRS.intersection(parts) or any(p.startswith(".") for p in parts):
                continue
            names.update(
                _INVENTORY_NAME.findall(path.read_bytes().decode("utf-8", errors="replace"))
            )
    if not names:
        msg = "the inventory sweep found no DEGENBOT_* name, so it cannot certify the schema"
        raise NamedRefusal(msg)
    return sorted(names)


def _report_path(path: Path, root: Path) -> str:
    """The path a finding is reported under: repo-relative when inside ``root``."""
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        return path.as_posix()


class _ReadFinder:
    """The read nodes of one syntax tree, with the aliases that bind them."""

    def __init__(self, path: str) -> None:
        self._path = path
        # The canonical module name counts as bound whether or not this source
        # imports it: a missing import is a different defect (the read would
        # raise at run time), and the gate's job is to see the read. An ALIAS is
        # the opposite case -- nothing but its import names it.
        self._modules: set[str] = {"os"}
        self._read_names: dict[str, str] = {}

    def reads(self, tree: ast.Module) -> list[Read]:
        """Every read in ``tree``, ordered by line and name."""
        self._bind(tree)
        parents = {
            child: parent for parent in ast.walk(tree) for child in ast.iter_child_nodes(parent)
        }
        found: list[Read] = []
        for node in ast.walk(tree):
            if isinstance(node, ast.Attribute | ast.Name) and self._is_read(node):
                found.append(Read(self._path, node.lineno, _read_name(node, parents)))
        return sorted(found, key=lambda read: (read.line, read.name))

    def _bind(self, tree: ast.Module) -> None:
        """Collect the aliases, tree-wide.

        Collection is per file rather than per scope: a name bound anywhere in a
        source makes that spelling a read everywhere in it. That is the
        fail-closed direction (an unused alias cannot hide one), and it costs a
        false positive only where a local variable shares an aliased read name.
        """
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    if alias.name == "os":
                        self._modules.add(alias.asname or alias.name)
            elif isinstance(node, ast.ImportFrom) and node.module == "os" and node.level == 0:
                for alias in node.names:
                    if alias.name == "*":
                        for exported in _ENV_READ_ATTRS:
                            self._read_names[exported] = exported
                    elif alias.name in _ENV_READ_ATTRS:
                        self._read_names[alias.asname or alias.name] = alias.name

    def _is_read(self, node: ast.AST) -> bool:
        if isinstance(node, ast.Attribute):
            return (
                node.attr in _ENV_READ_ATTRS
                and isinstance(node.value, ast.Name)
                and node.value.id in self._modules
            )
        return (
            isinstance(node, ast.Name)
            and isinstance(node.ctx, ast.Load)
            and node.id in self._read_names
        )


def _read_name(node: ast.AST, parents: Mapping[ast.AST, ast.AST]) -> str:
    """The name the read at ``node`` denotes, or ``COMPUTED``.

    A call or subscript argument is read as written, so a computed read is
    reported by the identifier it computes from -- the same convention the Rust
    half uses for the loader's ``env::var(name)`` seam. An expression no name is
    reducible from is ``<computed>``, so the whole-mapping read is loud and can
    never be enumerated by accident.
    """
    parent = parents.get(node)
    if isinstance(parent, ast.Attribute) and parent.value is node:
        return _attribute_name(parent, parents)
    if isinstance(parent, ast.Call) and parent.func is node:
        return _argument_name(parent)
    if isinstance(parent, ast.Subscript) and parent.value is node:
        return _name_of(parent.slice)
    if (
        isinstance(parent, ast.Compare)
        and any(isinstance(operation, ast.In) for operation in parent.ops)
        and any(node is comparator for comparator in parent.comparators)
    ):
        return _name_of(parent.left)
    return COMPUTED


def _attribute_name(attribute: ast.Attribute, parents: Mapping[ast.AST, ast.AST]) -> str:
    """The name a mapping attribute reads; only ``.get(name)`` names one."""
    if attribute.attr != "get":
        return COMPUTED
    call = parents.get(attribute)
    if isinstance(call, ast.Call) and call.func is attribute:
        return _argument_name(call)
    return COMPUTED


def _argument_name(call: ast.Call) -> str:
    """The name a call's argument denotes."""
    if call.args:
        return _name_of(call.args[0])
    for keyword in call.keywords:
        if keyword.arg == "key":
            return _name_of(keyword.value)
    return COMPUTED


def _name_of(expression: ast.expr) -> str:
    """The name an argument expression denotes, or ``COMPUTED``."""
    if isinstance(expression, ast.Constant):
        value = expression.value
        if isinstance(value, bytes):
            try:
                return value.decode("utf-8")
            except UnicodeDecodeError:
                return COMPUTED
        return value if isinstance(value, str) and value else COMPUTED
    if isinstance(expression, ast.Name):
        return expression.id
    return COMPUTED


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="env_surface.py",
        description="Scan the Python companion's env reads, or the Rust tree's DEGENBOT_* names.",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    reads = commands.add_parser("reads", help="report unsanctioned environment reads")
    reads.add_argument(
        "--root",
        action="append",
        dest="roots",
        metavar="PATH",
        help="a directory to scan, relative to the repository root (default: src/degenbot)",
    )
    commands.add_parser("inventory", help="print the Rust tree's DEGENBOT_* names")
    return parser


def main(argv: Sequence[str]) -> int:
    """Report findings (exit 1) or a named refusal (exit 2)."""
    args = _parser().parse_args(argv)
    try:
        if args.command == "inventory":
            for name in inventory_names():
                sys.stdout.write(f"{name}\n")
            return 0
        violations = scan_tree(args.roots or ["src/degenbot"])
    except NamedRefusal as refusal:
        sys.stderr.write(f"env_surface: {refusal}\n")
        return 2
    if not violations:
        return 0
    for violation in violations:
        sys.stdout.write(f"{violation}\n")
    sys.stderr.write(
        f"env_surface: {len(violations)} unsanctioned env read(s): route them through "
        "degenbot-config, or add an enumerated, justified entry in scripts/env_surface.py\n"
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
