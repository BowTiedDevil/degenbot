use super::util::{assigned_log_idx_for_event, clone_scaled_event, clone_scaled_event_from_ref};
use super::{
    addr_to_hex, log_idx_value, parse_address, Address, DegenbotDb, HashSet, Log, Operation,
    OperationType, ParseError, ScaledTokenEvent, ScaledTokenEventType, TransactionOperationsParser,
    U256,
};

impl<'a> TransactionOperationsParser<'a> {
    // ── the 9 standard builders ─────────────────────────────────────────

    /// Pool Supply → match `CollateralMint`
    /// by `onBehalfOf` + amount (value - `balance_increase` vs `supply_amount`).
    #[expect(clippy::panic_in_result_fn)] // deliberate panic guard surfaces a bad state lazily
    pub(super) fn create_supply_operation(
        &self,
        operation_id: u32,
        supply_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let decoded = decode_supply_pool_event(supply_event)
            .ok_or_else(|| ParseError::Substrate("malformed Supply event".into()))?;
        // Resolve the expected aToken address.
        let expected_a_token = self
            .get_a_token_for_asset(decoded.reserve)?
            .ok_or_else(|| {
                ParseError::Substrate(format!(
                    "no aToken for reserve {} in market {}",
                    decoded.reserve, self.market_id
                ))
            })?;

        // Find the matching CollateralMint event.
        let mut collateral_mint: Option<&ScaledTokenEvent<'a>> = None;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralMint {
                continue;
            }
            if ev.token_address != expected_a_token {
                continue;
            }
            if ev.user_address != decoded.on_behalf_of {
                continue;
            }
            let balance_increase = ev.balance_increase.unwrap_or(U256::ZERO);
            let calculated_principal = ev.amount - balance_increase;
            if !Self::amounts_match(calculated_principal, decoded.amount, self.pool_revision) {
                continue;
            }
            collateral_mint = Some(ev);
            break;
        }
        let collateral_mint = collateral_mint.ok_or_else(|| {
            ParseError::NoMatch(format!(
                "SUPPLY at log={} missing CollateralMint",
                log_idx_value(supply_event)
            ))
        })?;

        // Look for matching Transfer event (mint-from-zero on the same aToken).
        let mut transfer_events: Vec<&'a Log> = Vec::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.from_address != Some(Address::ZERO) {
                continue;
            }
            if ev.target_address != Some(decoded.on_behalf_of) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralTransfer
                && ev.event_type != ScaledTokenEventType::Erc20CollateralTransfer
            {
                continue;
            }
            if ev.amount == collateral_mint.amount {
                transfer_events.push(ev.log);
                break;
            }
        }
        assert_eq!(transfer_events.len(), 1, "SUPPLY expects 1 Transfer");

        assigned_log_idx_for_event(assigned_indices, collateral_mint);
        for l in &transfer_events {
            assigned_indices.insert(log_idx_value(l));
        }

        Ok(Operation {
            operation_id,
            operation_type: OperationType::Supply,
            pool_revision: self.pool_revision,
            pool_event: Some(supply_event),
            scaled_events: vec![clone_scaled_event(collateral_mint)],
            transfer_events,
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    /// Pool Withdraw → match `CollateralBurn`
    /// by user + amount (value + `balance_increase` vs `withdraw_amount`). The
    /// "interest-exceeds-withdrawal → Mint instead of Burn" branch is the
    /// §4.2-drift edge — verify plumbing equivalence.
    #[expect(clippy::too_many_lines)] // mirror's body intrinsic — §4.2-drift match has 6 branches
    pub(super) fn create_withdraw_operation(
        &self,
        operation_id: u32,
        withdraw_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let decoded = decode_withdraw_pool_event(withdraw_event)
            .ok_or_else(|| ParseError::Substrate("malformed Withdraw event".into()))?;
        let expected_a_token = self
            .get_a_token_for_asset(decoded.reserve)?
            .ok_or_else(|| {
                ParseError::Substrate(format!(
                    "no aToken for reserve {} in market {}",
                    decoded.reserve, self.market_id
                ))
            })?;

        // First try a CollateralBurn.
        let mut collateral_burn: Option<&ScaledTokenEvent<'a>> = None;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralBurn {
                continue;
            }
            if ev.token_address != expected_a_token {
                continue;
            }
            if ev.user_address != decoded.user {
                continue;
            }
            let balance_increase = ev.balance_increase.unwrap_or(U256::ZERO);
            let calculated_burn = ev.amount + balance_increase;
            if !Self::amounts_match(calculated_burn, decoded.amount, self.pool_revision) {
                continue;
            }
            collateral_burn = Some(ev);
            break;
        }

        // Fallback: "interest exceeds withdrawal → Mint" branch.
        let mut interest_mint: Option<&ScaledTokenEvent<'a>> = None;
        if collateral_burn.is_none() {
            for ev in scaled_events {
                if assigned_indices.contains(&ev.log_index) {
                    continue;
                }
                if ev.event_type != ScaledTokenEventType::CollateralMint {
                    continue;
                }
                if ev.token_address != expected_a_token {
                    continue;
                }
                interest_mint = Some(ev);
                break;
            }
        }
        if collateral_burn.is_none() && interest_mint.is_none() {
            return Err(ParseError::NoMatch(format!(
                "WITHDRAW at log={} missing CollateralBurn + interest-Mint",
                log_idx_value(withdraw_event)
            )));
        }

        // Find the matching Transfer event (mint → from-zero; burn → to-zero).
        let mut transfer_event: Option<&'a Log> = None;
        if let Some(interest_mint_ev) = interest_mint {
            // Mint → Transfer (CreditTransfer) from any addr — Python `_create_withdraw_operation`
            // interest_mint-branch: search any (CollateralTransfer / ERC20_COLLATERAL_TRANSFER).
            for ev in scaled_events {
                if assigned_indices.contains(&ev.log_index)
                    || ev.log_index == interest_mint_ev.log_index
                {
                    continue;
                }
                if ev.event_type != ScaledTokenEventType::CollateralTransfer
                    && ev.event_type != ScaledTokenEventType::Erc20CollateralTransfer
                {
                    continue;
                }
                if ev.token_address != expected_a_token {
                    continue;
                }
                transfer_event = Some(ev.log);
                break;
            }
        } else if let Some(burn) = collateral_burn {
            // Burn → Transfer (Transfer-to-zero).
            for ev in scaled_events {
                if assigned_indices.contains(&ev.log_index) || ev.log_index == burn.log_index {
                    continue;
                }
                if ev.event_type != ScaledTokenEventType::CollateralTransfer
                    && ev.event_type != ScaledTokenEventType::Erc20CollateralTransfer
                {
                    continue;
                }
                if ev.token_address != expected_a_token {
                    continue;
                }
                if ev.target_address != Some(Address::ZERO) {
                    continue;
                }
                transfer_event = Some(ev.log);
                break;
            }
        }
        let transfer_event = transfer_event.ok_or_else(|| {
            ParseError::NoMatch(format!(
                "WITHDRAW at log={} missing transfer event",
                log_idx_value(withdraw_event)
            ))
        })?;

        let scaled_token_events: Vec<ScaledTokenEvent<'a>> = if let Some(im) = interest_mint {
            vec![clone_scaled_event(im)]
        } else {
            #[expect(clippy::unwrap_used)] // interest_mint absent ⇒ collateral_burn is Some
            let burn = collateral_burn.unwrap();
            vec![clone_scaled_event(burn)]
        };

        // Mark assignments.
        for ev in &scaled_token_events {
            assigned_indices.insert(ev.log_index);
        }
        assigned_indices.insert(log_idx_value(transfer_event));

        Ok(Operation {
            operation_id,
            operation_type: OperationType::Withdraw,
            pool_revision: self.pool_revision,
            pool_event: Some(withdraw_event),
            scaled_events: scaled_token_events,
            transfer_events: vec![transfer_event],
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    /// Pool Borrow → match `DebtMint` (or
    /// `GhoDebtMint` if the reserve is the GHO token). Includes GHO-BORROW
    /// detection.
    #[expect(clippy::panic_in_result_fn)] // deliberate panic guard surfaces a bad state lazily
    pub(super) fn create_borrow_operation(
        &self,
        operation_id: u32,
        borrow_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let decoded = decode_borrow_pool_event(borrow_event)
            .ok_or_else(|| ParseError::Substrate("malformed Borrow event".into()))?;
        let is_gho = self.gho_token_address == Some(decoded.reserve);

        let expected_event_type = if is_gho {
            ScaledTokenEventType::GhoDebtMint
        } else {
            ScaledTokenEventType::DebtMint
        };

        let mut debt_mint: Option<&ScaledTokenEvent<'a>> = None;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.user_address != decoded.on_behalf_of {
                continue;
            }
            if ev.event_type != expected_event_type {
                continue;
            }
            let balance_increase = ev.balance_increase.unwrap_or(U256::ZERO);
            let calculated_borrow = ev.amount - balance_increase;
            if !Self::amounts_match(calculated_borrow, decoded.amount, self.pool_revision) {
                continue;
            }
            debt_mint = Some(ev);
            break;
        }
        let debt_mint = debt_mint.ok_or_else(|| {
            ParseError::NoMatch(format!(
                "BORROW at log={} missing DebtMint",
                log_idx_value(borrow_event)
            ))
        })?;

        // Look for matching Transfer event from ZERO_ADDRESS.
        let mut transfer_events: Vec<&'a Log> = Vec::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.from_address != Some(Address::ZERO) {
                continue;
            }
            if ev.target_address != Some(decoded.on_behalf_of) {
                continue;
            }
            if ev.amount != debt_mint.amount {
                continue;
            }
            transfer_events.push(ev.log);
            break;
        }
        assert_eq!(transfer_events.len(), 1, "BORROW expects 1 Transfer");

        let op_type = if is_gho {
            OperationType::GhoBorrow
        } else {
            OperationType::Borrow
        };
        assigned_log_idx_for_event(assigned_indices, debt_mint);
        for l in &transfer_events {
            assigned_indices.insert(log_idx_value(l));
        }

        Ok(Operation {
            operation_id,
            operation_type: op_type,
            pool_revision: self.pool_revision,
            pool_event: Some(borrow_event),
            scaled_events: vec![clone_scaled_event(debt_mint)],
            transfer_events,
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    /// Pool Repay → match `DebtBurn` (or
    /// `GhoDebtBurn`), dispatching to `_create_repay_with_atokens_operation` if
    /// `useATokens=true`.
    #[expect(clippy::panic_in_result_fn)] // deliberate panic guard surfaces a bad state lazily
    pub(super) fn create_repay_operation(
        &self,
        operation_id: u32,
        repay_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let decoded = decode_repay_pool_event(repay_event)
            .ok_or_else(|| ParseError::Substrate("malformed Repay event".into()))?;
        let is_gho = self.gho_token_address == Some(decoded.reserve);
        if decoded.use_a_tokens {
            assert!(!is_gho, "REPAY_WITH_ATOKENS for GHO is impossible");
            return self.create_repay_with_atokens_operation(
                operation_id,
                repay_event,
                decoded.reserve,
                decoded.user,
                decoded.amount,
                scaled_events,
                assigned_indices,
            );
        }
        // Standard REPAY path.
        let principal_repay_event = self.find_principal_repay_event(
            decoded.amount,
            is_gho,
            scaled_events,
            assigned_indices,
        )?;
        // Find debt Transfer-to-zero for the matched principal.
        let transfer_events = self.find_debt_transfer_to_zero(
            decoded.user,
            principal_repay_event.amount,
            scaled_events,
            assigned_indices,
        );
        let op_type = if is_gho {
            OperationType::GhoRepay
        } else {
            OperationType::Repay
        };
        assigned_log_idx_for_event(assigned_indices, principal_repay_event);
        for l in &transfer_events {
            assigned_indices.insert(log_idx_value(l));
        }
        Ok(Operation {
            operation_id,
            operation_type: op_type,
            pool_revision: self.pool_revision,
            pool_event: Some(repay_event),
            scaled_events: vec![clone_scaled_event_from_ref(principal_repay_event)],
            transfer_events,
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    /// _`find_principal_repay_event`
    /// + _`find_collateral_adjustment_event` (the paired vToken-Burn +
    ///   aToken-Transfer matching). GHO repayment with aTokens is impossible
    ///   (asserted in caller).
    #[expect(clippy::too_many_arguments)]
    fn create_repay_with_atokens_operation(
        &self,
        operation_id: u32,
        repay_event: &'a Log,
        reserve: Address,
        user: Address,
        repay_amount: U256,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let principal_repay_event =
            self.find_principal_repay_event(repay_amount, false, scaled_events, assigned_indices)?;
        let collateral_adjustment_event = self.find_collateral_adjustment_event(
            user,
            reserve,
            repay_amount,
            scaled_events,
            assigned_indices,
        )?;
        assigned_log_idx_for_event(assigned_indices, principal_repay_event);
        assigned_log_idx_for_event(assigned_indices, collateral_adjustment_event);
        Ok(Operation {
            operation_id,
            operation_type: OperationType::RepayWithAtokens,
            pool_revision: self.pool_revision,
            pool_event: Some(repay_event),
            scaled_events: vec![
                clone_scaled_event_from_ref(principal_repay_event),
                clone_scaled_event_from_ref(collateral_adjustment_event),
            ],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    // ── the `_find_*` helpers ────────────────────────────────────────

    /// For REPAY: match either Burn
    /// (amount + `balance_increase` == `repay_amount`) or Mint (`balance_increase`
    /// - amount == `repay_amount` — interest > repayment path).
    fn find_principal_repay_event<'b>(
        &self,
        repay_amount: U256,
        is_gho: bool,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &HashSet<u64>,
    ) -> Result<&'b ScaledTokenEvent<'a>, ParseError>
    where
        'a: 'b,
    {
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            let valid_types = if is_gho {
                (
                    ScaledTokenEventType::GhoDebtBurn,
                    ScaledTokenEventType::GhoDebtMint,
                )
            } else {
                (
                    ScaledTokenEventType::DebtBurn,
                    ScaledTokenEventType::DebtMint,
                )
            };
            if ev.event_type != valid_types.0 && ev.event_type != valid_types.1 {
                continue;
            }
            let bal_inc = ev.balance_increase.unwrap_or(U256::ZERO);
            let calculated = if ev.event_type == valid_types.0
                || ev.event_type == ScaledTokenEventType::GhoDebtBurn
            {
                // Burn: amount + balance_increase.
                ev.amount + bal_inc
            } else {
                // Mint: balance_increase - amount.
                bal_inc - ev.amount
            };
            if !Self::amounts_match(calculated, repay_amount, self.pool_revision) {
                continue;
            }
            return Ok(ev);
        }
        Err(ParseError::NoMatch(
            "no matching principal repay event".into(),
        ))
    }

    /// `REPAY_WITH_ATOKENS` paired
    /// vToken-Burn + aToken-Transfer matching). Both Burn + Mint branches
    /// (the interest-exceeds-repayment edge).
    fn find_collateral_adjustment_event<'b>(
        &self,
        user: Address,
        reserve: Address,
        expected_amount: U256,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &HashSet<u64>,
    ) -> Result<&'b ScaledTokenEvent<'a>, ParseError>
    where
        'a: 'b,
    {
        let expected_a_token = self.get_a_token_for_asset(reserve)?.ok_or_else(|| {
            ParseError::Substrate(format!(
                "no aToken for reserve {reserve} in market {}",
                self.market_id
            ))
        })?;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralBurn
                && ev.event_type != ScaledTokenEventType::CollateralMint
            {
                continue;
            }
            if ev.user_address != user {
                continue;
            }
            let _ = expected_a_token; // Boundary: aToken-address match could be
                                      // added if multiple collateral tokens exist for a single user;
                                      // matching by `user` + amount is the Python's primary match
                                      // (it also doesn't filter by token contract explicitly here).
            let bal_inc = ev.balance_increase.unwrap_or(U256::ZERO);
            let adjustment = if ev.event_type == ScaledTokenEventType::CollateralMint {
                bal_inc - ev.amount
            } else {
                ev.amount + bal_inc
            };
            if !Self::amounts_match(adjustment, expected_amount, self.pool_revision) {
                continue;
            }
            return Ok(ev);
        }
        Err(ParseError::NoMatch(
            "no matching collateral adjustment event".into(),
        ))
    }

    /// Find debt transfer event to
    /// zero address matching the principal burn amount. Returns 0 or 1 events.
    #[expect(clippy::unused_self)] // mirror's symmetric API to siblings
    fn find_debt_transfer_to_zero<'b>(
        &self,
        user: Address,
        amount: U256,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &HashSet<u64>,
    ) -> Vec<&'a Log>
    where
        'a: 'b,
    {
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::DebtTransfer
                && ev.event_type != ScaledTokenEventType::Erc20DebtTransfer
                && ev.event_type != ScaledTokenEventType::GhoDebtTransfer
            {
                continue;
            }
            if ev.from_address != Some(user) {
                continue;
            }
            if ev.amount != amount {
                continue;
            }
            return vec![ev.log];
        }
        Vec::new()
    }

    /// Resolves the aToken address
    /// for an underlying asset (None if not a market asset). Uses the
    /// [`DegenbotDb::lookup_asset_by_underlying_address_on_conn`] substrate
    /// (the §3 surface — no ad-hoc SQL JOINs in the parser).
    pub(super) fn get_a_token_for_asset(
        &self,
        underlying: Address,
    ) -> Result<Option<Address>, ParseError> {
        let row = DegenbotDb::lookup_asset_by_underlying_address_on_conn(
            self.conn,
            self.market_id,
            &addr_to_hex(underlying),
        )?;
        Ok(row.and_then(|a| parse_address(&a.a_token_address)))
    }

    /// Resolves the vToken address
    /// for an underlying debt asset (None if not a market asset). Required by
    /// `_create_liquidation_operation`'s `SEPARATE_BURNS` pattern detection.
    pub(super) fn get_v_token_for_asset(
        &self,
        underlying: Address,
    ) -> Result<Option<Address>, ParseError> {
        let row = DegenbotDb::lookup_asset_by_underlying_address_on_conn(
            self.conn,
            self.market_id,
            &addr_to_hex(underlying),
        )?;
        Ok(row.and_then(|a| parse_address(&a.v_token_address)))
    }
}

// ── pool-event decode helpers ─────────────────────────────────────────────
//
// Tiny structure-only decoders for Pool-event fields the parser-matching needs
// (user + reserve address + amount). These bypass the full decode (which
// returns many fields the parser doesn't use) — direct topic/data slicing for
// the literal fields every builder needs.

struct SupplyPoolDecoded {
    reserve: Address,
    on_behalf_of: Address,
    amount: U256,
}
struct WithdrawPoolDecoded {
    reserve: Address,
    user: Address,
    amount: U256,
}
struct BorrowPoolDecoded {
    reserve: Address,
    on_behalf_of: Address,
    amount: U256,
}
struct RepayPoolDecoded {
    reserve: Address,
    user: Address,
    amount: U256,
    use_a_tokens: bool,
}

fn decode_supply_pool_event(log: &Log) -> Option<SupplyPoolDecoded> {
    // Real Aave V3 Supply: 4 topics [sig, reserve, onBehalfOf, referralCode].
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let reserve = Address::from_word(topics[1]);
    let on_behalf_of = Address::from_word(topics[2]);
    let data = log.data().data.as_ref();
    // data = abi.encode(address user, uint256 amount) — amount is word 1.
    if data.len() < 64 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[32..64]);
    let amount = U256::from_be_bytes::<32>(buf);
    Some(SupplyPoolDecoded {
        reserve,
        on_behalf_of,
        amount,
    })
}

