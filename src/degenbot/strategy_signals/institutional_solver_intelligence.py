"""Institutional Solver Intelligence policy kernel.

This module models a deterministic, auditable control loop for proposal-level
policy updates. It classifies candidate adaptive decisions as
allow/review/block with explicit reasons, supports batched evaluation and
correction proposals, and persists governance artifacts for replay.
"""

from __future__ import annotations

import json
import sqlite3
from dataclasses import dataclass
from enum import StrEnum
from hashlib import sha256
from pathlib import Path
from time import time_ns
from typing import TYPE_CHECKING, Final

from degenbot.exceptions.base import DegenbotValueError

if TYPE_CHECKING:
    from collections.abc import Iterable


class InstitutionalSolverDecision(StrEnum):
    """Deterministic action for a candidate adaptive decision."""

    APPROVE = "approve"
    REVIEW = "review"
    BLOCK = "block"


@dataclass(frozen=True, slots=True)
class InstitutionalSolverResponsibilities:
    """Declared behavioral contract for the AI administrator."""

    protect_capital: str
    enforce_determinism: str
    strategic_awareness: str
    adversarial_resilience: str
    governance_discipline: str


@dataclass(frozen=True, slots=True)
class AdaptationCandidate:
    """A deterministic input to the adaptive policy gate."""

    candidate_id: str
    expected_profit_bps: int
    observed_drawdown_bps: int
    confidence_bps: int
    sample_count: int
    required_human_approval: bool = False
    requires_invariant_checks: bool = True
    policy_drift_score: int = 0


@dataclass(frozen=True, slots=True)
class PolicyObservation:
    """Observed outcome for a previously evaluated adaptive candidate."""

    observation_id: str
    candidate_id: str
    realized_profit_bps: int
    realized_drawdown_bps: int
    confidence_bps: int
    policy_drift_score: int = 0


@dataclass(frozen=True, slots=True)
class InstitutionalAction:
    """Result of the policy gate for one candidate."""

    candidate_id: str
    decision: InstitutionalSolverDecision
    policy_id: str
    controls: tuple[str, ...]
    rationale: str


@dataclass(frozen=True, slots=True)
class PolicyCorrectionProposal:
    """Auditable correction proposal emitted by the adaptive control loop."""

    proposal_id: str
    candidate_id: str
    decision: InstitutionalSolverDecision
    policy_id: str
    controls: tuple[str, ...]
    rationale: str
    proposed_min_profit_bps: int
    proposed_min_confidence_bps: int
    requires_human_review: bool


@dataclass(frozen=True, slots=True)
class InstitutionalBatchEvaluation:
    """Batch-level deterministic policy decision output."""

    batch_id: str
    policy_id: str
    created_at_ms: int
    actions: tuple[InstitutionalAction, ...]
    approved_count: int
    review_count: int
    blocked_count: int
    controls_digest: str


@dataclass(frozen=True, slots=True)
class PolicyCorrectionBatch:
    """Batch-level deterministic correction proposals."""

    batch_id: str
    policy_id: str
    created_at_ms: int
    proposals: tuple[PolicyCorrectionProposal, ...]
    approved_count: int
    review_count: int
    blocked_count: int
    controls_digest: str


@dataclass(frozen=True, slots=True)
class PolicySnapshot:
    """Lightweight immutable persisted artifact for governance replay."""

    snapshot_id: str
    batch_id: str
    policy_id: str
    created_at_ms: int
    candidate_count: int
    approved_count: int
    review_count: int
    blocked_count: int


