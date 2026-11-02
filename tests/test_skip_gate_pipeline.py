"""Driver-side skip recording for path registration.

The reason-tagged skip breakdown (``[build_paths] Progress`` lines) is folded by
the ENGINE now (the crawl's summary); what stays Python-side is the OBSERVATION
POLICY: each skip reason lands in the Rust ``degenbot.registration.skips`` meter
family through the FFI, and the first few occurrences of a reason carry their
detail into the debug log. This suite pins that policy against a recording fake.
"""

from __future__ import annotations

from degenbot.runner.build_paths import _observe_skip


class _RecordingPyBot:
    def __init__(self) -> None:
        self.reasons: list[str] = []

    def record_registration_skip(self, reason: str) -> None:
        self.reasons.append(reason)


class _MeteredBot:
    """A bot handle whose ``_py_bot`` is the skip meter."""

    def __init__(self, py_bot: object | None) -> None:
        self._py_bot = py_bot


def test_each_skip_reason_reaches_the_rust_meter() -> None:
    meter = _RecordingPyBot()
    bot = _MeteredBot(meter)
    _observe_skip(bot, "v4-dynamic-fee-rejected")
    _observe_skip(bot, "path-cap")
    assert meter.reasons == ["v4-dynamic-fee-rejected", "path-cap"]


def test_a_bot_without_a_meter_is_silent() -> None:
    # A construction context without a live core (unit tests, a bare handle)
    # must not attempt the meter record.
    _observe_skip(_MeteredBot(None), "v4-no-hash")
    _observe_skip(object(), "v4-no-hash")
