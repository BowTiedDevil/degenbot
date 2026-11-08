"""Typed decode of the submit-leaf records crossing dispatch_and_submit.

The Rust SubmitRecord enum (degenbot-submission::submit) is the source of
truth: Submitted { candidate_id, tx_hash, nonce } or Skipped { candidate_id,
reason }. Its pyfunction wire flattens those to dicts keyed by a "kind" string,
a nested "candidate" mapping, and a "reason" string for skips. This module is
the single home for decoding that wire into typed records: an unrecognized
kind/reason or missing payload keys is Rust/Python wire drift and RAISES — the
driver can no longer silently drop a submission event inside a string "if"
chain.

The candidate identity is ONE value, not two independent keys: a record names
the solver path AND the deployment that executed it, so two deployments that
simulate the same path stay distinguishable.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING, Any

from degenbot.exceptions.base import DegenbotValueError

if TYPE_CHECKING:
    from collections.abc import Mapping

    from degenbot._ffi.simulation import CandidateId


class SubmitSkipReason(StrEnum):
    """The typed SkipReason vocabulary of the Rust submit leaf."""

    POOLS_CLAIMED = "pools_claimed"
    DRY_RUN = "dry_run"
    INJECT_CODE = "inject_code"
    BROADCAST_FAILED = "broadcast_failed"
    # The batch could not obtain a nonce, so the candidate was blocked rather
    # than decided: it remains a retry candidate for its strategy.
    NONCE_CONTENDED = "nonce_contended"


def candidate_id_from_mapping(raw: Mapping[str, Any], /) -> CandidateId:
    """Decode one record's nested ``candidate`` mapping.

    Returns:
        The composite identity the record carries.

    Raises:
        DegenbotValueError: When the mapping is absent or lacks a half — the
            retired path-only wire shape, or wire drift. Both are loud.

    """
    # The domain barrier owns the FFI alias (a private `_ffi` import does not
    # belong in a leaf module); importing it here keeps this module free of a
    # package-initialization cycle.
    from degenbot.dispatch import CandidateId  # ruff: ignore[import-outside-top-level]

    try:
        return CandidateId(
            path_id=int(raw["path_id"]),
            deployment_id=int(raw["deployment_id"]),
        )
    except KeyError as e:
        raise DegenbotValueError(
            message=f"Candidate identity is missing {e.args[0]!r}: {raw!r}",
        ) from e


@dataclass(frozen=True, slots=True)
class SubmittedRecord:
    """The tx was broadcast — the resulting tx_hash + claimed nonce."""

    candidate: CandidateId
    tx_hash: str
    nonce: int

    @property
    def path_id(self) -> int:
        """The candidate's solver path (the pre-target half)."""
        return self.candidate.path_id


@dataclass(frozen=True, slots=True)
class SkippedRecord:
    """The candidate was skipped — detail set for BROADCAST_FAILED."""

    candidate: CandidateId
    reason: SubmitSkipReason
    detail: str | None = None

    @property
    def path_id(self) -> int:
        """The candidate's solver path (the pre-target half)."""
        return self.candidate.path_id


SubmitRecord = SubmittedRecord | SkippedRecord


def typed_submit_record(raw: Mapping[str, Any], /) -> SubmitRecord:
    """Decode one FFI submit-record dict into a typed record.

    Returns:
        The typed record.

    Raises:
        DegenbotValueError: On an unrecognized or missing "kind", an unknown
            skip "reason", or missing payload keys — wire drift, loud.

    """
    kind = raw.get("kind")
    # The composite identity is required on EVERY record kind: a wire dict that
    # carries a bare path id is the retired shape and cannot be joined back to a
    # deployment, so it is refused here rather than guessed at.
    try:
        candidate = candidate_id_from_mapping(raw["candidate"])
    except KeyError as e:
        raise DegenbotValueError(
            message=f"Unrecognized submit record (missing candidate): {raw!r}",
        ) from e

    if kind == "submitted":
        try:
            return SubmittedRecord(
                candidate=candidate,
                tx_hash=raw["tx_hash"],
                nonce=raw["nonce"],
            )
        except KeyError as e:
            raise DegenbotValueError(
                message=f"Unrecognized submit record (submitted payload): {raw!r}",
            ) from e

    if kind == "skipped":
        try:
            reason = SubmitSkipReason(raw["reason"])
        except KeyError as e:
            raise DegenbotValueError(
                message=f"Unrecognized submit record (skipped payload): {raw!r}",
            ) from e
        except ValueError as e:
            raise DegenbotValueError(
                message=f"Unrecognized submit record reason: {raw.get('reason')!r}",
            ) from e
        return SkippedRecord(candidate=candidate, reason=reason, detail=raw.get("detail"))

    raise DegenbotValueError(message=f"Unrecognized submit record kind: {kind!r}")