fn decode_withdraw_pool_event(log: &Log) -> Option<WithdrawPoolDecoded> {
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let reserve = Address::from_word(topics[1]);
    let user = Address::from_word(topics[2]);
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[0..32]);
    let amount = U256::from_be_bytes::<32>(buf);
    Some(WithdrawPoolDecoded {
        reserve,
        user,
        amount,
    })
}

fn decode_borrow_pool_event(log: &Log) -> Option<BorrowPoolDecoded> {
    // Real Aave V3 Borrow: 4 topics [sig, reserve, onBehalfOf, referralCode].
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let reserve = Address::from_word(topics[1]);
    let on_behalf_of = Address::from_word(topics[2]);
    let data = log.data().data.as_ref();
    // data = abi.encode(address user, uint256 amount, uint8 mode, uint256 rate)
    if data.len() < 128 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[32..64]);
    let amount = U256::from_be_bytes::<32>(buf);
    Some(BorrowPoolDecoded {
        reserve,
        on_behalf_of,
        amount,
    })
}

fn decode_repay_pool_event(log: &Log) -> Option<RepayPoolDecoded> {
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let reserve = Address::from_word(topics[1]);
    let user = Address::from_word(topics[2]);
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[0..32]);
    let amount = U256::from_be_bytes::<32>(buf);
    // bool use_a_tokens at byte 31 of word 1.
    let use_a_tokens = data[63] != 0;
    Some(RepayPoolDecoded {
        reserve,
        user,
        amount,
        use_a_tokens,
    })
}
