"""The one ``Bot`` double for the runner and registry seams.

``Bot`` owns the Rust engine on the Python side, so every test that drives the
runner or the engine registry injects a stand-in for it. The stand-in lives
here rather than in each test module, so a surface change lands in one file.

The double carries the driver-shell slice the Python side reads:

* ``chain_id`` — stamped into the construction context (``build_paths``);
* ``release_python_state()`` — the state trim's observable side
  (``released`` plus the ``"release"`` call name on the shared event list);
* ``_py_bot`` — the Rust bot handle the engine registry builds its engine over,
  and the handle the trim closes the snapshot read-tx on
  (:class:`RecordingPyBot` is that handle's double).

The block clock is NOT here any more: the session's ordered stream carries the
header arm and the Rust session consumer owns the clock, so a ``Bot`` double has
no tick receiver to hand out.

Per-test differences are constructor knobs, not subclasses:

* ``events`` — the list the double appends its call names to, so a test can
  assert call order across this double and the actor doubles beside it.
  ``None`` records nothing (the double keeps no list of its own).
* ``py_bot`` — a real ``Bot`` (the shared-BotState registry tests) or a
  hand-written ``_py_bot`` stand-in (a snapshot-tx canary). ``None`` is honest
  for a runner test: the trim guards on ``py_bot is not None``, so a double
  without one skips the close exactly as a cold-start bot does.
"""

from __future__ import annotations


class FakeBot:
    """In-process stand-in for the runner/registry ``Bot`` seam.

    Nothing is invented: every member is a fact the runner tests assert. Every
    other read answers a fixed value the driver shell never branches on.
    """

    def __init__(
        self,
        *,
        chain_id: int = 1,
        events: list[str] | None = None,
        py_bot: object | None = None,
    ) -> None:
        self.chain_id = chain_id
        self._py_bot = py_bot
        self.released = False
        self._events = events

    def _record(self, name: str) -> None:
        if self._events is not None:
            self._events.append(name)

    def release_python_state(self) -> None:
        """Record the state trim; dropping the Python refs is the caller's job."""
        self.released = True
        self._record("release")


class RecordingPyBot:
    """The Rust ``Bot._py_bot`` handle double, with the snapshot fence.

    Models the retained-tx guard at the Python seam:

    * ``acquire_tick_map_handle()`` is one ``assemble_*_tick_map`` call taking
      its handle (the guard's ``acquire``);
    * ``release_tick_map_handle()`` is that call releasing it;
    * ``close_snapshot_tx()`` is the guard's commit: it refuses while any
      assembler still holds a handle, exactly as the guard's drain-then-canary
      does.

    So a test can assert the ORDERING INVARIANT directly — no assembler is live
    when the canary runs — instead of observing a bare drop-commit call.
    """

    def __init__(self, events: list[str]) -> None:
        self._events = events
        self.in_flight = 0

    def acquire_tick_map_handle(self) -> None:
        """Take one assembly call's handle."""
        self.in_flight += 1
        self._events.append("assembler_acquired")

    def release_tick_map_handle(self) -> None:
        """Release one assembly call's handle."""
        self.in_flight -= 1
        self._events.append("assembler_released")

    def close_snapshot_tx(self) -> None:
        """Commit the read tx — refused while an assembler still holds a handle."""
        if self.in_flight:
            msg = (
                "close_snapshot_tx: tick-map assemblies still hold the "
                "SnapshotDb after the quiescence drain"
            )
            raise RuntimeError(msg)
        self._events.append("close_snapshot_tx")
