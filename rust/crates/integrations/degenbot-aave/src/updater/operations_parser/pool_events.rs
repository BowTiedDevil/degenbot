use super::{
    aave_event_decoder, log_idx_value, HashSet, Log, Operation, ParseError, ScaledTokenEvent,
    TransactionOperationsParser,
};

impl<'a> TransactionOperationsParser<'a> {
    // ── the per-pool-event dispatch (`_create_operation_from_pool_event`) ──

    pub(super) fn create_operation_from_pool_event(
        &self,
        operation_id: u32,
        pool_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        all_events: &'a [&'a Log],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let topic = pool_event
            .topics()
            .first()
            .copied()
            .ok_or_else(|| ParseError::Substrate("pool_event has no topic".into()))?;
        // Match by topic — use the decoder's topic constants to dispatch.
        if topic == aave_event_decoder::AAVE_SUPPLY_TOPIC {
            self.create_supply_operation(operation_id, pool_event, scaled_events, assigned_indices)
        } else if topic == aave_event_decoder::AAVE_WITHDRAW_TOPIC {
            self.create_withdraw_operation(
                operation_id,
                pool_event,
                scaled_events,
                assigned_indices,
            )
        } else if topic == aave_event_decoder::AAVE_BORROW_TOPIC {
            self.create_borrow_operation(operation_id, pool_event, scaled_events, assigned_indices)
        } else if topic == aave_event_decoder::AAVE_REPAY_TOPIC {
            self.create_repay_operation(operation_id, pool_event, scaled_events, assigned_indices)
        } else if topic == aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC {
            // the real liquidation engine (the
            // SINGLE/COMBINED_BURN/SEPARATE_BURNS pattern detection + the
            // `_analyze_liquidation_scenarios` / `_analyze_user_liquidation_
            // count` pre-analysis dicts over `all_events`).
            self.create_liquidation_operation(
                operation_id,
                pool_event,
                scaled_events,
                all_events,
                assigned_indices,
            )
        } else if topic == aave_event_decoder::AAVE_DEFICIT_CREATED_TOPIC {
            Ok(self.create_deficit_operation(operation_id, pool_event))
        } else {
            Err(ParseError::Substrate(format!(
                "unexpected pool-event topic {topic}"
            )))
        }
    }
}

/// Extract the pool events from a `&[&Log]` slice. Sorted by logIndex.
pub(super) fn extract_pool_events<'a>(events: &[&'a Log]) -> Vec<&'a Log> {
    let mut pool: Vec<&Log> = events
        .iter()
        .copied()
        .filter(|l| is_pool_event_log(l))
        .collect();
    pool.sort_by_key(|l| log_idx_value(l));
    pool
}

/// `true` if the log is one of the 6 pool-event anchors (Supply/Withdraw/
/// Borrow/Repay/LiquidationCall/DeficitCreated).
fn is_pool_event_log(log: &Log) -> bool {
    let Some(t) = log.topics().first() else {
        return false;
    };
    [
        aave_event_decoder::AAVE_SUPPLY_TOPIC,
        aave_event_decoder::AAVE_WITHDRAW_TOPIC,
        aave_event_decoder::AAVE_BORROW_TOPIC,
        aave_event_decoder::AAVE_REPAY_TOPIC,
        aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC,
        aave_event_decoder::AAVE_DEFICIT_CREATED_TOPIC,
    ]
    .contains(t)
}
