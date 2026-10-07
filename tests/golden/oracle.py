"""Golden-value record/replay seam for on-chain oracle parity tests (L2).

See ``docs/architecture/golden-onchain-parity.md`` for the full design. In
short: on-chain parity tests assert ``local_calc == on_chain_result``. The
``GoldenOracle`` records the on-chain result once (record mode, against a pinned
fork) into a human-readable JSON file of plain ints, then replays that int at CI
time (replay mode) with **no RPC** and **no secrets**.

The deferred oracle callable is only invoked in record mode; in replay mode the
recorded value is returned directly and the callable is never called. Both modes
preserve the test's own ``try/except … continue`` revert handling: a revert
captured at record time is stored as a ``{"reverted": true, …}`` entry and
replayed as a :class:`GoldenResult` with ``reverted=True``, so the test's skip
path reproduces exactly.
"""

from __future__ import annotations

import json
import socket
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

from tests.golden.record_errors import (
    CANONICAL_REVERT_EXCEPTION,
    call_with_transport_retry,
    canonical_revert_reason,
    classify_record_failure,
)

if TYPE_CHECKING:
    from collections.abc import Callable, Iterator

# Root for L2 golden files: tests/golden/data/<module path>/<TestName>.json
GOLDEN_ROOT = Path(__file__).resolve().parent / "data"

_CHAIN_MISMATCH_MSG = "golden file {path} recorded for chain_id {recorded} but test binds {bound}"
_BLOCK_MISMATCH_MSG = (
    "golden file {path} recorded at block {recorded} but test binds {bound}; "
    "re-record with --golden-mode=record"
)
_MISSING_KEY_MSG = (
    "no golden entry for key {key!r} in {path}; "
    "re-record with --golden-mode=record against a fork pinned to block {block}"
)


class GoldenError(AssertionError):
    """Raised when a golden entry is missing or the harness is mis-used."""


@dataclass(frozen=True)
class GoldenResult:
    """The recorded (or freshly-captured) on-chain oracle result for one key.

    ``value`` is the raw return value (typically an ``int``) when the contract
    call returned normally; ``None`` when it reverted. Tests should branch on
    ``reverted`` *before* reading ``value`` — mirroring the ``try/except`` they
    had around the live call — so reverts reproduce the same skip behaviour at
    replay time.
    """

    value: object
    reverted: bool
    exception_type: str | None
    message: str | None

    @property
    def ok(self) -> bool:
        """True iff the oracle call returned a value (did not revert)."""
        return not self.reverted


def _entry_to_json(result: GoldenResult) -> dict:
    if result.reverted:
        entry: dict = {
            "reverted": True,
            "exception": result.exception_type,
        }
        if result.message:
            entry["message"] = result.message
        return entry
    return {"value": result.value}


def _entry_from_json(entry: dict) -> GoldenResult:
    if entry.get("reverted"):
        return GoldenResult(
            value=None,
            reverted=True,
            exception_type=entry.get("exception"),
            message=entry.get("message"),
        )
    return GoldenResult(
        value=entry["value"],
        reverted=False,
        exception_type=None,
        message=None,
    )


class GoldenOracle:
    """Record/replay oracle for one test function's on-chain parity assertions.

    One JSON file per test function, path derived from the pytest nodeid so the
    file location mirrors the test location. Pass ``chain_id`` and
    ``block_number`` when binding: they are written to the file header and, on
    replay, asserted to match — this catches the classic mistake of re-recording
    at a different block than the golden file pins.
    """

    def __init__(
        self,
        *,
        path: Path,
        chain_id: int,
        block_number: int,
        mode: str,
    ) -> None:
        self._path = path
        self._chain_id = chain_id
        self._block_number = block_number
        self._recording = mode == "record"
        self._data = self._load_or_init()

    # -- file plumbing -----------------------------------------------------

    def _load_or_init(self) -> dict:
        if not self._path.exists():
            return {
                "chain_id": self._chain_id,
                "block_number": self._block_number,
                "recorded_at": None,
                "entries": {},
            }
        data = json.loads(self._path.read_text(encoding="utf-8"))
        # Replay-time sanity: the golden file must have been recorded at the
        # block the test claims. A mismatch means the test was re-pinned but
        # not re-recorded (stale goldens).
        if data.get("chain_id") != self._chain_id:
            raise GoldenError(
                _CHAIN_MISMATCH_MSG.format(
                    path=self._path,
                    recorded=data.get("chain_id"),
                    bound=self._chain_id,
                ),
            )
        if data.get("block_number") != self._block_number:
            raise GoldenError(
                _BLOCK_MISMATCH_MSG.format(
                    path=self._path,
                    recorded=data.get("block_number"),
                    bound=self._block_number,
                ),
            )
        return data

    def _flush(self) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True)
        payload = dict(self._data)
        payload["entries"] = dict(sorted(payload["entries"].items()))
        payload["recorded_at"] = datetime.now(UTC).isoformat(timespec="seconds")
        payload["chain_id"] = self._chain_id
        payload["block_number"] = self._block_number
        self._path.write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )

    # -- public API --------------------------------------------------------

    @property
    def path(self) -> Path:
        return self._path

    @property
    def is_recording(self) -> bool:
        return self._recording

    def check(
        self,
        key: str,
        *,
        contract: Callable[[], object],
    ) -> GoldenResult:
        """Return the on-chain oracle result for ``key``.

        - **Record mode:** invoke ``contract()``, classify the outcome, persist
          the entry, and return it. A revert persists its canonical entry (see
          :mod:`tests.golden.record_errors`); a transport failure is retried a
          bounded number of times and then fails the run loudly; any other
          error fails immediately. The test's own ``assert local ==
          result.value`` therefore validates against live chain state at record
          time.
        - **Replay mode:** return the recorded entry **without** calling
          ``contract()``. A missing key raises :class:`GoldenError` telling you
          to re-record.

        ``contract`` is always required: in replay it is documentation of which
        on-chain call the recorded int came from; in record it is the call
        itself.
        """
        entries: dict = self._data["entries"]

        if self._recording:
            try:
                value = call_with_transport_retry(contract)
            except Exception as exc:
                # Transport weather (retries exhausted above) and programming
                # errors fail the record run loudly — neither may become an
                # entry. Only the on-chain revert is golden-worthy.
                if classify_record_failure(exc) != "contract-revert":
                    raise
                result = GoldenResult(
                    value=None,
                    reverted=True,
                    exception_type=CANONICAL_REVERT_EXCEPTION,
                    message=canonical_revert_reason(getattr(exc, "message", None) or str(exc)),
                )
            else:
                result = GoldenResult(
                    value=value,
                    reverted=False,
                    exception_type=None,
                    message=None,
                )
            entries[key] = _entry_to_json(result)
            self._flush()  # incremental: a partial run still persists.
            return result

        if key not in entries:
            raise GoldenError(
                _MISSING_KEY_MSG.format(
                    key=key,
                    path=self._path,
                    block=self._block_number,
                ),
            )
        return _entry_from_json(entries[key])

    def keys(self) -> list[str]:
        """All recorded keys (deterministic, sorted)."""
        return sorted(self._data["entries"])


