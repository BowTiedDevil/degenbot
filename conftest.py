"""
Sybil-powered doctest for README.md.

Validates that Python code blocks in the project README remain importable
and syntactically valid after refactors.

This conftest lives at the project root so Sybil can discover README.md
relative to its path. It is picked up because pyproject.toml includes
"README.md" in testpaths.
"""

import pytest
from sybil import Sybil
from sybil.parsers.markdown.clear import ClearNamespaceParser
from sybil.parsers.markdown.codeblock import PythonCodeBlockParser
from sybil.parsers.markdown.skip import SkipParser

pytest_collect_file = Sybil(
    parsers=[PythonCodeBlockParser(), SkipParser(), ClearNamespaceParser()],
    path=".",
    patterns=["README.md"],
    excludes=["*/README.md"],
).pytest()


@pytest.hookimpl(trylast=True)
def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    """
    Restore document order for README.md sybil items and pin them to one
    xdist worker.

    Sybil's skip directive state is tracked per-document and depends on
    sequential, ordered evaluation. Two pytest plugins break this:

    - pytest-randomly reorders items, breaking the skip state machine.
    - pytest-xdist distributes items across workers, so each worker sees
      an unbalanced subset of skip:start/skip:end pairs.

    This hook uses ``trylast=True`` so it runs after pytest-randomly's
    shuffle, then re-sorts README items back into document (line) order
    and marks them with a shared ``xdist_group`` so ``--dist=loadgroup``
    sends them to a single worker.
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
        if len(parts) == 2:
            try:
                return int(parts[1].split(",")[0])
            except ValueError:
                return 0
        return 0

    readme_items.sort(key=readme_sort_key)

    for item in readme_items:
        item.add_marker(pytest.mark.xdist_group("readme_sybil"))

    items[:] = readme_items + other_items
