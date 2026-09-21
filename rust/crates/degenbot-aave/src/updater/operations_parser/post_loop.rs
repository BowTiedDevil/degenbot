use super::util::{clone_scaled_event, is_part_of_burn, is_part_of_mint};
use super::{
    aave_event_decoder, addr_to_hex, parse_address, Address, DecodedAaveEvent, DegenbotDb, HashSet,
    Log, Operation, OperationType, ScaledTokenEvent, ScaledTokenEventType,
    TransactionOperationsParser, U256,
};

impl<'a> TransactionOperationsParser<'a> {
    /// The v8-vs-v9+ `ray_div`
    /// subtlety (DP3): for `pool_revision` <= 8, `amountMinted` is in underlying
    /// units → apply `ray_div(amountMinted, liquidity_index, HALF_UP)` to
    /// derive the scaled amount; for `pool_revision` >= 9, `amountMinted`
    /// equals the scaled amount directly.
    pub(super) fn create_mint_to_treasury_operations(
        &self,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        next_op_id: &mut u32,
        minted_to_treasury_events: &[&'a Log],
    ) -> Vec<Operation<'a>> {
        let mut operations: Vec<Operation<'a>> = Vec::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralMint {
                continue;
            }
            // Only CollateralMint where caller == Pool (mintToTreasury indicator).
            if ev.caller_address != Some(self.pool_address) {
                continue;
            }
            // Skip pure-interest accrual (amount == balance_increase).
            let bal_inc = ev.balance_increase.unwrap_or(U256::ZERO);
            if ev.amount == bal_inc {
                // Pure-accrual Mint — not a MintToTreasury.
                assigned_indices.insert(ev.log_index);
                continue;
            }

            // Determine the underlying asset (the Mint event's aToken → asset row).
            let asset_row = DegenbotDb::lookup_asset_by_token_address_on_conn(
                self.conn,
                self.market_id,
                &addr_to_hex(ev.token_address),
                "a_token",
            )
            .ok()
            .flatten();
            let underlying_addr = asset_row
                .as_ref()
                .and_then(|a| parse_address(&a.underlying_token_address))
                .unwrap_or(Address::ZERO);

            // Extract the amountMinted from the MintedToTreasury log matching
            // this underlying asset.
            let mut minted_amount: Option<U256> = None;
            for mt_ev in minted_to_treasury_events {
                if let Some(DecodedAaveEvent::MintedToTreasury(m)) =
                    aave_event_decoder::decode_aave_log(mt_ev)
                {
                    if m.reserve == underlying_addr {
                        minted_amount = Some(m.amount_minted);
                        break;
                    }
                }
            }

            // DP3: store the MintedToTreasury event's `amount_minted` as the
            // raw UNDERLYING amount on the operation (regardless of pool
            // revision). The scaling conversion (ray_div / ray_div_ceil per
            // rev) happens EXACTLY ONCE in `dispatch_mint_to_treasury`
            // (single conversion at apply-time). Previously this DP3 arm
            // pre-converted for rev < 9 → `dispatch_mint_to_treasury` would
            // convert AGAIN → double-division by `idx`, shrinking the applied
            // scaled_amount by a factor of `idx/RAY` (root cause:
            // treasury 0x464C divergences across 4 aToken positions — WETH
            // -8.04e12, wstETH -2.29e11, DAI -3.12e15, USDC -6415).
            let resolved = minted_amount;

