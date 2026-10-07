"""Record-mode failure taxonomy for the golden oracle's contract callables.

A record run (``--golden-mode=record``) drives live on-chain calls and persists
their outcomes into the golden JSON. A callable fails one of three disjoint
ways, and only one of them is golden-worthy:

- **Contract revert** — the on-chain outcome itself. The parsed revert
  reason/data is chain truth, so it persists as a canonical revert entry: the
  ``ContractLogicError`` class marker plus the reason/data, with the provider's
  message text stripped. The same on-chain outcome must record byte-identically
  regardless of which node served the fork, so provider formatting never
  enters the golden bytes.
- **Transport failure** — timeout, connection loss, rate limit, or a non-revert
  JSON-RPC error from the provider seam. Endpoint weather, not chain truth;
  persisted, it gold-plates a flaky node as a revert. Retried a bounded number
  of times with a small backoff, then the record run fails loudly with the
  exact error and no entry is written.
- **Programming error** — an ``AttributeError``, ``TypeError``, or ``KeyError``
  from helper code, a mis-bound callable, a chain mismatch. Fails immediately;
  a bug must never be laundered into a golden entry.

The revert shapes classified here mirror the provider seam's own vocabulary:
the Rust core maps an ``eth_call`` revert to the degenbot-owned
``ContractLogicError`` (raised at the FFI layer with the node's error text
embedded), while transport weather arrives as ``TimeoutError``,
``ConnectionError``, or a ``RuntimeError`` carrying a non-revert
``Provider error:`` display.
"""

from __future__ import annotations

import re
import time
from typing import TYPE_CHECKING

from degenbot.exceptions import AnvilError, ContractLogicError

if TYPE_CHECKING:
    from collections.abc import Callable
    from typing import Literal

    RecordFailureClass = Literal["contract-revert", "transport", "programmer"]

# Bounded retry for transport-class failures: attempt the call this many times,
# sleeping the backoff times the attempt number between tries, then surface the
# last error. Kept public so probe tests can tune them without reaching into
# call frames.
RECORD_RETRY_ATTEMPTS = 3
RECORD_RETRY_BACKOFF_SECONDS = 0.5

# The one exception-class marker a persisted revert entry may carry.
CANONICAL_REVERT_EXCEPTION = "ContractLogicError"

# Revert markers mirroring the provider adapter's own list
# (``degenbot.provider.alloy_errors``): the phrases a backend error carries
# when the failure is an on-chain revert rather than endpoint weather.
_REVERT_MARKERS = ("execution reverted", "reverted", "0x08c379a0", "0x4e487b71")

# ABI shapes decodable without the target contract's ABI: ``Error(string)`` and
# ``Panic(uint256)`` are the two canonical selectors every EVM revert may use.
_ERROR_STRING_SELECTOR = "08c379a0"
_PANIC_SELECTOR = "4e487b71"

_HEX_BLOB_RE = re.compile(r"0x[0-9a-fA-F]+")
_ETH_CALL_REVERT_PREFIX_RE = re.compile(r"^eth_call to \S+ reverted:\s*")
_NODE_CODE_PREFIX_RE = re.compile(r"^(?:error code|code) \d+\s*:\s*")
_REVERT_PHRASE_RE = re.compile(r"^execution reverted\s*(?::\s*)?")
_DATA_FIELD_RE = re.compile(r"data\s*[:=]\s*['\"]?(0x[0-9a-fA-F]+)")

# A repr()'d payload tuple — the shape the retired web3 recorder str()'d a
# custom error into (``('0x…', '0x…')``); the first blob is the revert data.
_TUPLE_ITEM = r"(?:'0x[0-9a-fA-F]+'|\"0x[0-9a-fA-F]+\")"
_TUPLE_REPR_RE = re.compile(rf"^\(\s*{_TUPLE_ITEM}(?:\s*,\s*{_TUPLE_ITEM})*\s*,?\s*\)$")


def is_revert_shape(exc: BaseException) -> bool:
    """Whether ``exc`` presents as an on-chain ``eth_call`` execution revert."""
    if isinstance(exc, ContractLogicError):
        return True
    if isinstance(exc, RuntimeError):
        message = str(exc).lower()
        return any(marker in message for marker in _REVERT_MARKERS)
    return False


