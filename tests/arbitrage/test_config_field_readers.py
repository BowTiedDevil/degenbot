"""Every ``ArbitrageConfig`` field must have a reader.

A frozen-dataclass field assigned in ``ArbitrageConfig.build`` and never read is *not*
statically unreachable — the assignment is itself a use — so vulture
(``just dead-code``) passes such a field. What the analyser cannot see is the
defect that matters to an operator: the field survives as a declared,
documented, typed entry of the config value object while nothing consults it,
so setting it earns silence.

The gate is an AST reader census over the Python tree rather than a
reachability check: every field declared on ``ArbitrageConfig`` must be
attribute-loaded somewhere. The census counts an attribute load wherever one
appears and silently skips any file that does not parse, so what it can
honestly report is "read somewhere in the scanned roots", never "read by an
operator": it is a heuristic, and only as sound as those two limits.
"""

from __future__ import annotations

import ast
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterable

from degenbot.runner import config as config_module
from degenbot.runner.config import ArbitrageConfig

_CONFIG_PATH = Path(config_module.__file__)

#: Roots whose attribute loads count as readers. Tests legitimately assert on
#: config fields, and a field read only from a test is still a real field, so
#: excluding them would flag live fields as dead. The cost is that any file in
#: these roots can satisfy the census by naming the attribute: a test asserting
#: on ``cfg.<field>`` manufactures the very reader it is checked for. The
#: check is therefore a heuristic, not a proof.
_READER_ROOTS = ("src", "tests", "examples", "scripts")


def _repo_root() -> Path:
    """The checkout root, derived from the installed package location."""
    return _CONFIG_PATH.parents[3]


def _python_sources() -> list[Path]:
    root = _repo_root()
    sources: list[Path] = []
    for name in _READER_ROOTS:
        base = root / name
        if base.is_dir():
            sources.extend(sorted(base.rglob("*.py")))
    return sources


def _declared_fields() -> list[str]:
    """The annotated field names declared on ``ArbitrageConfig``, in order."""
    tree = ast.parse(_CONFIG_PATH.read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == ArbitrageConfig.__name__:
            return [
                stmt.target.id
                for stmt in node.body
                if isinstance(stmt, ast.AnnAssign) and isinstance(stmt.target, ast.Name)
            ]
    raise AssertionError(f"class {ArbitrageConfig.__name__} not found in {_CONFIG_PATH}")


def _reader_census(fields: Iterable[str]) -> dict[str, list[str]]:
    """Every ``file:line`` that attribute-*loads* each field, in one tree walk.

    A store (``cfg.field = ...``) is not a read; a frozen dataclass has no
    legitimate one, and counting it would let a write pose as a consumer.

    One parse per source file, bucketed by the declared fields, instead of a
    re-parse per field: the tree size dwarfs the field count, so the per-field
    form costs O(fields x files) parses for a census whose answer is O(files).
    """
    wanted = set(fields)
    sites: dict[str, list[str]] = {field: [] for field in fields}
    for path in _python_sources():
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"))
        # Fails open: this file's reads do not count, so a field read only here
        # reads as unread. That is a documented limit of a gate that is already
        # a heuristic (see the module docstring), and raising instead would hand
        # this gate a red it does not own for a file under ``examples`` or
        # ``scripts`` that nothing imports and nothing collects. The realistic
        # cases stay loud on their own — a broken ``src`` file breaks this
        # module's own import, a broken ``tests`` file breaks pytest collection.
        except SyntaxError:  # pragma: no cover
            continue
        rel = path.relative_to(_repo_root())
        for node in ast.walk(tree):
            if (
                isinstance(node, ast.Attribute)
                and isinstance(node.ctx, ast.Load)
                and node.attr in wanted
            ):
                sites[node.attr].append(f"{rel}:{node.lineno}")
    return sites


def test_every_config_field_has_a_reader() -> None:
    """No declared field may be a knob nothing reads."""
    fields = _declared_fields()
    readers = _reader_census(fields)
    unread = sorted(name for name in fields if not readers[name])
    assert not unread, (
        f"{len(unread)} ArbitrageConfig field(s) are constructed by ArbitrageConfig.build and read "
        f"nowhere: {unread}. The build assignment keeps such a field statically "
        f"reachable, so dead-code analysis passes it; only this census sees it. Delete "
        f"the field, or read it at the call site that should honour it."
    )
