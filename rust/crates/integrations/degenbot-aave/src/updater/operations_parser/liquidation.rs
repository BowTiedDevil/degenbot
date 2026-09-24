use super::util::{clone_scaled_event, is_part_of_burn, is_part_of_mint};
use super::{
    aave_event_decoder, log_idx_value, Address, HashMap, HashSet, Log, Operation, OperationType,
    ParseError, ScaledTokenEvent, ScaledTokenEventType, TransactionOperationsParser, U256,
};

impl<'a> TransactionOperationsParser<'a> {
    // ── the liquidation engine ────────────────────────────────────────────

    // ── the liquidation engine fns ──────────────────────────────────

    /// pre-analyze
    /// liquidations to detect multi-liquidation scenarios. Returns a mapping
    /// of `(user, debt_v_token_address)` → `liquidation_count`. This allows
    /// proper disambiguation when the same user is liquidated multiple times
    /// with the same debt asset in one transaction.
    ///
    /// §4.2-parity note: the Python reads `decode_address(ev["topics"][3])`
    /// (user) + `decode_address(ev["topics"][2])` (`debt_asset`) per
    /// `LiquidationCall` event → resolves `debt_asset` → vToken via
    /// `_get_v_token_for_asset`; we mirror the same per-event vToken resolution.
    fn analyze_liquidation_scenarios(
        &self,
        all_events: &[&Log],
    ) -> Result<HashMap<(Address, Address), usize>, ParseError> {
        let mut counts: HashMap<(Address, Address), usize> = HashMap::new();
        for ev in all_events {
            let topics = ev.topics();
            if topics.first() != Some(&aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC) {
                continue;
            }
            if topics.len() < 4 {
                continue;
            }
            let user = Address::from_word(topics[3]);
            let debt_asset = Address::from_word(topics[2]);
            let v_token_address = self.get_v_token_for_asset(debt_asset)?.ok_or_else(|| {
                ParseError::Substrate(format!(
                    "no vToken for debt_asset {debt_asset} in market {}",
                    self.market_id
                ))
            })?;
            *counts.entry((user, v_token_address)).or_insert(0) += 1;
        }
        Ok(counts)
    }

    /// count total
    /// liquidations per user (not per user+asset pair). Returns `user` →
    /// `total_liquidations`.
    ///
    /// When a user has exactly 1 liquidation, ALL debt burns for that user
    /// belong to that single liquidation. This handles bad-debt liquidations
    /// where the protocol burns multiple debt positions via `_burnBadDebt()`.
    /// When a user has multiple liquidations, use asset-specific matching to
    /// disambiguate which burns belong to which liquidation.
    pub(super) fn analyze_user_liquidation_count(all_events: &[&Log]) -> HashMap<Address, usize> {
        let mut counts: HashMap<Address, usize> = HashMap::new();
        for ev in all_events {
            let topics = ev.topics();
            if topics.first() != Some(&aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC) {
                continue;
            }
            if topics.len() < 4 {
                continue;
            }
            let user = Address::from_word(topics[3]);
            *counts.entry(user).or_insert(0) += 1;
        }
        counts
    }

