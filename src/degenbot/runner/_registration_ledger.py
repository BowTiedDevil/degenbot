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

Four memos (the cold-soak negative-memoization set):

- registered paths: hop signatures already answered by a completed
  registration — the dup fast-path in front of the verify choreography.
- verified pools: pools whose verify lifecycle COMPLETED (a pool fact; the
  core's at-most-once verify claim dedups concurrent windows only).
- unregistrable pools: STABLE typed build refusals (a pool fact — no
  candidate path containing the pool can register); consulted at O(hops)
  before any build/verify.
- rejected paths: the path-composition policy gate deny / engine path-predicate deny,
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
from degenbot._ffi import (
    fold_registration_unit as _core_fold_registration_unit,
)
from degenbot._ffi import (
    registration_outcome_tags,
    registration_pool_memo_key,
    registration_unit_kinds,
)
from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HighFeePoolRejectedError,
    HookedPoolRejectedError,
)
from degenbot.exceptions.base import DegenbotValueError
from degenbot.pathfinding import PoolKind
from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    from degenbot._ffi import BuildRefusalView, RegistrationFoldDelta, UnregistrablePoolRecord

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

#: The per-unit outcome kinds, BUILT from the core's kind list.
#:
#: The unit-sequence contract (one outcome per unit, folded exactly once
#: through the core's counter fold) is the core's
#: (`degenbot_bot::bot_core::registration_ledger::RegistrationUnitOutcome`);
#: these members are minted FROM ``registration_unit_kinds()`` so a kind
#: added or renamed in the core shows up on the next build instead of
#: drifting into a Python-only spelling.
RegistrationUnitKind = StrEnum(
    "RegistrationUnitKind",
    [(kind.upper().replace("-", "_"), kind) for kind in registration_unit_kinds()],
    module=__name__,
    qualname="RegistrationUnitKind",
)


def fold_registration_unit(  # ruff: ignore[too-many-arguments] - mirrors the core fold seam 1:1
    *,
    kind: RegistrationUnitKind | str,
    tag: str | None,
    counts_as_skip: bool,
    created: bool,
    v4_hops: int,
    detail: str | None,
) -> RegistrationFoldDelta:
    """Fold one unit outcome with the CORE's counter arithmetic.

    The one outcome-counter fold lives in
    ``degenbot_bot::bot_core::registration_ledger::PipelineReport::absorb``;
    this forwards the driver's unit fields and returns the fold delta the
    driver applies to its own counter storage. An unknown kind or an
    untagged skip raises ``ValueError`` — the fold never guesses an outcome.

    Returns:
        The fold delta the driver applies to its own counter storage.

    """
    return _core_fold_registration_unit(str(kind), tag, counts_as_skip, created, v4_hops, detail)


#: The core failure kind each typed Python refusal maps to. The mapping is
#: Python-side because the exception TYPES are; the taxonomy those kinds
#: produce (outcome tag, stable-vs-transient, skip accounting) is the core's.
_FAILURE_KINDS: tuple[tuple[type[BaseException], str], ...] = (
    (HookedPoolRejectedError, "hooked-pool"),
    (DynamicFeePoolRejectedError, "dynamic-fee"),
    (HighFeePoolRejectedError, "high-fee"),
)


def _core_pool_kind_label(pool_kind: PoolKind) -> str:
    """Spell the pool-family label the core's memo/refusal seam takes.

    The typed `PoolKind` is what a hop carries; the core seam takes the
    family label string. One home for that conversion, matched on the enum:
    Python cannot exhaustiveness-check a PyO3 enum, so the unmatched arm
    raises instead of guessing a family.

    Returns:
        The core's family label for the kind.

    Raises:
        DegenbotValueError: On a kind outside the closed family set, naming
            the raw value and the known set — Rust/Python wire drift, never
            a silently guessed family.

    """
    match pool_kind:
        case PoolKind.V2:
            return "V2"
        case PoolKind.V3:
            return "V3"
        case PoolKind.V4:
            return "V4"
        case _:
            msg = (
                f"Unrecognized pool kind {pool_kind!r}: the registration seam "
                "spells only V2, V3, V4 (PoolKind members) — Rust/Python "
                "wire drift."
            )
            raise DegenbotValueError(message=msg)


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
    def pool_memo_key(step: Any, pool_kind: PoolKind) -> str | None:
        """Hop identity the negative memos key on — known BEFORE any build.

        V2/V3 key off the subgraph address; V4 off the pool id (the DB edge
        carries it pre-build, so a refused pool is recognizable without an
        RPC). ``None`` = not memoizable (no identity on this step) — an
        unrecognized family is never that answer: the kind leaves through
        the closed-set gate first, which raises `DegenbotValueError` naming
        the raw value and the known set.

        Returns:
            The memo key, or ``None`` when the step carries no memoizable
            identity.

        """
        label = _core_pool_kind_label(pool_kind)
        if pool_kind == PoolKind.V4:
            if not step.hash:
                return None
            return registration_pool_memo_key(label, None, to_0x_hex(step.hash))
        if not step.address:
            return None
        return registration_pool_memo_key(label, str(step.address).lower(), None)

    # ── typed build-refusal classification ──

    @staticmethod
    def classify_build_refusal(exc: BaseException, *, pool_kind: PoolKind) -> BuildRefusalView:
        """Classify a hop-build exception by TYPE — never by class name.

        ``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError`` are the
        V4 admission refusals; ``HighFeePoolRejectedError`` is a stable pool
        fact for every family. Any other exception is transient (retryable)
        even when its class name happens to match a stable refusal's. The
        detail text rides the record for logging and never becomes a label.
        An unrecognized family is never classified as some other family: the
        kind leaves through the closed-set gate first, which raises
        `DegenbotValueError` naming the raw value and the known set.

        Returns:
            The typed refusal view (kind label + detail text).

        """
        failure_kind = "transient"
        for exc_type, kind in _FAILURE_KINDS:
            if isinstance(exc, exc_type):
                failure_kind = kind
                break
        return _core_classify_build_refusal(
            failure_kind,
            _core_pool_kind_label(pool_kind),
            f"{type(exc).__name__}: {exc}",
        )

    # ── registered-path memo ──

    def path_registered(self, hop_sig: HopSignature) -> bool:
        """Return True when this exact hop signature already completed registration.

        Returns:
            True when the hop signature is in the registered-path memo.

        """
        return self._core.path_registered(hop_sig)

    def memoize_registered_path(self, hop_sig: HopSignature) -> None:
        """Record a completed registration (engine-created or engine-dedup'd)."""
        self._core.memoize_registered_path(hop_sig)

    # ── verify-once pool memo ──

    def pool_verified(self, key: str) -> bool:
        """Return True when this pool's verify lifecycle already completed.

        Returns:
            True when the pool key is in the verify-once memo.

        """
        return self._core.pool_verified(key)

    def memoize_verified_pool(self, key: str) -> None:
        """Record a COMPLETED verify lifecycle (a pool fact)."""
        self._core.memoize_verified_pool(key)

    # ── unregistrable-pool memo ──

    def unregistrable_record(self, key: str | None) -> UnregistrablePoolRecord | None:
        """Return the memoized stable refusal for a pool key, or None.

        Returns:
            The stable refusal record, or ``None`` when the key has none.

        """
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
        """Return True when this hop signature already hit a deterministic deny.

        Returns:
            True when the hop signature is in the rejected-path memo.

        """
        return self._core.path_rejected(hop_sig)

    def memoize_rejected_path(self, hop_sig: HopSignature) -> None:
        """Record a deterministic policy/predicate deny for a hop signature."""
        self._core.memoize_rejected_path(hop_sig)
