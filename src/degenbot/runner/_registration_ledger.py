"""The cockpit-private registration outcome ledger.

Owns the ONE memo concept behind ``PathRegistrationPipeline._registration_unit``:
the hop-identity key derivation, the typed stable-vs-transient classification
of build refusals (matching the real exception TYPES — never
``type(exc).__name__`` strings), the four memo records below, and the bounded
outcome vocabulary the metric tag path may use.

The pipeline asks this ledger instead of carrying four ad-hoc collections, so
``_registration_unit`` is build/verify choreography plus ledger asks.

Four memos (the cold-soak negative-memoization set, W73FVY follow-up):

- ``_registered_paths``: hop signatures already answered by a completed
  registration — the dup fast-path in front of the verify choreography.
- ``_verified_pools``: pools whose verify lifecycle COMPLETED (a pool fact;
  the seat claims table dedups concurrent windows only).
- ``_unregistrable_pools``: STABLE typed build refusals (a pool fact — no
  candidate path containing the pool can register); consulted at O(hops)
  before any build/verify.
- ``_rejected_paths``: the D7KMQO policy gate deny / engine path-predicate
  deny, deterministic per hop signature.

TRANSIENT build/register failures are deliberately never memoized (a raced
build or RPC blip must stay retryable).
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import StrEnum
from typing import Any

from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HighFeePoolRejectedError,
    HookedPoolRejectedError,
)
from degenbot.utils.bytes import to_0x_hex

#: A path's hop signature: tuple of ``(engine pool_id, zero_for_one)``.
HopSignature = tuple[tuple[int, bool], ...]


class RegistrationOutcome(StrEnum):
    """Bounded metric-label vocabulary for registration outcomes.

    Every tag this ledger hands the metric/skip path is one of these values,
    so the skip-reason and admission counters stay a closed set. The
    exception class and message ride the outcome's ``detail`` (log-only,
    first few occurrences) instead of the label, keeping cardinality
    bounded.
    """

    REGISTERED = "registered"
    DUP = "dup"
    PATH_CAP = "path-cap"
    DIRECTION_MISMATCH = "direction-mismatch"
    UNKNOWN_POOL_TYPE = "unknown-pool-type"
    V4_NO_HASH = "v4-no-hash"
    V4_HOOK_REJECTED = "v4-hook-rejected"
    V4_DYNAMIC_FEE_REJECTED = "v4-dynamic-fee-rejected"
    PATH_REJECTED = "path-rejected-memo"
    BUILD_V2_REFUSED = "build-v2-refused"
    BUILD_V3_REFUSED = "build-v3-refused"
    BUILD_V4_REFUSED = "build-v4-refused"
    REGISTER_FAILED = "register-fail"


#: The bounded build-refusal tag for each pool family.
_BUILD_REFUSED: dict[str, RegistrationOutcome] = {
    "V2": RegistrationOutcome.BUILD_V2_REFUSED,
    "V3": RegistrationOutcome.BUILD_V3_REFUSED,
    "V4": RegistrationOutcome.BUILD_V4_REFUSED,
}


@dataclass(frozen=True)
class BuildRefusal:
    """Typed classification of a hop-build exception.

    ``stable`` is the pool-fact verdict: a stable refusal can never appear
    in a registrable path, so its pool identity is memoized; a transient
    failure stays retryable. ``counts_as_skip`` preserves the V4 admission
    counter parity (hook / dynamic-fee refusals carry their own counters and
    do not add to ``skip_count``).
    """

    outcome: RegistrationOutcome
    stable: bool
    counts_as_skip: bool
    detail: str | None = None


@dataclass(frozen=True)
class UnregistrableRecord:
    """The memoized refusal of one pool: bounded tag + skip accounting."""

    outcome: RegistrationOutcome
    counts_as_skip: bool


class RegistrationLedger:
    """The four registration memos + the typed build-refusal classification."""

    def __init__(self) -> None:
        self._registered_paths: set[HopSignature] = set()
        self._verified_pools: set[str] = set()
        self._unregistrable_pools: dict[str, UnregistrableRecord] = {}
        self._rejected_paths: set[HopSignature] = set()

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
            return f"v4id:{to_0x_hex(step.hash)}"
        if pool_type in {"V2", "V3"} and step.address:
            return f"p:{str(step.address).lower()}"
        return None

    # ── typed build-refusal classification ──

    @staticmethod
    def classify_build_refusal(exc: BaseException, *, pool_type: str) -> BuildRefusal:
        """Classify a hop-build exception by TYPE — never by class name.

        ``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError`` are the
        V4 admission refusals (their own counters, no ``skip_count``);
        ``HighFeePoolRejectedError`` is a stable pool fact for every family.
        Any other exception is transient (retryable) even when its class name
        happens to match a stable refusal's.
        """
        detail = f"{type(exc).__name__}: {exc}"
        if isinstance(exc, HookedPoolRejectedError):
            return BuildRefusal(
                outcome=RegistrationOutcome.V4_HOOK_REJECTED,
                stable=True,
                counts_as_skip=False,
                detail=detail,
            )
        if isinstance(exc, DynamicFeePoolRejectedError):
            return BuildRefusal(
                outcome=RegistrationOutcome.V4_DYNAMIC_FEE_REJECTED,
                stable=True,
                counts_as_skip=False,
                detail=detail,
            )
        outcome = _BUILD_REFUSED.get(pool_type, RegistrationOutcome.BUILD_V3_REFUSED)
        return BuildRefusal(
            outcome=outcome,
            stable=isinstance(exc, HighFeePoolRejectedError),
            counts_as_skip=True,
            detail=detail,
        )

    # ── registered-path memo ──

    def path_registered(self, hop_sig: HopSignature) -> bool:
        """True when this exact hop signature already completed registration."""
        return hop_sig in self._registered_paths

    def memoize_registered_path(self, hop_sig: HopSignature) -> None:
        """Record a completed registration (engine-created or engine-dedup'd)."""
        self._registered_paths.add(hop_sig)

    # ── verify-once pool memo ──

    def pool_verified(self, key: str) -> bool:
        """True when this pool's verify lifecycle already completed."""
        return key in self._verified_pools

    def memoize_verified_pool(self, key: str) -> None:
        """Record a COMPLETED verify lifecycle (a pool fact)."""
        self._verified_pools.add(key)

    # ── unregistrable-pool memo ──

    def unregistrable_record(self, key: str | None) -> UnregistrableRecord | None:
        """The memoized stable refusal for a pool key, or None."""
        if key is None:
            return None
        return self._unregistrable_pools.get(key)

    def memoize_unregistrable(
        self,
        key: str | None,
        outcome: RegistrationOutcome,
        *,
        counts_as_skip: bool,
    ) -> None:
        """Record a STABLE build refusal (setdefault — first tag wins)."""
        if key is not None:
            self._unregistrable_pools.setdefault(
                key,
                UnregistrableRecord(outcome=outcome, counts_as_skip=counts_as_skip),
            )

    # ── rejected-path memo ──

    def path_rejected(self, hop_sig: HopSignature) -> bool:
        """True when this hop signature already hit a deterministic deny."""
        return hop_sig in self._rejected_paths

    def memoize_rejected_path(self, hop_sig: HopSignature) -> None:
        """Record a deterministic policy/predicate deny for a hop signature."""
        self._rejected_paths.add(hop_sig)
