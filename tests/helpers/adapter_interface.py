"""Instruments for the declared-interface surface (ADR-100 D10).

Both halves of the boundary are *counting* claims — "construction invokes no
callback", "a case run encodes once per case", "the case path reads no chain" —
so they are checked against recording stand-ins rather than asserted:

- :class:`CountingComposer` is a compose callable over one fixed payload, and
  :class:`BuilderComposer` one whose payload is built from the `SolveResult` view
  it receives; both keep their call log and the view of every call;
- :class:`FailingComposer` is a callable that refuses, so "a broken encoder is
  reported, never excused" is a fact about the report;
- :class:`DecliningComposer` raises the typed routine decline
  (:class:`UnsupportedPath`), so "this path is outside what my contract can
  encode" is a fact about the classification, not about a message string;
- :class:`CountingProvider` is a provider stand-in that records every attribute
  access, so a run that reached a provider cannot pass silently.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from tests.helpers.seam import UnsupportedPath

if TYPE_CHECKING:
    from collections.abc import Callable


class CountingComposer:
    """A compose callable over one fixed payload, with its call log."""

    def __init__(self, payload: bytes) -> None:
        """Record ``payload`` and start an empty call log."""
        self.payload = payload
        self.views: list[Any] = []
        self.calls = 0

    def __call__(self, result: Any) -> bytes:
        """Record the view the seam handed over and return the payload."""
        self.calls += 1
        self.views.append(result)
        return self.payload

    @property
    def called(self) -> bool:
        """Whether the seam invoked this callable at all."""
        return self.calls > 0


class BuilderComposer:
    """A compose callable that builds its payload from the view it receives."""

    def __init__(self, build: Callable[[Any], bytes]) -> None:
        """Bind the payload builder and start an empty call log."""
        self.build = build
        self.views: list[Any] = []
        self.calls = 0

    def __call__(self, result: Any) -> bytes:
        """Record the view, then build the payload from it."""
        self.calls += 1
        self.views.append(result)
        return self.build(result)


class FailingComposer:
    """A compose callable that refuses every path."""

    def __init__(self, message: str) -> None:
        """Bind the refusal message and start an empty call log."""
        self.message = message
        self.calls = 0

    def __call__(self, _result: Any) -> bytes:
        """Count the call, then refuse."""
        self.calls += 1
        raise ValueError(self.message)


class DecliningComposer:
    """A compose callable that declines every path with the typed signal."""

    def __init__(self, message: str = "this contract cannot encode the path") -> None:
        """Bind the decline message and start an empty call log."""
        self.message = message
        self.views: list[Any] = []
        self.calls = 0

    def __call__(self, result: Any) -> bytes:
        """Record the view, then decline the path."""
        self.calls += 1
        self.views.append(result)
        raise UnsupportedPath(self.message)


class CountingProvider:
    """A provider stand-in that records every attribute access and call."""

    def __init__(self) -> None:
        """Start the access log."""
        self.calls = 0

    def __getattr__(self, name: str) -> object:
        """Count the access and hand back a refusing recorder."""
        self.calls += 1

        def _reached(*_args: object, **_kwargs: object) -> object:
            self.calls += 1
            message = f"this path reached provider.{name}, which owns no chain access"
            raise AssertionError(message)

        return _reached
