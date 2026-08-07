"""Sybil-powered doctest for README.md.

Validates that Python code blocks in the project README remain importable
and syntactically valid after refactors.

This conftest lives at the project root so Sybil can discover README.md
relative to its path. It is picked up because pyproject.toml includes
"README.md" in testpaths.
"""

import os
from collections.abc import Iterable

import pytest
from sybil import Document, Region, Sybil
from sybil.parsers.abstract import AbstractSkipParser
from sybil.parsers.markdown.clear import ClearNamespaceParser
from sybil.parsers.markdown.codeblock import PythonCodeBlockParser
from sybil.parsers.markdown.lexers import DirectiveInHTMLCommentLexer
from sybil.parsers.markdown.skip import SkipParser


def _live_rpc_blocks_enabled() -> bool:
    """Report whether the README's live-RPC examples should execute.

    They run in local dev (RPC + anvil reachable) and are skipped in the constrained
    CI/CD runner that has neither. Offline mode is the CI default (GitHub Actions
    exports ``CI=true``) and can be forced either way with ``DEGENBOT_OFFLINE=1|0``
    for local reproduction of CI behaviour.

    Returns:
        True when live-RPC blocks should run (online); False to skip them (offline).

    """
    override = os.environ.get("DEGENBOT_OFFLINE")
    if override is not None:
        return override.lower() not in {"1", "true", "yes"}
    return os.environ.get("CI", "").lower() != "true"


class _LiveRpcSkipParser(AbstractSkipParser):
    """Skip parser for the README's live-RPC code blocks.

    Recognises ``<!-- live-rpc: start -->`` / ``<!-- live-rpc: end -->`` directives.
    When live-RPC blocks are enabled (local dev) these directives are inert and the
    enclosed code runs; when disabled (offline CI) they raise ``pytest.skip`` so the
    doctest stays green without network access.

    This is deliberately independent of the standard :class:`SkipParser` (``skip:``)
    and of the document namespace — the flag lives in this process, so the
    ``clear-namespace`` directive mid-document cannot erase it. Pre-existing
    unconditional ``skip:`` directives keep their always-on behaviour.
    """

    directive = "live-rpc"

    def __init__(self, *, enabled: bool) -> None:
        self._enabled = enabled
        super().__init__([DirectiveInHTMLCommentLexer(self.directive)])

    def __call__(self, document: Document) -> Iterable[Region]:
        if self._enabled:
            # Online: don't emit the skip regions at all, so the blocks execute.
            return ()
        return super().__call__(document)


pytest_collect_file = Sybil(
    parsers=[
        PythonCodeBlockParser(),
        SkipParser(),
        _LiveRpcSkipParser(enabled=_live_rpc_blocks_enabled()),
        ClearNamespaceParser(),
    ],
    path=".",
    patterns=["README.md"],
    excludes=["*/README.md"],
).pytest()


_EXPECTED_LINE_PARTS = 2


@pytest.hookimpl(tryfirst=True)
def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    """Restore document order for README.md sybil items and pin them to one xdist worker.

    Sybil's skip directive state is tracked per-document and depends on sequential, ordered
    evaluation. Two pytest plugins break this:
    - pytest-randomly reorders items, breaking the skip state machine.
    - pytest-xdist distributes items across workers, so each worker sees
      an unbalanced subset of skip:start/skip:end pairs.

    This hook runs before pytest-randomly's shuffle (``tryfirst=True``) and uses
    ``pytest.mark.order(N)`` to pin each README item to its document position. pytest-order
    (``trylast=True`` by default) then re-orders these items back into sequence after the random
    shuffle.

    It also marks them with a shared ``xdist_group`` so ``--dist=loadgroup`` sends them to a single
    worker.
    """
    readme_items = []
    other_items = []
    for item in items:
        if "README.md" in item.nodeid:
            readme_items.append(item)
        else:
            other_items.append(item)

    # Sort README items by line number to restore document order
    def readme_sort_key(item: pytest.Item) -> int:
        parts = item.nodeid.split("line:")
        if len(parts) == _EXPECTED_LINE_PARTS:
            try:
                return int(parts[1].split(",")[0])
            except ValueError:
                return 0
        return 0

    readme_items.sort(key=readme_sort_key)

    for index, item in enumerate(readme_items):
        item.add_marker(pytest.mark.order(index))
        item.add_marker(pytest.mark.xdist_group("readme_sybil"))

    items[:] = readme_items + other_items