            assigned_indices.insert(ev.log_index);
            let op_id = *next_op_id;
            *next_op_id += 1;
            operations.push(Operation {
                operation_id: op_id,
                operation_type: OperationType::MintToTreasury,
                pool_revision: self.pool_revision,
                pool_event: None,
                scaled_events: vec![clone_scaled_event(ev)],
                transfer_events: Vec::new(),
                balance_transfer_events: Vec::new(),
                minted_to_treasury_amount: resolved,
                debt_to_cover: None,
                validation_errors: Vec::new(),
            });
        }
        operations
    }

    /// `DEFICIT_CREATED` → Unknown
    /// (placeholder awaiting downstream liquidation matching). The Python
    /// deliberately uses UNKNOWN so `DEFICIT_CREATED` doesn't interfere with
    /// liquidation processing downstream.
    pub(super) fn create_deficit_operation(
        &self,
        operation_id: u32,
        deficit_event: &'a Log,
    ) -> Operation<'a> {
        Operation {
            operation_id,
            operation_type: OperationType::Unknown,
            pool_revision: self.pool_revision,
            pool_event: Some(deficit_event),
            scaled_events: Vec::new(),
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        }
    }

    /// Deficit-coverage
    /// `BalanceTransfer` + Burn pair (phase 4c). ERC20-Transfer +
    /// `BalanceTransfer` + Burn triplet matching (the §4.2-drift knife-edge
    /// per DP6 — kept in A; escalate to B if gnarly).
    pub(super) fn create_deficit_coverage_operations(
        &self,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        next_op_id: &mut u32,
    ) -> Vec<Operation<'a>> {
        let mut operations: Vec<Operation<'a>> = Vec::new();
        let mut local_assigned: HashSet<u64> = HashSet::new();

        // Find all unassigned BalanceTransfers (aToken-side, including ERC20).
        let balance_transfers: Vec<&ScaledTokenEvent<'a>> = scaled_events
            .iter()
            .filter(|ev| {
                (ev.event_type == ScaledTokenEventType::CollateralTransfer
                    || ev.event_type == ScaledTokenEventType::Erc20CollateralTransfer)
                    && !assigned_indices.contains(&ev.log_index)
                    && !local_assigned.contains(&ev.log_index)
            })
            .collect();

        for bt_ev in balance_transfers {
            // For each BalanceTransfer, look for a paired CollateralBurn.
            let bt_target = bt_ev.target_address.unwrap_or(Address::ZERO);
            let paired_burn: Option<&ScaledTokenEvent<'a>> = scaled_events.iter().find(|burn_ev| {
                if assigned_indices.contains(&burn_ev.log_index)
                    || local_assigned.contains(&burn_ev.log_index)
                {
                    return false;
                }
                if burn_ev.event_type != ScaledTokenEventType::CollateralBurn {
                    return false;
                }
                if burn_ev.user_address != bt_target {
                    return false;
                }
                burn_ev.token_address == bt_ev.token_address
            });

            // DeficitCoverage: the bt_ev is required to be ERC20_COLLATERAL_TRANSFER
            // for the triplet middle-insertion. If paired_burn is Some, build the op.
            let paired = if let Some(burn) = paired_burn {
                let mut paired_events: Vec<ScaledTokenEvent<'a>> =
                    vec![clone_scaled_event(bt_ev), clone_scaled_event(burn)];
                // The Python requires bt_ev.event_type == ERC20_COLLATERAL_TRANSFER
                // here; an additional look for a matching BalanceTransfer (with index
                // field) for the same transfer — inserted between transfer + burn.
                let mut bt_events: Vec<&'a Log> = Vec::new();
                if bt_ev.event_type == ScaledTokenEventType::Erc20CollateralTransfer {
                    for other_ev in scaled_events {
                        if assigned_indices.contains(&other_ev.log_index)
                            || local_assigned.contains(&other_ev.log_index)
                        {
                            continue;
                        }
                        if other_ev.event_type != ScaledTokenEventType::CollateralTransfer {
                            continue;
                        }
                        if other_ev.from_address != bt_ev.from_address {
                            continue;
                        }
                        paired_events.insert(1, clone_scaled_event(other_ev));
                        bt_events.push(other_ev.log);
                        local_assigned.insert(other_ev.log_index);
                        break;
                    }
                }
                let op_id = *next_op_id;
                *next_op_id += 1;
                assigned_indices.insert(bt_ev.log_index);
                assigned_indices.insert(burn.log_index);
                operations.push(Operation {
                    operation_id: op_id,
                    operation_type: OperationType::DeficitCoverage,
                    pool_revision: self.pool_revision,
                    pool_event: None,
                    scaled_events: paired_events,
                    transfer_events: Vec::new(),
                    balance_transfer_events: bt_events,
                    minted_to_treasury_amount: None,
                    debt_to_cover: None,
                    validation_errors: Vec::new(),
                });
                true
            } else {
                false
            };
            // Only mark the bt_ev assigned when it was actually paired into
            // a DeficitCoverage op. Unpaired transfers (no paired burn — the
            // normal aToken-transfer case, NOT an Umbrella DeficitCoverage)
            // must fall through to create_transfer_operations as standalone
            // BalanceTransfer ops (previously the unconditional
            // `local_assigned.insert` stole them → the recipient's credit
            // never landed → their Withdraw's Burn went negative).
            //
            // NB: the paired bt_ev is already directly inserted into
            // `assigned_indices` above (so the extend below flushing the
            // inner-loop BalanceTransfer companions suffices).
            if paired {
                local_assigned.insert(bt_ev.log_index);
            }
        }

        // Merge local_assigned back into assigned_indices.
        assigned_indices.extend(local_assigned.iter().copied());
        operations
    }

    /// Unassigned Mint events
    /// (with `amount == balance_increase` or small balanceIncrease) → pure
    /// interest accrual operations. Includes dust mints from discounts.
    pub(super) fn create_interest_accrual_operations(
        &self,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        next_op_id: &mut u32,
    ) -> Vec<Operation<'a>> {
        let mut operations: Vec<Operation<'a>> = Vec::new();
        let mut local_assigned: HashSet<u64> = HashSet::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) || local_assigned.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralMint
                && ev.event_type != ScaledTokenEventType::DebtMint
                && ev.event_type != ScaledTokenEventType::GhoDebtMint
            {
                continue;
            }
            // Find matching Transfer event (from ZERO_ADDRESS to this user).
            let mut transfer_events: Vec<&'a Log> = Vec::new();
            for transfer_ev in scaled_events {
                let from_zero = transfer_ev.from_address == Some(Address::ZERO);
                let Some(target) = transfer_ev.target_address else {
                    continue;
                };
                if transfer_ev.event_type != ScaledTokenEventType::CollateralTransfer
                    && transfer_ev.event_type != ScaledTokenEventType::Erc20CollateralTransfer
                {
                    continue;
                }
                if !from_zero || target != ev.user_address {
                    continue;
                }
                if transfer_ev.token_address != ev.token_address {
                    continue;
                }
                if assigned_indices.contains(&transfer_ev.log_index)
                    || local_assigned.contains(&transfer_ev.log_index)
                {
                    continue;
                }
                if transfer_ev.amount <= ev.amount {
                    transfer_events.push(transfer_ev.log);
                    local_assigned.insert(transfer_ev.log_index);
                    break;
                }
            }
            let op_id = *next_op_id;
            *next_op_id += 1;
            operations.push(Operation {
                operation_id: op_id,
                operation_type: OperationType::InterestAccrual,
                pool_revision: self.pool_revision,
                pool_event: None,
                scaled_events: vec![clone_scaled_event(ev)],
                transfer_events,
                balance_transfer_events: Vec::new(),
                minted_to_treasury_amount: None,
                debt_to_cover: None,
                validation_errors: Vec::new(),
            });
            local_assigned.insert(ev.log_index);
        }
        operations
    }

    /// Unassigned transfer events → standalone `BALANCE_TRANSFER` (or
    /// `STKAAVE_TRANSFER`) operations. Pairs ERC20 Transfer + `BalanceTransfer`
    /// for the same movement.
    pub(super) fn create_transfer_operations(
        &self,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        next_op_id: &mut u32,
    ) -> Vec<Operation<'a>> {
        let mut operations: Vec<Operation<'a>> = Vec::new();
        let mut local_assigned: HashSet<u64> = HashSet::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) || local_assigned.contains(&ev.log_index) {
                continue;
            }
            let is_transfer = matches!(
                ev.event_type,
                ScaledTokenEventType::CollateralTransfer
                    | ScaledTokenEventType::DebtTransfer
                    | ScaledTokenEventType::DiscountTransfer
                    | ScaledTokenEventType::Erc20CollateralTransfer
                    | ScaledTokenEventType::Erc20DebtTransfer
                    | ScaledTokenEventType::GhoDebtTransfer
            );
            if !is_transfer {
                continue;
            }
            assert!(ev.index.is_none(), "Transfer events have no index");
            // Skip transfers to/from zero that are part of mints/burns.
            if ev.target_address == Some(Address::ZERO)
                && is_part_of_burn(ev, scaled_events, &mut local_assigned)
            {
                continue;
            }
            if ev.from_address == Some(Address::ZERO)
                && is_part_of_mint(ev, scaled_events, &mut local_assigned)
            {
                continue;
            }
            // Find matching BalanceTransfer for the same movement.
            let mut balance_transfer_events: Vec<&'a Log> = Vec::new();
            for bt_ev in scaled_events {
                if assigned_indices.contains(&bt_ev.log_index)
                    || local_assigned.contains(&bt_ev.log_index)
                {
                    continue;
                }
                if bt_ev.index.is_none() {
                    continue;
                }
                if bt_ev.from_address != ev.from_address {
                    continue;
                }
                if bt_ev.target_address != ev.target_address {
                    continue;
                }
                if bt_ev.token_address != ev.token_address {
                    continue;
                }
                if !Self::are_compatible_transfer_types(ev.event_type, bt_ev.event_type) {
                    continue;
                }
                local_assigned.insert(bt_ev.log_index);
                balance_transfer_events.push(bt_ev.log);
                break;
            }
            let op_type = if ev.event_type == ScaledTokenEventType::DiscountTransfer {
                OperationType::StkAaveTransfer
            } else {
                OperationType::BalanceTransfer
            };
            let op_id = *next_op_id;
            *next_op_id += 1;
            operations.push(Operation {
                operation_id: op_id,
                operation_type: op_type,
                pool_revision: self.pool_revision,
                pool_event: None,
                scaled_events: vec![clone_scaled_event(ev)],
                transfer_events: Vec::new(),
                balance_transfer_events,
                minted_to_treasury_amount: None,
                debt_to_cover: None,
                validation_errors: Vec::new(),
            });
        }
        assigned_indices.extend(local_assigned.iter().copied());
        operations
    }
}