def classify_record_failure(exc: BaseException) -> RecordFailureClass:
    """Classify a record-time callable failure.

    Returns ``"contract-revert"`` for on-chain reverts (persist as a canonical
    revert entry), ``"transport"`` for endpoint weather (retry, then fail the
    run loudly), and ``"programmer"`` for everything else (fail immediately).
    """
    if is_revert_shape(exc):
        return "contract-revert"
    if isinstance(exc, (TimeoutError, OSError, AnvilError)):
        return "transport"
    if isinstance(exc, RuntimeError) and str(exc).startswith("Provider error:"):
        return "transport"
    return "programmer"


def _canonical_from_data(hexdata: str) -> str:
    """Canonical form for one revert payload (``hexdata``: bare lowercase hex).

    ``Error(string)`` decodes to the revert string and ``Panic(uint256)`` to
    ``Panic(<code>)``; any other payload (a custom error whose name needs the
    target contract's ABI) stays the raw ``0x`` blob — chain truth either way.
    """
    if hexdata.startswith(_ERROR_STRING_SELECTOR) and len(hexdata) >= 8 + 128:
        try:
            data_start = 8 + int(hexdata[8 : 8 + 64], 16) * 2
            length = int(hexdata[data_start : data_start + 64], 16) * 2
            payload = hexdata[data_start + 64 : data_start + 64 + length]
            return bytes.fromhex(payload).decode("utf-8")
        except (ValueError, UnicodeDecodeError):
            pass  # malformed shape: fall through to the raw blob
    if hexdata.startswith(_PANIC_SELECTOR) and len(hexdata) == 8 + 64:
        return f"Panic({int(hexdata[8:], 16)})"
    return f"0x{hexdata}"


def canonical_revert_reason(message: str | None) -> str | None:
    """Parse the on-chain revert reason/data out of a provider's error text.

    Strips the provider seam's wrappers (the ``eth_call to … reverted:`` frame,
    a node code prefix, the ``execution reverted`` phrase) and canonicalizes
    what remains. Returns ``None`` when the text carries no reason or data, so
    the entry omits ``message`` entirely.
    """
    if not message:
        return None
    text = message.strip()
    text = _ETH_CALL_REVERT_PREFIX_RE.sub("", text, count=1).strip()
    text = _NODE_CODE_PREFIX_RE.sub("", text, count=1).strip()
    text = _REVERT_PHRASE_RE.sub("", text, count=1).strip()
    # A node echoing the JSON-RPC error's `data` field inline carries the
    # revert payload behind the message.
    data_field = _DATA_FIELD_RE.search(message)
    if data_field is not None:
        return _canonical_from_data(data_field.group(1)[2:].lower())
    if _TUPLE_REPR_RE.fullmatch(text):
        blob = _HEX_BLOB_RE.search(text)
        if blob is not None:
            return _canonical_from_data(blob.group(0)[2:].lower())
    if _HEX_BLOB_RE.fullmatch(text):
        return _canonical_from_data(text[2:].lower())
    return text or None


def canonical_revert_entry(exc: BaseException) -> dict:
    """The canonical golden entry for one contract revert.

    Carries the ``ContractLogicError`` class marker and the parsed revert
    reason/data only — never the provider's message text — so the same
    on-chain outcome persists byte-identically from any provider.
    """
    reason = canonical_revert_reason(getattr(exc, "message", None) or str(exc))
    entry: dict = {"reverted": True, "exception": CANONICAL_REVERT_EXCEPTION}
    if reason:
        entry["message"] = reason
    return entry


def call_with_transport_retry(
    call: Callable[[], object],
    *,
    attempts: int | None = None,
    backoff_seconds: float | None = None,
) -> object:
    """Invoke ``call``, retrying transport-class failures a bounded number of times.

    Contract reverts and programming errors propagate immediately (the caller
    classifies and either persists or fails). Transport failures sleep
    ``backoff_seconds * attempt`` between attempts; once exhausted, the exact
    last error is re-raised so the record run fails loudly with it and no
    entry is written.
    """
    tries = RECORD_RETRY_ATTEMPTS if attempts is None else attempts
    backoff = RECORD_RETRY_BACKOFF_SECONDS if backoff_seconds is None else backoff_seconds
    last: BaseException | None = None
    for attempt in range(1, tries + 1):
        try:
            return call()
        except Exception as exc:
            if classify_record_failure(exc) != "transport":
                raise
            last = exc
            if attempt < tries:
                time.sleep(backoff * attempt)
    if last is None:
        msg = "retry loop exhausted without a transport failure to re-raise"
        raise AssertionError(msg)
    raise last
