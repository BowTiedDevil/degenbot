use alloy::primitives::{I256, U256};

/// Profit score as a signed value (`output − input`), so direction and
/// refinement can compare candidates on the unprofitable side without U256
/// underflow.
pub(super) fn walk_profit_score(output: U256, input: U256) -> I256 {
    let o = I256::try_from(output).unwrap_or(I256::MAX);
    let i = I256::try_from(input).unwrap_or(I256::MAX);
    o - i
}

pub(super) struct DirectionObservation {
    pub(super) score: I256,
    pub(super) model_domain_exhausted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DirectionAction {
    Advance,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StopReason {
    ModelDomainExhausted,
    InputSaturated,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "diagnostic fields are read by direction tests; production traversal consumes the action"
    )
)]
pub(super) struct DirectionDecision {
    pub(super) action: DirectionAction,
    pub(super) probes: usize,
    pub(super) final_span: U256,
    pub(super) stop_reason: Option<StopReason>,
}

impl DirectionObservation {
    pub(super) fn from_output(output: U256, input: U256, model_domain_exhausted: bool) -> Self {
        Self {
            score: walk_profit_score(output, input),
            model_domain_exhausted,
        }
    }
}

pub(super) fn direction_at_edge(
    x_l: U256,
    x_r: U256,
    mut observe: impl FnMut(U256) -> DirectionObservation,
) -> DirectionDecision {
    let back = x_r.saturating_sub(U256::from(64u64)).max(x_l);
    let score_back = observe(back).score;
    let mut probes = 1usize;
    let mut final_span = U256::from(64u64);
    loop {
        let fwd = x_r.saturating_add(final_span);
        let observation = observe(fwd);
        probes += 1;
        if observation.score >= score_back {
            return DirectionDecision {
                action: DirectionAction::Advance,
                probes,
                final_span,
                stop_reason: None,
            };
        }
        if observation.model_domain_exhausted {
            return DirectionDecision {
                action: DirectionAction::Stop,
                probes,
                final_span,
                stop_reason: Some(StopReason::ModelDomainExhausted),
            };
        }
        if fwd == U256::MAX {
            return DirectionDecision {
                action: DirectionAction::Stop,
                probes,
                final_span,
                stop_reason: Some(StopReason::InputSaturated),
            };
        }
        final_span = final_span.saturating_mul(U256::from(2u64));
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn observation(score: i64, model_domain_exhausted: bool) -> DirectionObservation {
        DirectionObservation {
            score: I256::try_from(score).unwrap(),
            model_domain_exhausted,
        }
    }

    #[test]
    fn negative_score_recovery_advances_with_rear_parity() {
        let mut observations = [observation(-10, false), observation(-5, false)].into_iter();
        let mut inputs = Vec::new();
        let decision = direction_at_edge(U256::ZERO, U256::from(1_000u64), |input| {
            inputs.push(input);
            observations.next().unwrap()
        });
        assert_eq!(decision.action, DirectionAction::Advance);
        assert_eq!(decision.probes, 2);
        assert_eq!(decision.final_span, U256::from(64u64));
        assert_eq!(decision.stop_reason, None);
        assert_eq!(inputs, [U256::from(936u64), U256::from(1_064u64)]);
    }

    #[test]
    fn local_dip_widens_until_forward_score_reaches_rear_score() {
        let mut observations = [
            observation(100, false),
            observation(90, false),
            observation(95, false),
            observation(100, false),
        ]
        .into_iter();
        let mut inputs = Vec::new();
        let decision = direction_at_edge(U256::ZERO, U256::from(1_000u64), |input| {
            inputs.push(input);
            observations.next().unwrap()
        });
        assert_eq!(decision.action, DirectionAction::Advance);
        assert_eq!(decision.probes, 4);
        assert_eq!(decision.final_span, U256::from(256u64));
        assert_eq!(decision.stop_reason, None);
        assert_eq!(
            inputs,
            [
                U256::from(936u64),
                U256::from(1_064u64),
                U256::from(1_128u64),
                U256::from(1_256u64),
            ]
        );
    }

    #[test]
    fn immediate_rise_advances_on_first_forward_probe() {
        let mut observations = [observation(0, false), observation(1, false)].into_iter();
        let decision = direction_at_edge(U256::ZERO, U256::from(1_000u64), |_| {
            observations.next().unwrap()
        });
        assert_eq!(decision.action, DirectionAction::Advance);
        assert_eq!(decision.probes, 2);
        assert_eq!(decision.final_span, U256::from(64u64));
    }

    #[test]
    fn oscillation_stops_widening_on_first_rear_parity() {
        let mut observations = [
            observation(10, false),
            observation(9, false),
            observation(11, false),
        ]
        .into_iter();
        let decision = direction_at_edge(U256::ZERO, U256::from(1_000u64), |_| {
            observations.next().unwrap()
        });
        assert_eq!(decision.action, DirectionAction::Advance);
        assert_eq!(decision.probes, 3);
        assert_eq!(decision.final_span, U256::from(128u64));
    }

    #[test]
    fn equal_forward_score_advances() {
        let mut observations = [observation(7, false), observation(7, false)].into_iter();
        let decision = direction_at_edge(U256::ZERO, U256::from(1_000u64), |_| {
            observations.next().unwrap()
        });
        assert_eq!(decision.action, DirectionAction::Advance);
        assert_eq!(decision.probes, 2);
        assert_eq!(decision.stop_reason, None);
    }

    #[test]
    fn continuous_decline_stops_on_model_domain_exhaustion() {
        let mut observations = [
            observation(100, false),
            observation(90, false),
            observation(80, true),
        ]
        .into_iter();
        let decision = direction_at_edge(U256::ZERO, U256::from(1_000u64), |_| {
            observations.next().unwrap()
        });
        assert_eq!(decision.action, DirectionAction::Stop);
        assert_eq!(decision.probes, 3);
        assert_eq!(decision.final_span, U256::from(128u64));
        assert_eq!(decision.stop_reason, Some(StopReason::ModelDomainExhausted));
    }

    #[test]
    fn decline_without_exhaustion_stops_at_input_saturation() {
        let mut observations = [observation(0, false), observation(-1, false)].into_iter();
        let decision = direction_at_edge(
            U256::ZERO,
            U256::MAX.saturating_sub(U256::from(64u64)),
            |_| observations.next().unwrap(),
        );
        assert_eq!(decision.action, DirectionAction::Stop);
        assert_eq!(decision.probes, 2);
        assert_eq!(decision.final_span, U256::from(64u64));
        assert_eq!(decision.stop_reason, Some(StopReason::InputSaturated));
    }

    #[test]
    fn forward_span_saturates_at_u256_max() {
        let mut observations = [
            observation(0, false),
            observation(-1, false),
            observation(-2, false),
        ]
        .into_iter();
        let decision = direction_at_edge(
            U256::ZERO,
            U256::MAX.saturating_sub(U256::from(128u64)),
            |_| observations.next().unwrap(),
        );
        assert_eq!(decision.action, DirectionAction::Stop);
        assert_eq!(decision.probes, 3);
        assert_eq!(decision.final_span, U256::from(128u64));
        assert_eq!(decision.stop_reason, Some(StopReason::InputSaturated));
    }
}
