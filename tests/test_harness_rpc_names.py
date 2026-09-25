"""The ``ETHEREUM_*_NODE_*_URI`` names stay inside the test harness.

The suite resolves endpoints through ``tests/conftest.py``'s
``*_{ARCHIVE,FULL}_NODE_{HTTP,WS}_URI`` names. They are test-scoped on purpose:
the suite needs two node tiers per chain, while the operator file's ``[nodes]``
tables hold one endpoint per transport per chain. Three properties follow, and
each is guarded here because the failure mode is a silently wrong endpoint
rather than an error:

* user-facing documentation shows a literal endpoint in the fences a reader
  can see, never an import of the pytest harness;
* the boundary is stated where an operator reads it (``tests.env.example``);
* ``_rpc`` never grows a ``DEGENBOT_RPC_*`` fallback, which would leave two
  namespaces sharing a lookup and the next ambiguity unguarded.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

from tests import conftest as harness

if TYPE_CHECKING:
    from collections.abc import Iterator

REPO_ROOT = Path(__file__).resolve().parent.parent

HARNESS_IMPORT = "from tests.conftest import"

INVISIBLE_BLOCK_START = "<!-- invisible-code-block"
COMMENT_END = "-->"
FENCE = "```"
PYTHON_FENCE = f"{FENCE}python"


def _user_facing_docs() -> Iterator[Path]:
    yield REPO_ROOT / "README.md"
    yield from sorted((REPO_ROOT / "docs").rglob("*.md"))


def _reader_facing_python_fences(text: str) -> Iterator[tuple[int, str]]:
    """Yield ``(start line, source)`` for every ``python`` fence a reader can see.

    A fence enclosed by an ``<!-- invisible-code-block ... -->`` region is an
    execution shim rather than documentation, so its source is not yielded.

    Yields:
        The line the fence opens on and the lines it contains.
    """
    body: list[str] | None = None
    start = 0
    hidden = False
    for number, line in enumerate(text.splitlines(), start=1):
        if body is not None:
            if line.lstrip().startswith(FENCE):
                if not hidden:
                    yield start, "\n".join(body)
                body = None
            else:
                body.append(line)
            continue
        if not hidden and INVISIBLE_BLOCK_START in line:
            hidden = True
        elif line.lstrip().startswith(PYTHON_FENCE):
            body, start = [], number
        if hidden and COMMENT_END in line:
            hidden = False


def test_user_facing_docs_do_not_import_the_test_harness() -> None:
    """Fail only when the harness import sits in a fence a reader can see.

    Reason: the import is a suite execution detail, not user configuration.
    README.md keeps it inside ``<!-- invisible-code-block ... -->`` regions that
    exist solely so the surrounding examples run, so a hidden occurrence is
    correct; the same import in a visible ``python`` fence would teach users to
    import the pytest harness.
    """
    offenders = [
        f"{path.relative_to(REPO_ROOT)}:{line_number}"
        for path in _user_facing_docs()
        for line_number, source in _reader_facing_python_fences(path.read_text())
        if HARNESS_IMPORT in source
    ]
    assert not offenders, (
        f"user-facing docs import the pytest harness in a visible fence: {offenders}"
    )


def test_hidden_execution_regions_are_not_reader_facing() -> None:
    """Reason: the guard above only means something if it can tell the two apart.

    A fence hidden inside an invisible region is where the suite's endpoint
    binding belongs, so the import there is not reported; the same import in a
    visible fence is.
    """
    hidden = f"{INVISIBLE_BLOCK_START}: python\n{HARNESS_IMPORT} RPC_URL\n{COMMENT_END}\n"
    visible = f"{PYTHON_FENCE}\n{HARNESS_IMPORT} RPC_URL\n{FENCE}\n"
    assert not [
        source for _, source in _reader_facing_python_fences(hidden) if HARNESS_IMPORT in source
    ]
    assert [
        source for _, source in _reader_facing_python_fences(visible) if HARNESS_IMPORT in source
    ]


def test_tests_env_example_states_the_harness_boundary() -> None:
    text = (REPO_ROOT / "tests.env.example").read_text()
    for statement in ("test harness", "DEGENBOT_RPC_", "[nodes]", "ape-config.yaml"):
        assert statement in text, f"tests.env.example does not state the boundary via {statement!r}"


def test_rpc_resolver_has_no_degenbot_namespace_fallback() -> None:
    code = harness._rpc.__code__
    # co_consts[0] is the function docstring, which names the operator namespace
    # BY DESIGN; scanning it would make this guard contradict its own subject, so
    # the literals under test start at index 1.
    literals = " ".join(c for c in code.co_consts[1:] if isinstance(c, str))
    assert "DEGENBOT_RPC" not in literals
    assert not any("DEGENBOT_RPC" in name for name in code.co_names)