    /// the §4.2-critical
    /// `SINGLE/COMBINED_BURN/SEPARATE_BURNS` pattern-detection debt-burn
    /// collector.
    ///
    /// Collection strategy (port-EXACT):
    /// - Single liquidation per user: collect ALL debt burns (no asset filter)
    ///   → handles bad-debt liquidations where `_burnBadDebt()` burns all debt positions.
    /// - Multiple liquidations per user: use asset filter + sequential matching
    ///   to disambiguate which burns belong to which liquidation.
    ///   - **`SEPARATE_BURNS`** (`liquidation_count_for_asset == total_burn_count`):
    ///     burn `i` belongs to liquidation `i`; return the burn at
    ///     `liquidation_position`.
    ///   - **`COMBINED_BURN`** (`liquidation_count_for_asset > total_burn_count` +
    ///     `liquidation_position == 0`): all burns go to the first liquidation.
    ///
    /// # §4.2-parity note (the `assigned_indices.add(ev.index)` quirk)
    ///
    /// The Python also calls `assigned_indices.add(ev.index)` to store the
    /// Burn event's `index` field (a ray-scale u256 liquidity-index value) in
    /// `assigned_indices`. This storage is **dead-code** — every
    /// `in assigned_indices` query is `ev.event["logIndex"] in assigned_indices`
    /// (small-int logIndex), never `ev.index`. The `add(ev.index)` stores
    /// ray-scale values that are never matched; we omit the assignment (a u64
    /// set can't hold ray values anyway) but preserve the assertions
    /// `assert ev.index is not None; assert ev.index > 0` (the runtime panic on
    /// ill-formed events) as `debug_assert!`. Output parity verified: the
    /// `assigned_indices` state never affects the parsed Operation output.
    pub(super) fn collect_debt_burns<'b>(
        user: Address,
        debt_v_token_address: Option<Address>,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        liquidation_analysis: &HashMap<(Address, Address), usize>,
        user_liquidation_count: usize,
        liquidation_position: usize,
    ) -> Vec<&'b ScaledTokenEvent<'a>>
    where
        'a: 'b,
    {
        let mut burns: Vec<&ScaledTokenEvent<'a>> = Vec::new();
        if user_liquidation_count == 1 {
            // Single liquidation per user: collect ALL debt burns (no asset filter).
            let mut candidate_burns: Vec<&ScaledTokenEvent<'a>> = scaled_events
                .iter()
                .filter(|ev| {
                    !assigned_indices.contains(&ev.log_index)
                        && ev.user_address == user
                        && (ev.event_type == ScaledTokenEventType::DebtBurn
                            || ev.event_type == ScaledTokenEventType::GhoDebtBurn)
                })
                .collect();
            candidate_burns.sort_by_key(|e| e.log_index);
            for ev in candidate_burns {
                burns.push(ev);
                assigned_indices.insert(ev.log_index);
                let idx = ev.index.unwrap_or(U256::ZERO);
                debug_assert!(
                    !idx.is_zero(),
                    "DebtBurn index must be > 0 (parity: assert ev.index > 0)"
                );
                // NB: the Python's `assigned_indices.add(ev.index)` here is
                // dead-code (no later `in assigned_indices` query hits the
                // ray-scale `index` field); omitted (u64 set can't hold it).
            }
        } else {
            #[expect(clippy::expect_used)] // required on the multi-liquidation path (documented)
            let debt_v =
                debt_v_token_address.expect("multi-liquidation requires debt_v_token_address");
            let liquidation_count_for_asset = liquidation_analysis
                .get(&(user, debt_v))
                .copied()
                .unwrap_or(1);
            let is_multi_liquidation = liquidation_count_for_asset > 1;
            // Get ALL burns for this (user, debt_asset) to determine pattern.
            // Don't filter by assigned_indices yet — we need total count for
            // pattern detection (parity: Python lines 1580-1586).
            let mut all_burns_for_asset: Vec<&ScaledTokenEvent<'a>> = scaled_events
                .iter()
                .filter(|ev| {
                    ev.user_address == user
                        && (ev.event_type == ScaledTokenEventType::DebtBurn
                            || ev.event_type == ScaledTokenEventType::GhoDebtBurn)
                        && ev.token_address == debt_v
                })
                .collect();
            all_burns_for_asset.sort_by_key(|e| e.log_index);
            // Now get only unassigned burns for assignment.
            let candidate_burns: Vec<&ScaledTokenEvent<'a>> = all_burns_for_asset
                .iter()
                .copied()
                .filter(|ev| !assigned_indices.contains(&ev.log_index))
                .collect();
            // Determine pattern: COMBINED_BURN vs SEPARATE_BURNS.
            let total_burn_count = all_burns_for_asset.len();
            if is_multi_liquidation && !candidate_burns.is_empty() {
                debug_assert!(
                    liquidation_count_for_asset >= total_burn_count,
                    "parity: assert liquidation_count_for_asset >= total_burn_count"
                );
                if liquidation_count_for_asset == total_burn_count {
                    // SEPARATE_BURNS pattern: each liquidation gets exactly one burn.
                    debug_assert!(
                        liquidation_position < total_burn_count,
                        "parity: assert liquidation_position < total_burn_count"
                    );
                    let target_burn = all_burns_for_asset[liquidation_position];
                    debug_assert!(
                        !assigned_indices.contains(&target_burn.log_index),
                        "SEPARATE_BURNS target burn already assigned"
                    );
                    burns.push(target_burn);
                    assigned_indices.insert(target_burn.log_index);
                    let idx = target_burn.index.unwrap_or(U256::ZERO);
                    debug_assert!(!idx.is_zero());
                } else if liquidation_position == 0 {
                    // COMBINED_BURN pattern: more liquidations than burns.
                    // All burns go to the first liquidation.
                    for ev in candidate_burns {
                        burns.push(ev);
                        assigned_indices.insert(ev.log_index);
                        let idx = ev.index.unwrap_or(U256::ZERO);
                        debug_assert!(!idx.is_zero());
                    }
                }
            } else {
                // Single liquidation or no burns: collect all available burns.
                for ev in candidate_burns {
                    burns.push(ev);
                    assigned_indices.insert(ev.log_index);
                    let idx = ev.index.unwrap_or(U256::ZERO);
                    debug_assert!(!idx.is_zero());
                }
            }
        }
        burns
    }

    /// collect collateral
    /// events (burns and transfers) for the liquidation. During liquidations
    /// a borrower may have BOTH collateral burned AND multiple transfers.
    /// Collateral may be burned OR transferred to treasury (`BalanceTransfer`).
    ///
    /// Returns `(collateral_burn, collateral_transfers)`.
    ///
    /// # EIWEPM bug class — the burn-side / mint-side pair ERC20 Transfer
    ///
    /// By Aave V3 protocol, the aToken emits the `Burn` event AND the paired
    /// ERC20 `Transfer user→0x0` (the burn-side companion) as ONE operational
    /// action (the same for `Mint` + `Transfer 0x0→user`). Pre-fix this fn
    /// pushed the burn-side ERC20 Transfer into `collateral_transfers`
    /// unfiltered (no `is_part_of_burn` / `is_part_of_mint` call — those filters
    /// lived only on the standalone Transfer path at line 1471/1476). The
    /// Liquidation op ended up with BOTH the `CollateralBurn` AND its paired
    /// ERC20 `Transfer user→0x0` in `op.scaled_events`. `dispatch_liquidation`
    /// then applied both with no `override_transfer_with_paired_bt` (the
    /// Liquidation path's intentional no-op for that override) → the user's
    /// collateral balance was debited TWICE by the Burn delta (the p1198-class
    /// 4.33× divergence — root-caused + math-verified on mainnet).
    ///
    /// Fix (per the orchestrator's directive): wire the already-ported
    /// `is_part_of_burn` (line 2522) + `is_part_of_mint` (line 2548) filters
    /// INTO this fn — match the standalone Transfer path's exact semantics
    /// (same `user_address` + `token_address` matching, no value heuristic).
    /// Mark the filtered events into `assigned_indices` so the standalone
    /// Step-4e `create_transfer_operations` skips them (it would re-apply the
    /// filter from `scaled_events` and skip — but pre-marking is the safest
    /// mirror of the standalone pattern's `local_assigned` write-back).
    pub(super) fn collect_collateral_events<'b>(
        user: Address,
        collateral_a_token_address: Option<Address>,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> (
        Option<&'b ScaledTokenEvent<'a>>,
        Vec<&'b ScaledTokenEvent<'a>>,
    )
    where
        'a: 'b,
    {
        let mut collateral_transfers: Vec<&ScaledTokenEvent<'a>> = Vec::new();
        let mut collateral_burn: Option<&ScaledTokenEvent<'a>> = None;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            // Match collateral events only if they belong to this liquidation's
            // collateral asset (parity: prevents incorrect matching when a user
            // is liquidated multiple times with different collateral assets).
            if let Some(expected) = collateral_a_token_address {
                if ev.token_address != expected {
                    continue;
                }
            }
            if ev.event_type == ScaledTokenEventType::CollateralBurn && ev.user_address == user {
                collateral_burn = Some(ev);
            } else if (ev.event_type == ScaledTokenEventType::CollateralTransfer
                || ev.event_type == ScaledTokenEventType::Erc20CollateralTransfer)
                && ev.user_address == user
            {
                // mirror the LC-op collateral-transfer filter
                // Liquidation-op filter — in Liquidation
                // ops, ALL ERC20 CollateralTransfers (`Erc20CollateralTransfer`
                // variant — index=None, the standard ERC20 Transfer event emitted
                // by the aToken) are SKIPPED. The user's collateral debit is
                // captured by the Burn event + the protocol-fee balance move by
                // the paired BalanceTransfer (`CollateralTransfer` variant —
                // index=non-None, preserved below). Without this guard the
                // `Erc20CollateralTransfer(user→treasury, fee)` emitted alongside
                // the LiquidationCall over-debits the user / over-credits the
                // treasury by exactly the `liquidationProtocolFee × bonus /
                // (1+bonus)` share — empirically verified on Rust-written
                // 16591070 prod DB for users 0x23dB (id 134) + 0x87A6 (id 1018)
                // WETH positions (residuals byte-match the on-chain ERC20
                // Transfer amounts; ratio = 0.476% = 0.10×0.05/1.05).
                //
                // Context confirmation (per orchestrator reminder (a)):
                // `collect_collateral_events` is LC-only — sole caller is
                // `create_liquidation_operation`; the unconditional skip CANNOT
                // over-filter non-LC ERC20 Transfers.
                //
                // Marked `assigned` so the standalone Step-4e Transfer path
                // (`create_transfer_operations`) doesn't re-collect it — same
                // write-back as the 1cf6578d `is_part_of_burn` / `is_part_of_mint`
                // guards below. The 1cf6578d ZERO-address guards below remain
                // ACTIVE for the `CollateralTransfer` (BalanceTransfer) variant
                // (target/from == ZERO is degenerate for BT events but defensive).
                if ev.event_type == ScaledTokenEventType::Erc20CollateralTransfer {
                    assigned_indices.insert(ev.log_index);
                    continue;
                }
                // EIWEPM: skip the burn-side pair ERC20 Transfer to ZERO (the
                // Burn event itself is the sole operational debit), and the
                // mint-side pair ERC20 Transfer from ZERO (the Mint event is
                // the sole operational credit). Matches the standalone Transfer
                // path's `is_part_of_burn` / `is_part_of_mint` filter exact
                // semantics (user-address + token-address match — no value
                // heuristic; the reference's matching contract).
                if ev.target_address == Some(Address::ZERO)
                    && is_part_of_burn(ev, scaled_events, assigned_indices)
                {
                    continue;
                }
                if ev.from_address == Some(Address::ZERO)
                    && is_part_of_mint(ev, scaled_events, assigned_indices)
                {
                    continue;
                }
                collateral_transfers.push(ev);
            }
        }
        (collateral_burn, collateral_transfers)
    }

    /// the gnarly ~160-LoC
    /// fn that builds an `Operation { operation_type: Liquidation, ... }` (or
    /// `GhoLiquidation` if the debt asset is GHO). Coordinates:
    /// - `analyze_liquidation_scenarios` + `analyze_user_liquidation_count` (the
    ///   pre-analysis dicts over `all_events`).
    /// - `collect_debt_burns` (the `SINGLE/COMBINED_BURN/SEPARATE_BURNS` detection).
    /// - `collect_collateral_events` (the `collateral_burn` vs `collateral_transfers` split).
    /// - the `debt_mint` net-debt-increase branch (accrued interest > repayment).
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::Substrate`] if either the aToken or vToken sibling
    /// lookup fails (parity: `assert debt_v_token_address is not None`).
    #[expect(clippy::too_many_lines)] // 5-nested-branch fn — intrinsic
    pub(super) fn create_liquidation_operation(
        &self,
        operation_id: u32,
        liquidation_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        all_events: &[&'a Log],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let topics = liquidation_event.topics();
        let collateral_asset = Address::from_word(topics[1]);
        let debt_asset = Address::from_word(topics[2]);
        let user = Address::from_word(topics[3]);
        // Extract debtToCover from LiquidationCall event data (word 0).
        let data = liquidation_event.data().data.as_ref();
        let mut buf = [0u8; 32];
        if data.len() >= 32 {
            buf.copy_from_slice(&data[0..32]);
        }
        let debt_to_cover = U256::from_be_bytes::<32>(buf);
        let is_gho = self.gho_token_address == Some(debt_asset);
        let collateral_a_token_address = self.get_a_token_for_asset(collateral_asset)?;
        let debt_v_token_address = self.get_v_token_for_asset(debt_asset)?.ok_or_else(|| {
            ParseError::Substrate(format!(
                "no vToken for debt_asset {debt_asset} in market {}",
                self.market_id
            ))
        })?;
        // Pre-analyze liquidations to detect multi-liquidation scenarios.
        let liquidation_analysis = self.analyze_liquidation_scenarios(all_events)?;
        let user_liquidation_analysis = Self::analyze_user_liquidation_count(all_events);
        let user_liquidation_count = user_liquidation_analysis.get(&user).copied().unwrap_or(1);
        // Calculate this liquidation's position among all liquidations for
        // this (user, debt_asset) — sequential matching; burn[i] belongs to
        // liquidation[i]. Count LiquidationCall events before this one with
        // the same (user, debt_asset).
        let this_log_index = log_idx_value(liquidation_event);
        let mut liquidation_position = 0usize;
        for ev in all_events {
            if ev.topics().first() != Some(&aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC) {
                continue;
            }
            if ev.topics().len() < 4 {
                continue;
            }
            if Address::from_word(ev.topics()[3]) != user {
                continue;
            }
            if Address::from_word(ev.topics()[2]) != debt_asset {
                continue;
            }
            if log_idx_value(ev) < this_log_index {
                liquidation_position += 1;
            }
        }
        // Collect debt burns (SINGLE/COMBINED_BURN/SEPARATE_BURNS detection).
        let debt_burns = Self::collect_debt_burns(
            user,
            Some(debt_v_token_address),
            scaled_events,
            assigned_indices,
            &liquidation_analysis,
            user_liquidation_count,
            liquidation_position,
        );
        // Collect collateral events (burns + transfers).
        let (collateral_burn, collateral_transfers) = Self::collect_collateral_events(
            user,
            collateral_a_token_address,
            scaled_events,
            assigned_indices,
        );
        // A liquidation requires at least one collateral event.
        if collateral_burn.is_none() && collateral_transfers.is_empty() {
            return Err(ParseError::NoMatch(format!(
                "Expected at least 1 collateral event (burn or transfer) for liquidation. \
                 User: {user}, scaled_events log indices: {}",
                scaled_events
                    .iter()
                    .map(|e| e.log_index.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        // Find debt mint events that represent net debt increase during
        // liquidation (when accrued interest > debt repayment:
        // balance_increase > amount).
        let mut debt_mint: Option<&ScaledTokenEvent<'a>> = None;
        for scaled_event in scaled_events {
            if assigned_indices.contains(&scaled_event.log_index) {
                continue;
            }
            if scaled_event.user_address != user {
                continue;
            }
            if is_gho && scaled_event.event_type != ScaledTokenEventType::GhoDebtMint {
                continue;
            }
            if !is_gho && scaled_event.event_type != ScaledTokenEventType::DebtMint {
                continue;
            }
            // Match debt mint events only if they belong to this liquidation's
            // debt asset (parity: assert event_token_address == debt_v_token_address).
            if scaled_event.token_address != debt_v_token_address {
                continue;
            }
            let bal_inc = scaled_event.balance_increase.unwrap_or(U256::ZERO);
            debug_assert!(
                bal_inc > scaled_event.amount,
                "parity: balance_increase > amount for net-debt-increase mint"
            );
            debt_mint = Some(scaled_event);
            break;
        }
        // Assemble scaled_token_events + balance_transfer_events.
        let mut scaled_token_events: Vec<ScaledTokenEvent<'a>> = Vec::new();
        let mut balance_transfer_events: Vec<&'a Log> = Vec::new();
        // Note: debt_burns may be empty for flash loan liquidations or when
        // interest > repayment; debt_mint is set when interest > repayment;
        // collateral_burn may be None when collateral is transferred to treasury.
        if let Some(burn) = collateral_burn {
            scaled_token_events.push(clone_scaled_event(burn));
        }
        // Add all debt burns (primary and secondary).
        for burn in &debt_burns {
            scaled_token_events.push(clone_scaled_event(burn));
        }
        if let Some(mint) = debt_mint {
            scaled_token_events.push(clone_scaled_event(mint));
        }
        if !collateral_transfers.is_empty() {
            // Add all collateral transfers to scaled_token_events. Both
            // ERC20 Transfers (index=0) and BalanceTransfer events (index>0)
            // are collateral events that should be validated together.
            for transfer in &collateral_transfers {
                scaled_token_events.push(clone_scaled_event(transfer));
                // Track BalanceTransfer events separately so ERC20 Transfers
                // can use them for proper scaling during processing.
                let idx = transfer.index.unwrap_or(U256::ZERO);
                if !idx.is_zero() {
                    balance_transfer_events.push(transfer.log);
                }
            }
        }
        let op_type = if is_gho {
            OperationType::GhoLiquidation
        } else {
            OperationType::Liquidation
        };
        Ok(Operation {
            operation_id,
            operation_type: op_type,
            pool_revision: self.pool_revision,
            pool_event: Some(liquidation_event),
            scaled_events: scaled_token_events,
            transfer_events: Vec::new(),
            balance_transfer_events,
            minted_to_treasury_amount: None,
            debt_to_cover: Some(debt_to_cover),
            validation_errors: Vec::new(),
        })
    }
}
