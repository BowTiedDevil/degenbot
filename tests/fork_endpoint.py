"""Bounded retry for the fork lane's upstream endpoint.

A forked ``anvil`` fetches its genesis state from the fork target *before* it
reports readiness, and alloy reads a child that dies during that fetch as
"timed out waiting for node to spawn": the readiness wait watches stdout for the
``Listening on`` line and a dying child produces empty lines instead. A hosted
endpoint's transient refusal therefore reaches the test as an opaque spawn
timeout — a free-tier budget answering HTTP 429 is indistinguishable from a
missing binary.

The refused request is anvil's, not the harness's, so no client-side retry can
re-issue it. Two things the harness can do, and this module does:

* wait for the endpoint to answer again *before* spawning — the probe is a
  plain ``eth_chainId`` liveness call, bounded, with exponential backoff;
* give the spawn one second chance when a failed spawn coincides with an
  unhealthy endpoint.

The classification is evidence, never the message text: the spawn error's own
``HTTP 429`` sentence is unconditional prose attached to every forked-spawn
failure, so a second probe decides whether the upstream or the fork itself
failed — an endpoint that answers while the spawn failed is the fork's problem
(bad flags, a missing block) and retrying would only hide it.

Scope is the test lane: production spawn failures stay loud and unretried.

Which endpoint the lane dials is the operator's declaration, not the harness's
judgement: ``tests.env`` (gitignored) or the process environment names it, and
``just test-fork`` fails closed unless the machine declared one. A 429 is then a
fact about that declaration, which the harness cannot second-guess — re-pointing
the lane at another endpoint is an operator edit. So the lane's own defence is
to wait the budget window out, and to say plainly when it cannot.
"""

from __future__ import annotations

import enum
import json
import time
import urllib.error
import urllib.request
from typing import TYPE_CHECKING, Any, Final

from degenbot.logging import logger

if TYPE_CHECKING:
    from collections.abc import Callable

#: Probe budget: five probes spanning ~30s of backoff rides out a per-minute
#: free-tier budget refill without approaching the suite's per-test deadline.
PROBE_ATTEMPTS: Final = 5
PROBE_BASE_DELAY_S: Final = 2.0
PROBE_MAX_DELAY_S: Final = 16.0
PROBE_TIMEOUT_S: Final = 10.0

#: Two spawn attempts, not more: each one may spend the whole fork spawn
#: deadline, and the suite's per-test budget is 300s.
SPAWN_ATTEMPTS: Final = 2
#: A failed spawn that spent longer than this already consumed the deadline, so
#: retrying it would only spend the test's budget twice. A fast death (a child
#: killed by a refused genesis fetch) is the shape worth a second chance.
SPAWN_RETRY_DEADLINE_S: Final = 60.0

#: The schemes the liveness probe can dial: it is a JSON-RPC POST, so a WS or
#: IPC fork target is spawned unwrapped rather than probed.
_PROBED_SCHEMES: Final = ("http://", "https://")

_LIVENESS_REQUEST: Final[dict[str, Any]] = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "eth_chainId",
    "params": [],
}


class UpstreamAnswer(enum.Enum):
    """How a fork target answered the lane's liveness probe."""

    ANSWERED = "answered"
    RATE_LIMITED = "answered HTTP 429 (per-IP budget spent)"
    SERVER_ERROR = "answered HTTP 5xx (transient upstream fault)"
    REFUSED = "answered a non-retryable HTTP status"
    UNREACHABLE = "did not answer (transport error or timeout)"


class ForkUpstreamUnavailable(RuntimeError):
    """The fork target never answered the probe within its bounded budget."""


