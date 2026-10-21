"""Sybil-powered doctest for README.md.

Validates that Python code blocks in the project README remain importable
and syntactically valid after refactors.

This conftest lives at the project root so Sybil can discover README.md
relative to its path. It is picked up because pyproject.toml includes
"README.md" in testpaths.
"""

import functools
import pathlib
from collections.abc import Iterable

import pytest
from sybil import Document, Region, Sybil
from sybil.parsers.abstract import AbstractSkipParser
from sybil.parsers.markdown.clear import ClearNamespaceParser
from sybil.parsers.markdown.codeblock import PythonCodeBlockParser
from sybil.parsers.markdown.lexers import DirectiveInHTMLCommentLexer
from sybil.parsers.markdown.skip import SkipParser

# The pin MUST precede the first degenbot import: the FFI module init resolves
# the typed config from the process layers (first-wins), and the engine boot
# re-resolves those layers at every construction — see tests/suite_env.py.
# The unsorted physical order is the point, hence the I001 ignore.
import tests.suite_env  # ruff:ignore[I001, F401]  # import-side-effect pin

from degenbot.bot import _bot as _bot_module
from degenbot.provider import OfflineProvider
from tests.offline import is_offline


def _live_rpc_blocks_enabled() -> bool:
    """Report whether the README's live-RPC examples should execute.

    They run in local dev (RPC + anvil reachable) and are skipped in the constrained
    CI/CD runner that has neither. Offline mode is the CI default, shared with the
    test-suite fork fixtures via :func:`tests._offline.is_offline`.

    Returns:
        True when live-RPC blocks should run (online); False to skip them (offline).

    """
    return not is_offline()


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

_LIVE_RPC_START = "<!-- live-rpc: start"
_LIVE_RPC_END = "<!-- live-rpc: end"


def _live_rpc_line_spans(readme_text: str) -> list[tuple[int, int]]:
    """The line spans (inclusive) of the README's ``live-rpc`` blocks.

    Every leg inside such a block talks to a live archive node, so it belongs to
    the on-demand tier, not the offline gate (ADR-074: `test` / pre-push / CI run
    `-m 'not online_rpc'`, and `just test-fork` runs `-m online_rpc`).

    Returns:

        One ``(first_line, last_line)`` pair per block, 1-indexed.

    """
    spans: list[tuple[int, int]] = []
    first: int | None = None
    for number, line in enumerate(readme_text.splitlines(), start=1):
        stripped = line.strip()
        if stripped.startswith(_LIVE_RPC_START):
            first = number
        elif stripped.startswith(_LIVE_RPC_END) and first is not None:
            spans.append((first, number))
            first = None
    return spans


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

    # A README block inside a `live-rpc` span talks to a live archive node, so it
    # joins the on-demand tier instead of the offline gate. The span map comes
    # from the document itself, so a new live-RPC example is covered by the
    # directive, not by remembering to add a marker.
    live_rpc_spans = _live_rpc_line_spans(
        (pathlib.Path(__file__).resolve().parent / "README.md").read_text(encoding="utf-8")
    )

    def in_live_rpc_block(item: pytest.Item) -> bool:
        line = readme_sort_key(item)
        return any(first <= line <= last for first, last in live_rpc_spans)

    for index, item in enumerate(readme_items):
        item.add_marker(pytest.mark.order(index))
        if in_live_rpc_block(item):
            item.add_marker(pytest.mark.online_rpc)

    items[:] = readme_items + other_items


# --- README offline replay ---------------------------------------------------
#
# The README's Bot examples run against a recorded chain cassette instead of a
# live archive node, so the suite is hermetic and offline (no RPC, no developer
# DB): every `eth_call`/`eth_getCode`/`eth_getBlockByNumber` the examples issue
# is replayed from `tests/fixtures/chain_data/1/readme_examples.json` through the
# `Bot(provider=...)` injection seam. The visible README code is unchanged — it
# still reads `degenbot.Bot(chain_id=1, node=RPC_URL, ...)`; only the DI seam
# `get_provider_from_config` is redirected, for README items only.
#
# Regenerate the cassette with `just record-readme-cassette`; gate it with
# `just check-readme-cassette`. See `scripts/record_readme_cassette.py`.
_README_CASSETTE = (
    pathlib.Path(__file__).resolve().parent / "tests/fixtures/chain_data/1/readme_examples.json"
)


# The provider factory is redirected for the duration of a README item and
# restored in teardown (Sybil items do not run conftest autouse fixtures, so the
# bind is done from the runtest hooks). `pytest.MonkeyPatch` owns the
# save/restore, so there is no module-level mutable state.
@functools.lru_cache(maxsize=1)
def _readme_offline_provider() -> OfflineProvider:
    """The cassette-backed provider, built once and reused across README items."""
    return OfflineProvider.from_json_file(_README_CASSETTE)


def pytest_runtest_setup(item: pytest.Item) -> None:
    """Bind the recorded cassette provider for README items (no-op elsewhere)."""
    if "README.md" not in item.nodeid or not _README_CASSETTE.is_file():
        return

    monkeypatch = pytest.MonkeyPatch()
    monkeypatch.setattr(
        _bot_module,
        "get_provider_from_config",
        lambda **_kwargs: _readme_offline_provider(),
    )
    item._readme_monkeypatch = monkeypatch  # type: ignore[attr-defined]


def pytest_runtest_teardown(item: pytest.Item, nextitem: pytest.Item | None) -> None:  # noqa: ARG001
    """Restore the configured provider factory after a README item."""
    monkeypatch = getattr(item, "_readme_monkeypatch", None)
    if monkeypatch is not None:
        monkeypatch.undo()
        item._readme_monkeypatch = None  # type: ignore[attr-defined]