class InstitutionalSolverIntelligence:
    """Deterministic, auditable control policy for self-improving behavior."""

    name: Final[str] = "Institutional Solver Intelligence"

    responsibilities = InstitutionalSolverResponsibilities(
        protect_capital=(
            "Protect capital first, avoid reckless risk and non-deterministic shortcuts."
        ),
        enforce_determinism=(
            "Prioritize correctness and deterministic replay over adaptive volatility."
        ),
        strategic_awareness=(
            "Adapt within approved policy bounds when infrastructure and risks justify it."
        ),
        adversarial_resilience=(
            "Assume hostile environments and reject control loops that create exploitability."
        ),
        governance_discipline=(
            "Hold to incident-ready governance discipline and explicit escalation conditions."
        ),
    )

    ai_principles: Final[tuple[str, ...]] = (
        "strategic",
        "disciplined",
        "mathematically_grounded",
        "precise",
        "emotionally_detached",
        "truth_aligned",
        "responsibility_minded",
        "no_assumptions",
    )

    safety_gates: Final[tuple[str, ...]] = (
        "fail closed on adverse confidence or sample conditions",
        "bounded drawdown and policy drift enforcement",
        "human approval path for high-impact adaptive changes",
        "invariant-preserving fallback when signals are ambiguous",
    )

    def __init__(
        self,
        *,
        max_drawdown_bps: int = 50,
        min_confidence_bps: int = 7000,
        min_sample_count: int = 30,
        min_profit_bps: int = 20,
        max_policy_drift_score: int = 40,
        policy_id: str = "adaptive_control_loop_v1",
        snapshot_db_path: str | Path = "/tmp/institutional_solver_intelligence.sqlite3",  # noqa: S108
    ) -> None:
        self._max_drawdown_bps = max_drawdown_bps
        self._min_confidence_bps = min_confidence_bps
        self._min_sample_count = min_sample_count
        self._min_profit_bps = min_profit_bps
        self._max_policy_drift_score = max_policy_drift_score
        self._policy_id = policy_id
        self._snapshot_db_path = Path(snapshot_db_path)
        self._snapshot_db_path.parent.mkdir(parents=True, exist_ok=True)
        self._initialize_snapshot_schema()

    @property
    def policy_id(self) -> str:
        return self._policy_id

    def evaluate(self, candidate: AdaptationCandidate) -> InstitutionalAction:
        """Evaluate a single candidate and return a deterministic decision."""

        if candidate.requires_invariant_checks is False:
            return InstitutionalAction(
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.BLOCK,
                policy_id=self._policy_id,
                controls=("invariant_validation", "replay_required"),
                rationale=(
                    f"candidate {candidate.candidate_id}: invariant checks are required "
                    "for adaptive reconfiguration"
                ),
            )

        if candidate.observed_drawdown_bps > self._max_drawdown_bps:
            return InstitutionalAction(
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.BLOCK,
                policy_id=self._policy_id,
                controls=("post_trade_drawdown_gate", "kill_switch_required"),
                rationale=(
                    f"candidate {candidate.candidate_id}: observed drawdown "
                    f"{candidate.observed_drawdown_bps} bps exceeds max {self._max_drawdown_bps} bps"
                ),
            )

        if candidate.sample_count < self._min_sample_count:
            return InstitutionalAction(
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.REVIEW,
                policy_id=self._policy_id,
                controls=("sample_reduction_gate", "review_required"),
                rationale=(
                    f"candidate {candidate.candidate_id}: insufficient samples "
                    f"{candidate.sample_count}<{self._min_sample_count} for stable adaptation"
                ),
            )

        if candidate.confidence_bps < self._min_confidence_bps:
            return InstitutionalAction(
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.REVIEW,
                policy_id=self._policy_id,
                controls=("confidence_gate", "review_required"),
                rationale=(
                    f"candidate {candidate.candidate_id}: confidence {candidate.confidence_bps} "
                    f"below policy floor {self._min_confidence_bps}"
                ),
            )

        if candidate.policy_drift_score > self._max_policy_drift_score:
            return InstitutionalAction(
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.REVIEW,
                policy_id=self._policy_id,
                controls=("drift_analysis", "governance_review"),
                rationale=(
                    f"candidate {candidate.candidate_id}: policy drift score "
                    f"{candidate.policy_drift_score} exceeds max {self._max_policy_drift_score}"
                ),
            )

        if candidate.expected_profit_bps < self._min_profit_bps:
            return InstitutionalAction(
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.REVIEW,
                policy_id=self._policy_id,
                controls=("min_profit_gate", "simulator_confirmation"),
                rationale=(
                    f"candidate {candidate.candidate_id}: expected profit "
                    f"{candidate.expected_profit_bps} bps below floor {self._min_profit_bps} bps"
                ),
            )

        if candidate.required_human_approval:
            return InstitutionalAction(
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.REVIEW,
                policy_id=self._policy_id,
                controls=("human_approval", "post_change_observability"),
                rationale=(f"candidate {candidate.candidate_id}: human approval required by governance"),
            )

        return InstitutionalAction(
            candidate_id=candidate.candidate_id,
            decision=InstitutionalSolverDecision.APPROVE,
            policy_id=self._policy_id,
            controls=("simulator_replay", "telemetry_logging"),
            rationale=(f"candidate {candidate.candidate_id}: all control gates passed"),
        )

    def evaluate_batch(
        self,
        batch_id: str,
        candidates: Iterable[AdaptationCandidate],
    ) -> InstitutionalBatchEvaluation:
        """Evaluate a deterministic batch and persist the result snapshot."""

        candidate_seq = tuple(candidates)
        if not candidate_seq:
            msg = "batch must contain at least one candidate"
            raise DegenbotValueError(msg)

        actions = tuple(self.evaluate(candidate) for candidate in candidate_seq)
        counts = self._count_decisions(actions)
        snapshot_id = self._snapshot_id(batch_id, created_at_ms=self._now_ms())
        controls_digest = self._controls_digest(action.controls for action in actions)
        created_at = self._now_ms()

        snapshot = InstitutionalBatchEvaluation(
            batch_id=batch_id,
            policy_id=self._policy_id,
            created_at_ms=created_at,
            actions=actions,
            approved_count=counts[InstitutionalSolverDecision.APPROVE],
            review_count=counts[InstitutionalSolverDecision.REVIEW],
            blocked_count=counts[InstitutionalSolverDecision.BLOCK],
            controls_digest=controls_digest,
        )
        self._persist_batch_snapshot(snapshot_id, snapshot)
        return snapshot

    def propose_batch_corrections(
        self,
        batch_id: str,
        candidates: Iterable[AdaptationCandidate],
        observations: Iterable[PolicyObservation],
    ) -> PolicyCorrectionBatch:
        """Evaluate observed outcomes and return deterministic correction proposals."""

        candidate_map = {candidate.candidate_id: candidate for candidate in tuple(candidates)}
        if not candidate_map:
            msg = "candidates batch must not be empty"
            raise DegenbotValueError(msg)

        proposals: list[PolicyCorrectionProposal] = []
        for observation in tuple(observations):
            candidate = candidate_map.get(observation.candidate_id)
            if candidate is None:
                msg = (
                    f"observation {observation.observation_id} references unknown candidate "
                    f"{observation.candidate_id!r}"
                )
                raise DegenbotValueError(msg)
            proposals.append(self.propose_policy_correction(candidate, observation))

        if not proposals:
            msg = "observations batch must not be empty"
            raise DegenbotValueError(msg)

        proposal_tuple = tuple(proposals)
        counts = self._count_decisions(action.decision for action in proposal_tuple)
        snapshot_id = self._snapshot_id(batch_id, created_at_ms=self._now_ms())
        controls_digest = self._controls_digest(proposal.controls for proposal in proposal_tuple)
        created_at = self._now_ms()

        snapshot = PolicyCorrectionBatch(
            batch_id=batch_id,
            policy_id=self._policy_id,
            created_at_ms=created_at,
            proposals=proposal_tuple,
            approved_count=counts[InstitutionalSolverDecision.APPROVE],
            review_count=counts[InstitutionalSolverDecision.REVIEW],
            blocked_count=counts[InstitutionalSolverDecision.BLOCK],
            controls_digest=controls_digest,
        )
        self._persist_proposal_snapshot(snapshot_id, snapshot)
        return snapshot

    def list_recent_snapshots(self, limit: int = 10) -> tuple[PolicySnapshot, ...]:
        if limit <= 0:
            return ()

        with sqlite3.connect(self._snapshot_db_path) as connection:
            rows = connection.execute(
                "SELECT snapshot_id, batch_id, policy_id, created_at_ms, candidate_count, approved_count, review_count, blocked_count "
                "FROM policy_snapshot_runs ORDER BY created_at_ms DESC LIMIT ?",
                (limit,),
            ).fetchall()
        return tuple(
            PolicySnapshot(
                snapshot_id=row[0],
                batch_id=row[1],
                policy_id=row[2],
                created_at_ms=row[3],
                candidate_count=row[4],
                approved_count=row[5],
                review_count=row[6],
                blocked_count=row[7],
            )
            for row in rows
        )

    def propose_policy_correction(
        self,
        candidate: AdaptationCandidate,
        observation: PolicyObservation,
    ) -> PolicyCorrectionProposal:
        """Convert observed outcomes into a deterministic correction proposal.

        This method never mutates live thresholds. Callers must route proposals
        through replay and governance before applying any policy changes.
        """

        if observation.candidate_id != candidate.candidate_id:
            msg = (
                "policy observation candidate_id must match adaptation candidate_id "
                f"({observation.candidate_id!r} != {candidate.candidate_id!r})"
            )
            raise DegenbotValueError(msg)

        proposal_id = f"{self._policy_id}:{candidate.candidate_id}:{observation.observation_id}"

        if observation.realized_drawdown_bps > self._max_drawdown_bps:
            return PolicyCorrectionProposal(
                proposal_id=proposal_id,
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.BLOCK,
                policy_id=self._policy_id,
                controls=("post_trade_drawdown_gate", "kill_switch_required", "human_review"),
                rationale=(
                    f"observation {observation.observation_id}: realized drawdown "
                    f"{observation.realized_drawdown_bps} bps exceeds max "
                    f"{self._max_drawdown_bps} bps"
                ),
                proposed_min_profit_bps=self._min_profit_bps,
                proposed_min_confidence_bps=self._min_confidence_bps,
                requires_human_review=True,
            )

        if observation.policy_drift_score > self._max_policy_drift_score:
            return PolicyCorrectionProposal(
                proposal_id=proposal_id,
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.REVIEW,
                policy_id=self._policy_id,
                controls=("drift_correction", "governance_review", "simulator_replay"),
                rationale=(
                    f"observation {observation.observation_id}: policy drift score "
                    f"{observation.policy_drift_score} exceeds max "
                    f"{self._max_policy_drift_score}"
                ),
                proposed_min_profit_bps=self._min_profit_bps,
                proposed_min_confidence_bps=self._min_confidence_bps,
                requires_human_review=True,
            )

        if observation.confidence_bps < self._min_confidence_bps:
            return PolicyCorrectionProposal(
                proposal_id=proposal_id,
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.REVIEW,
                policy_id=self._policy_id,
                controls=("confidence_correction", "sample_expansion", "simulator_replay"),
                rationale=(
                    f"observation {observation.observation_id}: confidence "
                    f"{observation.confidence_bps} below policy floor "
                    f"{self._min_confidence_bps}"
                ),
                proposed_min_profit_bps=self._min_profit_bps,
                proposed_min_confidence_bps=self._min_confidence_bps,
                requires_human_review=True,
            )

        if observation.realized_profit_bps < self._min_profit_bps:
            return PolicyCorrectionProposal(
                proposal_id=proposal_id,
                candidate_id=candidate.candidate_id,
                decision=InstitutionalSolverDecision.REVIEW,
                policy_id=self._policy_id,
                controls=("profit_correction", "simulator_replay", "shadow_execution"),
                rationale=(
                    f"observation {observation.observation_id}: realized profit "
                    f"{observation.realized_profit_bps} bps below floor "
                    f"{self._min_profit_bps} bps"
                ),
                proposed_min_profit_bps=max(self._min_profit_bps, candidate.expected_profit_bps),
                proposed_min_confidence_bps=max(self._min_confidence_bps, observation.confidence_bps),
                requires_human_review=True,
            )

        return PolicyCorrectionProposal(
            proposal_id=proposal_id,
            candidate_id=candidate.candidate_id,
            decision=InstitutionalSolverDecision.APPROVE,
            policy_id=self._policy_id,
            controls=("retain_policy", "telemetry_logging", "periodic_replay"),
            rationale=(
                f"observation {observation.observation_id}: realized outcome remains "
                "inside approved policy bounds"
            ),
            proposed_min_profit_bps=self._min_profit_bps,
            proposed_min_confidence_bps=self._min_confidence_bps,
            requires_human_review=False,
        )

    def _initialize_snapshot_schema(self) -> None:
        with sqlite3.connect(self._snapshot_db_path) as connection:
            connection.execute(
                """
                CREATE TABLE IF NOT EXISTS policy_snapshot_runs (
                    snapshot_id TEXT PRIMARY KEY,
                    batch_id TEXT NOT NULL,
                    policy_id TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    candidate_count INTEGER NOT NULL,
                    approved_count INTEGER NOT NULL,
                    review_count INTEGER NOT NULL,
                    blocked_count INTEGER NOT NULL,
                    controls_digest TEXT NOT NULL
                )
                """
            )
            connection.execute(
                """
                CREATE TABLE IF NOT EXISTS batch_action_snapshots (
                    snapshot_id TEXT NOT NULL,
                    candidate_id TEXT NOT NULL,
                    decision TEXT NOT NULL,
                    policy_id TEXT NOT NULL,
                    controls TEXT NOT NULL,
                    rationale TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    FOREIGN KEY(snapshot_id) REFERENCES policy_snapshot_runs(snapshot_id)
                )
                """
            )
            connection.execute(
                """
                CREATE TABLE IF NOT EXISTS batch_correction_snapshots (
                    snapshot_id TEXT NOT NULL,
                    proposal_id TEXT NOT NULL,
                    candidate_id TEXT NOT NULL,
                    decision TEXT NOT NULL,
                    policy_id TEXT NOT NULL,
                    controls TEXT NOT NULL,
                    rationale TEXT NOT NULL,
                    proposed_min_profit_bps INTEGER NOT NULL,
                    proposed_min_confidence_bps INTEGER NOT NULL,
                    requires_human_review INTEGER NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    FOREIGN KEY(snapshot_id) REFERENCES policy_snapshot_runs(snapshot_id)
                )
                """
            )
            connection.execute(
                "CREATE INDEX IF NOT EXISTS idx_policy_snapshot_runs_batch ON policy_snapshot_runs(batch_id)"
            )
            connection.execute(
                "CREATE INDEX IF NOT EXISTS idx_batch_action_batch ON batch_action_snapshots(snapshot_id)"
            )
            connection.execute(
                "CREATE INDEX IF NOT EXISTS idx_batch_correction_batch ON batch_correction_snapshots(snapshot_id)"
            )

    def _persist_batch_snapshot(self, snapshot_id: str, snapshot: InstitutionalBatchEvaluation) -> None:
        created_at = snapshot.created_at_ms
        with sqlite3.connect(self._snapshot_db_path) as connection:
            connection.execute(
                "INSERT OR REPLACE INTO policy_snapshot_runs("
                "snapshot_id,batch_id,policy_id,created_at_ms,candidate_count,approved_count,review_count,blocked_count,controls_digest"
                ") VALUES(?,?,?,?,?,?,?,?,?)",
                (
                    snapshot_id,
                    snapshot.batch_id,
                    snapshot.policy_id,
                    created_at,
                    len(snapshot.actions),
                    snapshot.approved_count,
                    snapshot.review_count,
                    snapshot.blocked_count,
                    snapshot.controls_digest,
                ),
            )
            for action in snapshot.actions:
                connection.execute(
                    "INSERT INTO batch_action_snapshots("
                    "snapshot_id,candidate_id,decision,policy_id,controls,rationale,created_at_ms"
                    ") VALUES(?,?,?,?,?,?,?)",
                    (
                        snapshot_id,
                        action.candidate_id,
                        action.decision.value,
                        action.policy_id,
                        json.dumps(action.controls),
                        action.rationale,
                        created_at,
                    ),
                )

    def _persist_proposal_snapshot(self, snapshot_id: str, snapshot: PolicyCorrectionBatch) -> None:
        created_at = snapshot.created_at_ms
        with sqlite3.connect(self._snapshot_db_path) as connection:
            connection.execute(
                "INSERT OR REPLACE INTO policy_snapshot_runs("
                "snapshot_id,batch_id,policy_id,created_at_ms,candidate_count,approved_count,review_count,blocked_count,controls_digest"
                ") VALUES(?,?,?,?,?,?,?,?,?)",
                (
                    snapshot_id,
                    snapshot.batch_id,
                    snapshot.policy_id,
                    created_at,
                    len(snapshot.proposals),
                    snapshot.approved_count,
                    snapshot.review_count,
                    snapshot.blocked_count,
                    snapshot.controls_digest,
                ),
            )
            for proposal in snapshot.proposals:
                connection.execute(
                    "INSERT INTO batch_correction_snapshots("
                    "snapshot_id,proposal_id,candidate_id,decision,policy_id,controls,rationale,proposed_min_profit_bps,proposed_min_confidence_bps,requires_human_review,created_at_ms"
                    ") VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                    (
                        snapshot_id,
                        proposal.proposal_id,
                        proposal.candidate_id,
                        proposal.decision.value,
                        proposal.policy_id,
                        json.dumps(proposal.controls),
                        proposal.rationale,
                        proposal.proposed_min_profit_bps,
                        proposal.proposed_min_confidence_bps,
                        int(proposal.requires_human_review),
                        created_at,
                    ),
                )

    def _controls_digest(self, controls: Iterable[tuple[str, ...]]) -> str:
        digest_input = json.dumps(
            sorted("|".join(control_set) for control_set in controls),
            separators=(",", ":"),
        ).encode("utf-8")
        return f"0x{sha256(digest_input).hexdigest()}"

    def _count_decisions(self, decisions: Iterable[InstitutionalSolverDecision | InstitutionalAction | PolicyCorrectionProposal]) -> dict[
        InstitutionalSolverDecision, int
    ]:
        counts = {
            InstitutionalSolverDecision.APPROVE: 0,
            InstitutionalSolverDecision.REVIEW: 0,
            InstitutionalSolverDecision.BLOCK: 0,
        }
        for entry in tuple(decisions):
            decision = entry if isinstance(entry, InstitutionalSolverDecision) else entry.decision
            counts[decision] += 1
        return counts

    def _snapshot_id(self, batch_id: str, created_at_ms: int) -> str:
        return f"{self._policy_id}:{batch_id}:{created_at_ms}"

    def _now_ms(self) -> int:
        return time_ns() // 1_000_000


__all__ = (
    "AdaptationCandidate",
    "InstitutionalAction",
    "InstitutionalBatchEvaluation",
    "InstitutionalSolverDecision",
    "InstitutionalSolverIntelligence",
    "InstitutionalSolverResponsibilities",
    "PolicyCorrectionBatch",
    "PolicyCorrectionProposal",
    "PolicyObservation",
    "PolicySnapshot",
)
