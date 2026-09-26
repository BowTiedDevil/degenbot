"""The cockpit registration ledger — a thin adapter over the Rust core.

The ledger that owns the ONE memo concept behind
``PathRegistrationPipeline._registration_unit`` now lives in the Rust core
(``degenbot_bot::bot_core::registration_ledger``): the four negative memos,
the typed stable-vs-transient build-refusal classification, and the bounded
outcome vocabulary the metric tag path may use. The pure-Rust driver and this
module read the SAME implementation, so a tag or a memo rule cannot drift
between them.

What stays here is only the translation Python owns:

* :class:`RegistrationOutcome` is *built from* the core's exported tag list
  rather than re-declared, so the closed set is the core's;
* :meth:`RegistrationLedger.pool_memo_key` and
  :meth:`RegistrationLedger.classify_build_refusal` are STATIC: a Python
  caller holds a discovery edge object and a Python exception, and mapping
  those onto the core's typed inputs is the adapter's job — the core decides
  the outcome, stability, and skip accounting that follow.

Four memos (the cold-soak negative-memoization set, W73FVY follow-up):

- registered paths: hop signatures already answered by a completed
  registration — the dup fast-path in front of the verify choreography.
- verified pools: pools whose verify lifecycle COMPLETED (a pool fact; the
  core's at-most-once verify claim dedups concurrent windows only).
- unregistrable pools: STABLE typed build refusals (a pool fact — no
  candidate path containing the pool can register); consulted at O(hops)
  before any build/verify.
- rejected paths: the D7KMQO policy gate deny / engine path-predicate deny,
  deterministic per hop signature.

TRANSIENT build/register failures are deliberately never memoized (a raced
build or RPC blip must stay retryable).
"""

from __future__ import annotations

from enum import StrEnum
from typing import TYPE_CHECKING, Any

from degenbot._ffi import RegistrationLedger as _CoreRegistrationLedger
from degenbot._ffi import (
    classify_build_refusal as _core_classify_build_refusal,
)
from degenbot._ffi import registration_outcome_tags, registration_pool_memo_key
from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HighFeePoolRejectedError,
    HookedPoolRejectedError,
)
from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    from degenbot._ffi import BuildRefusalView, UnregistrablePoolRecord

#: A path's hop signature: tuple of ``(engine pool_id, zero_for_one)``.
HopSignature = tuple[tuple[int, bool], ...]


#: The bounded metric-label vocabulary, BUILT from the core's tag list.
#:
#: Every tag the ledger hands the metric/skip path is one of the core's
#: ``RegistrationOutcome`` values, so the skip-reason and admission counters
#: stay a closed set. The exception class and message ride the record's
#: ``detail`` (log-only, first few occurrences) instead of the label, keeping
#: cardinality bounded. The members are minted FROM
#: ``registration_outcome_tags()`` rather than written out here, so a tag
#: added or renamed in the core shows up on the next build instead of being
#: silently missing from a Python counter.
RegistrationOutcome = StrEnum(
    "RegistrationOutcome",
    [(tag.upper().replace("-", "_"), tag) for tag in registration_outcome_tags()],
    module=__name__,
    qualname="RegistrationOutcome",
)

#: The core failure kind each typed Python refusal maps to. The mapping is
#: Python-side because the exception TYPES are; the taxonomy those kinds
#: produce (outcome tag, stable-vs-transient, skip accounting) is the core's.
_FAILURE_KINDS: tuple[tuple[type[BaseException], str], ...] = (
    (HookedPoolRejectedError, "hooked-pool"),
    (DynamicFeePoolRejectedError, "dynamic-fee"),
    (HighFeePoolRejectedError, "high-fee"),
)


class RegistrationLedger:
    """The four registration memos + typed build-refusal classification.

    Thin adapter: every instance method forwards to the core ledger held in
    `_core`, and the two static classification asks translate Python inputs
    into the core's typed vocabulary. No memo, tag, or rule is defined here.
    """

    def __init__(self) -> None:
        self._core = _CoreRegistrationLedger()

    # ── hop identity ──

    @staticmethod
    def pool_memo_key(step: Any, pool_type: str) -> str | None:
        """Hop identity the negative memos key on — known BEFORE any build.

        V2/V3 key off the subgraph address; V4 off the pool id (the DB edge
        carries it pre-build, so a refused pool is recognizable without an
        RPC). ``None`` = not memoizable (no identity on this step).
        """
        if pool_type == "V4":
            if not step.hash:
                return None
            return registration_pool_memo_key(pool_type, None, to_0x_hex(step.hash))
        if pool_type in {"V2", "V3"} and step.address:
            return registration_pool_memo_key(pool_type, str(step.address).lower(), None)
        return None

    # ── typed build-refusal classification ──

    @staticmethod
    def classify_build_refusal(exc: BaseException, *, pool_type: str) -> BuildRefusalView:
        """Classify a hop-build exception by TYPE — never by class name.

        ``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError`` are the
        V4 admission refusals; ``HighFeePoolRejectedError`` is a stable pool
        fact for every family. Any other exception is transient (retryable)
        even when its class name happens to match a stable refusal's. The
        detail text rides the record for logging and never becomes a label.
        """
        failure_kind = "transient"
        for exc_type, kind in _FAILURE_KINDS:
            if isinstance(exc, exc_type):
                failure_kind = kind
                break
        return _core_classify_build_refusal(
            failure_kind,
            pool_type,
            f"{type(exc).__name__}: {exc}",
        )

    # ── registered-path memo ──

    def path_registered(self, hop_sig: HopSignature) -> bool:
        """True when this exact hop signature already completed registration."""
        return self._core.path_registered(hop_sig)

    def memoize_registered_path(self, hop_sig: HopSignature) -> None:
        """Record a completed registration (engine-created or engine-dedup'd)."""
        self._core.memoize_registered_path(hop_sig)

    # ── verify-once pool memo ──

    def pool_verified(self, key: str) -> bool:
        """True when this pool's verify lifecycle already completed."""
        return self._core.pool_verified(key)

    def memoize_verified_pool(self, key: str) -> None:
        """Record a COMPLETED verify lifecycle (a pool fact)."""
        self._core.memoize_verified_pool(key)

    # ── unregistrable-pool memo ──

    def unregistrable_record(self, key: str | None) -> UnregistrablePoolRecord | None:
        """The memoized stable refusal for a pool key, or None."""
        return self._core.unregistrable_record(key)

    def memoize_unregistrable(
        self,
        key: str | None,
        outcome: RegistrationOutcome | str,
        *,
        counts_as_skip: bool,
    ) -> None:
        """Record a STABLE build refusal (setdefault — first tag wins)."""
        self._core.memoize_unregistrable(key, str(outcome), counts_as_skip)

    # ── rejected-path memo ──

    def path_rejected(self, hop_sig: HopSignature) -> bool:
        """True when this hop signature already hit a deterministic deny."""
        return self._core.path_rejected(hop_sig)

    def memoize_rejected_path(self, hop_sig: HopSignature) -> None:
        """Record a deterministic policy/predicate deny for a hop signature."""
        self._core.memoize_rejected_path(hop_sig)
