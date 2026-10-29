"""The pool-divergence memo's read surface on ``Dispatcher``.

The memo is the persistent cross-block skip state: a pool flagged `SolverCalc`
is dropped pre-sim for the decay window. A fresh dispatcher carries no
divergent pools and a zero lifetime drop tally; the action layer's skip +
feedback behaviour is proven at the Rust-core level, so these pin the FFI seam's
propagation of that state. The per-block ``[pool-divergence]`` signal render
reads ``outcome.failures`` — complementary to this persistent memo, not
redundant with it.
"""

from __future__ import annotations

from tests.helpers.seam import Dispatcher, DivergentPool


def test_fresh_dispatcher_has_no_divergent_pools() -> None:
    """A dispatcher with no feedback history reports no divergent pool."""
    dispatcher = Dispatcher.for_block(100)
    assert dispatcher.total_divergent_dropped == 0
    assert dispatcher.divergent_pools(100) == []


def test_divergent_pools_getter_is_typed() -> None:
    """The getter returns a list of the registered ``DivergentPool`` pyclass."""
    dispatcher = Dispatcher.for_block(100)
    assert isinstance(dispatcher.divergent_pools(100), list)
    assert DivergentPool is not None
