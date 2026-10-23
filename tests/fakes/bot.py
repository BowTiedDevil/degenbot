"""The one ``Bot`` double for the runner and registry seams.

``Bot`` owns the Rust engine on the Python side, so every test that drives the
runner or the engine registry injects a stand-in for it. The stand-in lives
here rather than in each test module, so a surface change lands in one file.

The double carries the driver-shell slice the Python side reads:

* ``chain_id`` — stamped into the construction context (``build_paths``);
* ``release_python_state()`` — the state trim's observable side
  (``released`` plus the ``"release"`` call name on the shared event list);
* ``block_stream()`` — the coordinator-owned block-clock receiver, handed out
  exactly once (a second call raises, as the real receiver's mpsc half is moved
  out on the first call and would crash the pump);
* ``_py_bot`` — the Rust bot handle the engine registry builds its engine over,
  and the handle the trim closes the snapshot read-tx on.

Per-test differences are constructor knobs, not subclasses:

* ``events`` — the list the double appends its call names to, so a test can
  assert call order across this double and the actor doubles beside it.
  ``None`` records nothing (the double keeps no list of its own).
* ``blocks`` — the block dicts the receiver yields, in order, then
  ``StopAsyncIteration``. The default empty list is the stream that ends
  immediately.
* ``py_bot`` — a real ``Bot`` (the shared-BotState registry tests) or a
  hand-written ``_py_bot`` stand-in (a snapshot-tx canary). ``None`` is honest
  for a runner test: the trim guards on ``py_bot is not None``, so a double
  without one skips the close exactly as a cold-start bot does.
"""

from __future__ import annotations


class FakeBlockStream:
    """Async iterator over a fixed block list, then ``StopAsyncIteration``.

    Stand-in for the once-only ``Bot.block_stream()`` receiver (the
    ``BlockStream.__anext__`` shape the result consumer awaits).
    """

    def __init__(self, blocks: list[dict[str, int]] | None = None) -> None:
        self._blocks = list(blocks or ())
        self._index = 0

    def __aiter__(self) -> FakeBlockStream:
        return self

    async def __anext__(self) -> dict[str, int]:
        if self._index >= len(self._blocks):
            raise StopAsyncIteration
        block = self._blocks[self._index]
        self._index += 1
        return block


class FakeBot:
    """In-process stand-in for the runner/registry ``Bot`` seam.

    Nothing is invented: the receiver is once-only and the release is recorded,
    because those are the two facts the runner tests assert. Every other read
    answers a fixed value the driver shell never branches on.
    """

    def __init__(
        self,
        *,
        chain_id: int = 1,
        events: list[str] | None = None,
        blocks: list[dict[str, int]] | None = None,
        py_bot: object | None = None,
    ) -> None:
        self.chain_id = chain_id
        self._py_bot = py_bot
        self.released = False
        self.block_stream_calls = 0
        self._events = events
        self._blocks = list(blocks or ())

    def _record(self, name: str) -> None:
        if self._events is not None:
            self._events.append(name)

    def release_python_state(self) -> None:
        """Record the state trim; dropping the Python refs is the caller's job."""
        self.released = True
        self._record("release")

    def block_stream(self) -> FakeBlockStream:
        """Hand out the block-clock receiver, exactly once."""
        self.block_stream_calls += 1
        if self.block_stream_calls > 1:
            msg = "block_stream() can only be called once"
            raise RuntimeError(msg)
        return FakeBlockStream(self._blocks)
