"""The fork lane's upstream-endpoint policy, exercised with no network.

:mod:`tests.fork_endpoint` waits out a hosted endpoint's transient refusal
(HTTP 429 / 5xx) before a forked anvil spawns: the refused request is anvil's
own genesis fetch, so no client-side retry can re-issue it. Every seam the
policy needs (the HTTP opener, the sleep, the clock, the spawn callable) is
injected here, so the policy is provable offline.
"""

import urllib.error
from typing import Self

import pytest

from tests.fork_endpoint import (
    ForkUpstreamUnavailable,
    UpstreamAnswer,
    probe_fork_upstream,
    spawn_fork,
    wait_for_upstream,
)

URL = "https://hosted.example/v1/key"


class _Response:
    """Minimal stand-in for the response object `urlopen` returns."""

    def __init__(self, status: int) -> None:
        self.status = status

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_: object) -> None:
        return None


class _Opener:
    """An opener answering with a scripted status, or raising a scripted error."""

    def __init__(self, answer: int | Exception) -> None:
        self._answer = answer

    def __call__(self, request: object, timeout: float) -> _Response:
        assert request is not None
        assert timeout > 0
        if isinstance(self._answer, Exception):
            raise self._answer
        return _Response(self._answer)


@pytest.mark.parametrize(
    ("answer", "expected"),
    [
        (200, UpstreamAnswer.ANSWERED),
        (429, UpstreamAnswer.RATE_LIMITED),
        (500, UpstreamAnswer.SERVER_ERROR),
        (503, UpstreamAnswer.SERVER_ERROR),
        (403, UpstreamAnswer.REFUSED),
        (urllib.error.URLError("connection reset"), UpstreamAnswer.UNREACHABLE),
        (TimeoutError("read timed out"), UpstreamAnswer.UNREACHABLE),
    ],
)
def test_probe_classifies_each_upstream_answer(answer: int | Exception, expected: UpstreamAnswer):
    assert probe_fork_upstream(URL, opener=_Opener(answer)) is expected


def _scripted(*answers: UpstreamAnswer):
    """A probe answering the scripted sequence, repeating its last answer."""
    queue = list(answers)

    def probe(_url: str, **_kwargs: object) -> UpstreamAnswer:
        return queue.pop(0) if len(queue) > 1 else queue[0]

    return probe


def _clock(*ticks: float):
    """A monotonic clock answering the scripted ticks, repeating the last one."""
    queue = list(ticks)

    def clock() -> float:
        return queue.pop(0) if len(queue) > 1 else queue[0]

    return clock


class _Spawn:
    """A spawn callable failing a scripted number of times, then answering."""

    def __init__(self, *, failures: int) -> None:
        self.calls = 0
        self._failures = failures

    def __call__(self) -> str:
        self.calls += 1
        if self.calls <= self._failures:
            msg = (
                "anvil spawn failed: timed out waiting for node to spawn; is the node binary "
                "installed? (fork target https://hosted.example/v1/key; a fork child that exits "
                "during startup is reported as a spawn timeout)"
            )
            raise RuntimeError(msg)
        return "fork"


def test_wait_for_upstream_backs_off_until_the_endpoint_answers():
    slept: list[float] = []
    answer = wait_for_upstream(
        URL,
        probe=_scripted(
            UpstreamAnswer.RATE_LIMITED,
            UpstreamAnswer.RATE_LIMITED,
            UpstreamAnswer.ANSWERED,
        ),
        sleep=slept.append,
    )
    assert answer is UpstreamAnswer.ANSWERED
    assert slept == [2.0, 4.0]


def test_wait_for_upstream_backoff_is_capped():
    slept: list[float] = []
    with pytest.raises(ForkUpstreamUnavailable):
        wait_for_upstream(
            URL,
            attempts=6,
            probe=_scripted(UpstreamAnswer.SERVER_ERROR),
            sleep=slept.append,
        )
    assert slept == [2.0, 4.0, 8.0, 16.0, 16.0]


def test_wait_for_upstream_names_the_endpoint_and_the_answer_it_exhausted():
    with pytest.raises(ForkUpstreamUnavailable) as raised:
        wait_for_upstream(
            URL,
            attempts=2,
            probe=_scripted(UpstreamAnswer.RATE_LIMITED),
            sleep=lambda _delay: None,
        )
    assert URL in str(raised.value)
    assert UpstreamAnswer.RATE_LIMITED.value in str(raised.value)


def test_spawn_retries_while_the_endpoint_reports_a_transient_answer():
    spawn = _Spawn(failures=1)
    slept: list[float] = []
    # Three probes: the pre-spawn wait, the evidence probe after the child died,
    # then the second attempt's pre-spawn wait.
    probe = _scripted(
        UpstreamAnswer.ANSWERED,
        UpstreamAnswer.RATE_LIMITED,
        UpstreamAnswer.ANSWERED,
    )
    assert (
        spawn_fork(spawn, fork_url=URL, probe=probe, sleep=slept.append, clock=_clock(0.0))
        == "fork"
    )
    assert spawn.calls == 2
    assert slept == [2.0]


def test_spawn_does_not_retry_when_the_endpoint_answers_after_the_failure():
    spawn = _Spawn(failures=5)
    with pytest.raises(RuntimeError):
        spawn_fork(
            spawn,
            fork_url=URL,
            probe=_scripted(UpstreamAnswer.ANSWERED),
            sleep=lambda _delay: None,
            clock=_clock(0.0),
        )
    assert spawn.calls == 1


def test_spawn_does_not_probe_a_non_http_target():
    spawn = _Spawn(failures=5)

    def probe(*_args: object, **_kwargs: object) -> UpstreamAnswer:
        pytest.fail("the liveness probe speaks HTTP; a ws fork target is not dialled")

    with pytest.raises(RuntimeError):
        spawn_fork(spawn, fork_url="ws://hosted.example/v1/key", probe=probe)
    assert spawn.calls == 1


def test_spawn_does_not_retry_an_in_memory_spawn():
    spawn = _Spawn(failures=5)

    def probe(*_args: object, **_kwargs: object) -> UpstreamAnswer:
        pytest.fail("an in-memory spawn has no upstream to probe")

    with pytest.raises(RuntimeError):
        spawn_fork(spawn, probe=probe)
    assert spawn.calls == 1


def test_spawn_does_not_retry_a_failure_that_spent_the_deadline():
    spawn = _Spawn(failures=5)
    with pytest.raises(RuntimeError):
        spawn_fork(
            spawn,
            fork_url=URL,
            probe=_scripted(UpstreamAnswer.ANSWERED, UpstreamAnswer.RATE_LIMITED),
            sleep=lambda _delay: None,
            clock=_clock(0.0, 120.0),
        )
    assert spawn.calls == 1