def probe_fork_upstream(
    fork_url: str,
    *,
    timeout_s: float = PROBE_TIMEOUT_S,
    opener: Callable[..., Any] = urllib.request.urlopen,  # ruff: ignore[suspicious-url-open-usage] — scheme-guarded by _PROBED_SCHEMES
) -> UpstreamAnswer:
    """Ask the fork target for its chain id and classify the answer.

    Returns:
        The classified answer; only :attr:`UpstreamAnswer.ANSWERED` means the
        endpoint is serving.

    """
    request = urllib.request.Request(  # ruff: ignore[suspicious-url-open-usage] — scheme-guarded by _PROBED_SCHEMES
        fork_url,
        data=json.dumps(_LIVENESS_REQUEST).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with opener(request, timeout=timeout_s) as response:
            return _answer_for_status(response.status)
    except urllib.error.HTTPError as exc:
        # HTTPError is the response object itself: close it, or the
        # interpreter's ResourceWarning (fatal under the suite's
        # warnings-as-errors) fires when it is collected.
        status = exc.code
        exc.close()
        return _answer_for_status(status)
    except (urllib.error.URLError, OSError):
        return UpstreamAnswer.UNREACHABLE


def _answer_for_status(status: int | None) -> UpstreamAnswer:
    if status is None:
        return UpstreamAnswer.UNREACHABLE
    if 200 <= status < 300:  # the only status family that means "serving"
        return UpstreamAnswer.ANSWERED
    if status == 429:  # the free-tier budget refusal
        return UpstreamAnswer.RATE_LIMITED
    if 500 <= status < 600:  # a transient server fault
        return UpstreamAnswer.SERVER_ERROR
    return UpstreamAnswer.REFUSED


def _backoff_s(attempt: int) -> float:
    return min(PROBE_BASE_DELAY_S * 2.0 ** (attempt - 1), PROBE_MAX_DELAY_S)


def wait_for_upstream(
    fork_url: str,
    *,
    attempts: int = PROBE_ATTEMPTS,
    probe: Callable[..., UpstreamAnswer] = probe_fork_upstream,
    sleep: Callable[[float], None] = time.sleep,
) -> UpstreamAnswer:
    """Probe the fork target until it answers, backing off between probes.

    Returns:
        :attr:`UpstreamAnswer.ANSWERED`.

    Raises:
        ForkUpstreamUnavailable: The endpoint never answered within ``attempts``
            probes; the message names the endpoint and the answer it exhausted.

    """
    answer = probe(fork_url)
    attempt = 1
    while answer is not UpstreamAnswer.ANSWERED:
        if attempt >= attempts:
            msg = (
                f"fork upstream {fork_url} {answer.value} on all {attempts} probes "
                f"(backoff {PROBE_BASE_DELAY_S:g}s doubling, capped at {PROBE_MAX_DELAY_S:g}s): "
                "the endpoint's budget or availability is the failure, not the test"
            )
            raise ForkUpstreamUnavailable(msg)
        delay = _backoff_s(attempt)
        logger.warning(
            "fork upstream %s %s; probe %d/%d, retrying in %.1fs",
            fork_url,
            answer.value,
            attempt,
            attempts,
            delay,
        )
        sleep(delay)
        attempt += 1
        answer = probe(fork_url)
    return answer


def spawn_fork[T](
    spawn: Callable[[], T],
    *,
    fork_url: str | None = None,
    attempts: int = SPAWN_ATTEMPTS,
    probe: Callable[..., UpstreamAnswer] = probe_fork_upstream,
    sleep: Callable[[float], None] = time.sleep,
    clock: Callable[[], float] = time.monotonic,
) -> T:
    """Spawn a fork after the target answers, retrying a transient failure.

    A local or in-memory spawn (``fork_url is None``) has no upstream to wait
    for, and a non-HTTP target cannot be probed; neither is retried.

    Returns:
        Whatever ``spawn`` returns (the fork handle).

    Raises:
        ForkUpstreamUnavailable: The endpoint never answered the probe.
        RuntimeError: The spawn failed; re-raised unchanged when the retry
            budget, the deadline, or evidence of a healthy endpoint rules out an
            upstream cause.

    """
    target = fork_url if fork_url and fork_url.startswith(_PROBED_SCHEMES) else None
    attempt = 1
    while True:
        if target is not None:
            wait_for_upstream(target, probe=probe, sleep=sleep)
        started = clock()
        try:
            return spawn()
        except RuntimeError:
            elapsed = clock() - started
            if (
                target is None
                or attempt >= attempts
                or elapsed > SPAWN_RETRY_DEADLINE_S
                or probe(target) is UpstreamAnswer.ANSWERED
            ):
                raise
            delay = _backoff_s(attempt)
            logger.warning(
                "fork spawn failed while %s was unhealthy; attempt %d/%d, retrying in %.1fs",
                target,
                attempt,
                attempts,
                delay,
            )
            sleep(delay)
            attempt += 1
