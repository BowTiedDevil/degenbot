"""The fork failure vocabulary survives the FFI seam by cause.

A driver branches on what to do — fix the environment, read the child's
stderr, widen the deadline, or stop retrying — so the cause has to arrive as
a Python type. A message string at the seam leaves every caller parsing
prose, and the retry policy above it guessing.

The types are FFI-raised (`create_exception!`), re-exported from
:mod:`degenbot.exceptions` so callers import them from one home.
"""

from pathlib import Path

import pytest

from degenbot._ffi.fork import AnvilFork as _FFIAnvilFork
from degenbot.exceptions import AnvilChildExitError, AnvilSpawnError, AnvilSpawnTimeoutError

ROOT = Path(__file__).resolve().parents[1]

EXITS_BEFORE_READY = (
    ROOT / "rust/crates/integrations/degenbot-fork/tests/fixtures/node-exits-before-ready.sh"
)

NONEXISTENT_PROGRAM = ROOT / "does-not-exist/anvil"


def test_the_spawn_family_is_importable_from_one_home():
    assert issubclass(AnvilSpawnError, RuntimeError)
    assert issubclass(AnvilChildExitError, RuntimeError)
    assert issubclass(AnvilSpawnTimeoutError, RuntimeError)
    assert AnvilSpawnError is not AnvilChildExitError


def test_a_program_that_could_not_run_is_a_typed_spawn_failure():
    with pytest.raises(AnvilSpawnError):
        _FFIAnvilFork(program=str(NONEXISTENT_PROGRAM))


def test_a_child_that_dies_before_readiness_is_a_typed_child_exit():
    with pytest.raises(AnvilChildExitError):
        _FFIAnvilFork(program=str(EXITS_BEFORE_READY))
