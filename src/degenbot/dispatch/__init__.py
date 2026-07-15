"""Companion-layer surface over the Rust dispatch/sim/signer pyclasses.

This package re-exports the Rust-owned dispatch, simulation, and transaction
signing symbols under stable, seam-agnostic names. Driver code (bot
operators, example bots) imports from here — never from the PyO3 wrapper
:mod:`degenbot_rs`.

The ``Py*`` prefix and ``*_py`` suffix on the FFI names are the seam naming
itself: ``Py*`` marks a raw ``#[pyclass]``, ``*_py`` marks a
``#[pyfunction]``. Those conventions are for the FFI layer's own bookkeeping
and should never leak into driver code. This package hides them.

.. note::

    These are **direct alias re-exports** (``from degenbot._ffi import PyX as
    X``), not Python wrapper classes. The Rust engine constructs and
    consumes these pyclasses / pyfunctions directly — driver code constructs
    a ``SimulateContext`` / ``TxSigner`` in Python and passes it to a Rust
    pyfunction that expects the exact ``PySimulateContext`` /
    ``PyTxSigner`` pyclass. A wrapper class would break type identity at the
    FFI boundary. The companion's only job is to give the symbols stable
    names so driver code does not import ``degenbot_rs``.

    If a future driver needs Python-side ergonomics over these types (block-tag
    resolution, ``__repr__``, context-manager protocol — the way
    :class:`degenbot.provider.AlloyProvider` wraps the Rust
    ``AlloyProvider``), a wrapper-class follow-up can add it; out of scope
    here.

Symbol map (FFI name → stable companion name):

- ``PyDispatchCandidate`` → :class:`DispatchCandidate`
- ``PyDispatchOutcome``   → :class:`DispatchOutcome`
- ``PyDispatcher``        → :class:`Dispatcher`
- ``PySimulateContext``   → :class:`SimulateContext`
- ``PyTxSigner``          → :class:`TxSigner`
- ``dispatch_profitable_py`` → :func:`dispatch_profitable`
- ``dispatch_and_submit_py`` → :func:`dispatch_and_submit`
- ``fetch_fee_history_py``   → :func:`fetch_fee_history`
"""

from degenbot._ffi import (
    PyDispatchCandidate as DispatchCandidate,
)
from degenbot._ffi import (
    PyDispatcher as Dispatcher,
)
from degenbot._ffi import (
    PyDispatchOutcome as DispatchOutcome,
)
from degenbot._ffi import (
    PySimulateContext as SimulateContext,
)
from degenbot._ffi import (
    PyTxSigner as TxSigner,
)
from degenbot._ffi import (
    dispatch_and_submit_py as dispatch_and_submit,
)
from degenbot._ffi import (
    dispatch_profitable_py as dispatch_profitable,
)
from degenbot._ffi import (
    fetch_fee_history_py as fetch_fee_history,
)

__all__ = [
    "DispatchCandidate",
    "DispatchOutcome",
    "Dispatcher",
    "SimulateContext",
    "TxSigner",
    "dispatch_and_submit",
    "dispatch_profitable",
    "fetch_fee_history",
]