def _nodeid_to_path(nodeid: str, root: Path = GOLDEN_ROOT) -> Path:
    """Map a pytest nodeid to a per-test golden file path.

    ``tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py::test_cached_calculations``
    → ``tests/golden/data/uniswap/v3/test_uniswap_v3_liquidity_pool/test_cached_calculations.json``

    Parametrized tests carry a ``[param]`` suffix in the nodeid; it is dropped
    from the *path* (one file per test function) and the caller is expected to
    fold the param into its ``key``.
    """
    file_part, sep, test_part = nodeid.partition("::")
    if not sep:
        # nodeid without a test id (e.g. a module) — fall back to the stem.
        test_part = "module"
    # strip the .py extension
    file_part = file_part.removesuffix(".py")
    # drop a trailing pytest param bracket from the test name
    test_name = test_part.split("[", 1)[0] or "test"
    # normalise OS path separators
    rel = file_part.replace("\\", "/")
    # the nodeid's file part is relative to the rootdir (e.g. "tests/...")
    return root / rel / f"{test_name}.json"


# -- shared offline-contract test API -----------------------------------------
#
# The on-chain parity suites replay with no RPC at all: any socket dial during
# a replay run is a defect, the golden file a module diffs is resolved the same
# way ``golden_factory`` resolves its oracle file, and each module's replay
# surface must equal its golden file's key set exactly. Every parity module
# imports these names into its own namespace (fixture discovery is by module
# namespace; no non-root ``pytest_plugins`` registration).

REPLAY_DIAL_MSG = "golden replay is offline by contract; a network dial is a defect"

RECORD_MODE_KEY_SET_SKIP_REASON = "the record run rewrites the golden file this test diffs"


def refuse_connection(*_args: object, **_kwargs: object) -> None:
    """Dial-block target: any connection attempt fails the replay run."""
    raise AssertionError(REPLAY_DIAL_MSG)


def parity_golden_file(request: pytest.FixtureRequest, test_name: str) -> Path:
    """The parity test's golden file, resolved like golden_factory (--golden-root aware)."""
    file_part = Path(request.path).relative_to(request.config.rootpath).as_posix()
    nodeid = f"{file_part}::{test_name}"
    rel = _nodeid_to_path(nodeid, GOLDEN_ROOT).relative_to(GOLDEN_ROOT)
    return Path(request.config.getoption("--golden-root")) / rel


@pytest.fixture(autouse=True)
def replay_makes_no_network_calls(
    request: pytest.FixtureRequest,
    monkeypatch: pytest.MonkeyPatch,
) -> Iterator[None]:
    """Arm a hard dial-block in replay; only --golden-mode=record may touch a node.

    Replay asserts recorded ints against pools built I/O-free, so any connection
    attempt means the offline contract broke — failing at the dial beats hanging
    on an unreachable endpoint. Record mode is the one sanctioned dialer (a fork
    pinned to the recorded block), so the block is armed only for replay.
    """
    if request.config.getoption("--golden-mode") == "record":
        yield
        return
    monkeypatch.setattr(socket, "create_connection", refuse_connection)
    monkeypatch.setattr(socket, "getaddrinfo", refuse_connection)
    monkeypatch.setattr(socket.socket, "connect", refuse_connection)
    monkeypatch.setattr(socket.socket, "connect_ex", refuse_connection)
    yield


def assert_golden_keys_exact(
    request: pytest.FixtureRequest,
    test_name: str,
    expected: set[str],
) -> None:
    """The golden file holds exactly the keys the module's replay drives.

    Replay fails loud on a missing key but stays silent on a stale extra one —
    nothing looks it up. Set-equality against the recorded file closes that
    drift: a shrunken case list cannot leave orphaned oracle entries behind.
    Skips in record mode, where the run rewrites the very file being diffed.
    """
    if request.config.getoption("--golden-mode") == "record":
        pytest.skip(RECORD_MODE_KEY_SET_SKIP_REASON)
    recorded = set(json.loads(parity_golden_file(request, test_name).read_text())["entries"])
    assert recorded == expected, (
        f"stale={sorted(recorded - expected)} missing={sorted(expected - recorded)}"
    )
