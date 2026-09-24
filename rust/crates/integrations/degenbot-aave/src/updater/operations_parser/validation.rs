use super::{Operation, OperationType, ScaledTokenEventType, TransactionOperationsParser};

impl TransactionOperationsParser<'_> {
    // ── the validators (`_validate_operation` + per-op) ───────────────────

    ///  dispatch. Mutates `op.validation_errors`
    /// (the validators are token-fillers here; the Python's strict top-level
    /// `TransactionOperations.validate` pass).
    /// Dispatch-style match over `OperationType` — one arm per variant. Kept as
    /// a single fn (rather than per-op validator helpers) because each arm is
    /// 2–8 lines of predicate checks; splitting would fragment the validation
    /// logic across ~10 helpers without improving clarity (the Python uses a
    /// validators dict, not per-op fns here).
    #[expect(clippy::too_many_lines)]
    pub(super) fn validate_operation(op: &mut Operation<'_>) {
        // The API dispatch is a match over the operation type; the
        // actual validation is per-op (`_validate_*` in Python). For A's
        // parse() integrity, the validators are minimal — they fill
        // `validation_errors` with descriptive messages on miss-pattern.
        // The strict top-level pass is a separate concern.
        match op.operation_type {
            OperationType::Supply => {
                if op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_collateral())
                    .count()
                    != 1
                {
                    op.validation_errors
                        .push("SUPPLY expects 1 collateral event".into());
                }
            }
            OperationType::Withdraw => {
                let burns: usize = op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_collateral() && e.event_type.is_burn())
                    .count();
                if burns > 1 {
                    op.validation_errors
                        .push(format!("WITHDRAW expects ≤1 collateral burn, got {burns}"));
                }
            }
            OperationType::Borrow | OperationType::GhoBorrow => {
                if op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_debt())
                    .count()
                    != 1
                {
                    op.validation_errors
                        .push("BORROW expects 1 debt event".into());
                }
            }
            OperationType::Repay | OperationType::GhoRepay => {
                if op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_debt())
                    .count()
                    != 1
                {
                    op.validation_errors
                        .push("REPAY expects 1 debt event".into());
                }
            }
            OperationType::RepayWithAtokens => {
                let debt = op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_debt())
                    .count();
                if debt != 1 {
                    op.validation_errors.push(format!(
                        "REPAY_WITH_ATOKENS expects 1 debt event, got {debt}"
                    ));
                }
                let burns = op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_collateral() && e.event_type.is_burn())
                    .count();
                if burns > 1 {
                    op.validation_errors.push(format!(
                        "REPAY_WITH_ATOKENS expects ≤1 collateral burn, got {burns}"
                    ));
                }
            }
            OperationType::InterestAccrual => {
                if op.scaled_events.len() != 1 {
                    op.validation_errors
                        .push("INTEREST_ACCRUAL expects 1 scaled event".into());
                }
            }
            OperationType::BalanceTransfer => {
                if op.scaled_events.len() != 1 {
                    op.validation_errors
                        .push("BALANCE_TRANSFER expects 1 scaled event".into());
                }
            }
            OperationType::DeficitCoverage => {
                if op.scaled_events.len() < 2 {
                    op.validation_errors
                        .push("DEFICIT_COVERAGE expects ≥2 (transfer + burn)".into());
                }
            }
            OperationType::MintToTreasury => {
                if op.scaled_events.len() != 1
                    || !matches!(
                        op.scaled_events[0].event_type,
                        ScaledTokenEventType::CollateralMint
                    )
                {
                    op.validation_errors
                        .push("MINT_TO_TREASURY expects 1 CollateralMint".into());
                }
            }
            OperationType::StkAaveTransfer
                if (op.scaled_events.len() != 1
                    || op.scaled_events[0].event_type
                        != ScaledTokenEventType::DiscountTransfer) =>
            {
                op.validation_errors
                    .push("STKAAVE_TRANSFER expects 1 DiscountTransfer".into());
            }
            // ExpectedAbsent: Liquidation / GhoLiquidation / GhoFlashLoan /
            // Unknown get minimal validation (B owns the LiquidationCall
            // builder + its validators).
            _ => {}
        }
    }
}
