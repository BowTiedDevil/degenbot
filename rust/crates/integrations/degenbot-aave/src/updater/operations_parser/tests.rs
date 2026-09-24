use super::*;
use crate::operations::{LiquidationGroup, LiquidationPattern};

#[test]
fn amounts_match_exact_below_threshold() {
    // pool_revision < 9 → exact match required.
    assert!(TransactionOperationsParser::amounts_match(
        U256::from(100),
        U256::from(100),
        5
    ));
    assert!(!TransactionOperationsParser::amounts_match(
        U256::from(99),
        U256::from(100),
        5
    ));
}

#[test]
fn amounts_match_tolerance_at_or_above_threshold() {
    // pool_revision >= 9 → ±TOKEN_AMOUNT_MATCH_TOLERANCE (2).
    assert!(TransactionOperationsParser::amounts_match(
        U256::from(100),
        U256::from(102),
        9
    ));
    assert!(TransactionOperationsParser::amounts_match(
        U256::from(100),
        U256::from(98),
        9
    ));
    assert!(!TransactionOperationsParser::amounts_match(
        U256::from(100),
        U256::from(103),
        9
    ));
}

#[test]
fn are_compatible_transfer_types_pairings() {
    use ScaledTokenEventType::*;
    assert!(TransactionOperationsParser::are_compatible_transfer_types(
        CollateralTransfer,
        Erc20CollateralTransfer,
    ));
    assert!(TransactionOperationsParser::are_compatible_transfer_types(
        DebtTransfer,
        Erc20DebtTransfer,
    ));
    // cross-token kinds are NOT compatible.
    assert!(!TransactionOperationsParser::are_compatible_transfer_types(
        CollateralTransfer,
        Erc20DebtTransfer,
    ));
}

#[test]
fn scaled_token_event_type_predicates() {
    use ScaledTokenEventType::*;
    assert!(CollateralMint.is_collateral());
    assert!(CollateralBurn.is_burn());
    assert!(DebtMint.is_debt());
    assert!(Erc20CollateralTransfer.is_collateral());
    assert!(!Erc20CollateralTransfer.is_burn());
    assert!(GhoDebtMint.is_debt());
    assert!(CollateralInterestBurn.is_collateral());
}

#[test]
fn operation_event_log_indices_dedupes() {
    // Operation.event_log_indices returns the unique log-index set across
    // pool_event + scaled_events + transfer_events + balance_transfer_events
    // — the parse() loop uses this to track which logs have been
    // consumed so subsequent operations skip them.
    let pool_log = test_log(1, &aave_event_decoder::AAVE_SUPPLY_TOPIC, &[]);
    let mut op = Operation {
        operation_id: 0,
        operation_type: OperationType::Supply,
        pool_revision: 9,
        pool_event: Some(&pool_log),
        scaled_events: Vec::new(),
        transfer_events: vec![&pool_log], // duplicate — must be deduped.
        balance_transfer_events: Vec::new(),
        minted_to_treasury_amount: None,
        debt_to_cover: None,
        validation_errors: Vec::new(),
    };
    op.transfer_events = vec![&pool_log];
    let indices = op.event_log_indices();
    assert_eq!(indices, vec![1]);
}

// helper: construct a minimal Log for tests.
fn test_log(idx: u64, topic0: &alloy::primitives::B256, _data: &[u8]) -> Log {
    use alloy::primitives::{Bytes, Log as AlloyLog};
    let inner = AlloyLog::new_unchecked(Address::ZERO, vec![*topic0], Bytes::default());
    Log {
        inner,
        block_hash: None,
        block_number: None,
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: Some(idx),
        removed: false,
    }
}

// ── liquidation engine tests (§4.2 zero-drift surface) ──────────────
//
// The static pattern-detection fns (`analyze_user_liquidation_count`,
// `collect_debt_burns`, `collect_collateral_events`) are the §4.2-drift
// critical bit — covered here. The full `create_liquidation_operation`
// builder needs an in-memory DB substrate (the `get_a_token_for_asset` /
// `get_v_token_for_asset` sibling lookups); an integration-test fixture
// is the dispatch glue's concern (the apply dispatch glue + the §4.2 cross-check
// is the consumer).

/// Construct a LiquidationCall-shape Log for tests: 4 topics
/// (`AAVE_LIQUIDATION_CALL_TOPIC`, `collateralAsset`, `debtAsset`, `user`) + a 4-word
/// data payload whose first word is `debt_to_cover`.
fn liquidation_call_log(
    idx: u64,
    collateral: Address,
    debt: Address,
    user: Address,
    debt_to_cover: U256,
) -> Log {
    use alloy::primitives::{Bytes, Log as AlloyLog, B256};
    let topics = vec![
        aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC,
        B256::left_padding_from(collateral.as_slice()),
        B256::left_padding_from(debt.as_slice()),
        B256::left_padding_from(user.as_slice()),
    ];
    let mut data = vec![0u8; 128];
    debt_to_cover
        .to_be_bytes::<32>()
        .iter()
        .enumerate()
        .for_each(|(i, b)| {
            data[32 + i] = *b;
        });
    let inner = AlloyLog::new_unchecked(Address::ZERO, topics, Bytes::from(data));
    Log {
        inner,
        block_hash: None,
        block_number: None,
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: Some(idx),
        removed: false,
    }
}

/// Construct a minimal `ScaledTokenEvent` for the liquidation tests. The
/// `token_address` discriminator (= `debt_v_token` / `collateral_a_token`)
/// drives the asset-specific filtering.
fn scaled_event(
    log_idx: u64,
    event_type: ScaledTokenEventType,
    user: Address,
    token_address: Address,
    amount: U256,
    index: Option<U256>,
) -> ScaledTokenEvent<'static> {
    // The Log field isn't read by the static collectors (`collect_debt_burns`/
    // `collect_collateral_events`); pass a synthetic placeholder for the lifetime.
    let log = Box::leak(Box::new(test_log(
        log_idx,
        &aave_event_decoder::AAVE_SUPPLY_TOPIC,
        &[],
    )));
    ScaledTokenEvent {
        log,
        decoded: ScaledTokenEventData::Burn {
            from: user,
            target: Address::ZERO,
            value: amount,
            balance_increase: U256::ZERO,
            index: index.unwrap_or(U256::from(1_000_000_000u64)),
        },
        event_type,
        token_address,
        user_address: user,
        caller_address: None,
        from_address: Some(user),
        target_address: Some(Address::ZERO),
        amount,
        balance_increase: Some(U256::ZERO),
        index,
        log_index: log_idx,
    }
}

/// Variant of `scaled_event` for a Transfer whose `target_address` is NOT
/// the ZERO address — i.e. a collateral movement to a liquidator (not the
/// burn-side pair ERC20 Transfer-to-ZERO). Used by `collect_collateral_events`
/// tests to assert the EIWEPM filter distinguishes the two cases.
fn transfer_to_event(
    log_idx: u64,
    event_type: ScaledTokenEventType,
    from: Address,
    target: Address,
    token_address: Address,
    amount: U256,
) -> ScaledTokenEvent<'static> {
    let log = Box::leak(Box::new(test_log(
        log_idx,
        &aave_event_decoder::AAVE_SUPPLY_TOPIC,
        &[],
    )));
    ScaledTokenEvent {
        log,
        decoded: ScaledTokenEventData::Burn {
            from,
            target,
            value: amount,
            balance_increase: U256::ZERO,
            index: U256::from(1_000_000_000u64),
        },
        event_type,
        token_address,
        user_address: from,
        caller_address: None,
        from_address: Some(from),
        target_address: Some(target),
        amount,
        balance_increase: Some(U256::ZERO),
        index: Some(U256::from(1_000_000_000u64)),
        log_index: log_idx,
    }
}

/// `SINGLE` pattern: one `LiquidationCall` → one debt Burn for the same user.
/// `collect_debt_burns` with `user_liquidation_count == 1` collects ALL
/// debt burns for the user (no asset filter) — handles bad-debt
/// multi-asset burns from `_burnBadDebt()`.
#[test]
fn collect_debt_burns_single_user_one_burn() {
    let user = Address::from([0xA0; 20]);
    let v_token = Address::from([0xB0; 20]);
    let burn = scaled_event(
        10,
        ScaledTokenEventType::DebtBurn,
        user,
        v_token,
        U256::from(1_000),
        Some(U256::from(1_000_000_000u64)),
    );
    let events = vec![burn];
    let mut assigned = HashSet::new();
    let analysis = HashMap::new();
    let burns = TransactionOperationsParser::collect_debt_burns(
        user,
        Some(v_token),
        &events,
        &mut assigned,
        &analysis,
        1,
        0,
    );
    assert_eq!(burns.len(), 1);
    assert_eq!(burns[0].log_index, 10);
    assert!(assigned.contains(&10));
}

/// `COMBINED_BURN` pattern: N liquidations share 1 burn — when
/// `liquidation_count_for_asset > total_burn_count` + `liquidation_position == 0`,
/// all burns go to the first liquidation.
#[test]
fn collect_debt_burns_combined_burn_pattern() {
    let user = Address::from([0xA0; 20]);
    let v_token = Address::from([0xB0; 20]);
    // 1 burn covering 2 liquidations (the COMBINED_BURN case).
    let combined_burn = scaled_event(
        100,
        ScaledTokenEventType::DebtBurn,
        user,
        v_token,
        U256::from(2_000),
        Some(U256::from(1_000_000_000u64)),
    );
    // An unrelated burn on a different vToken — must NOT be collected
    // (asset-specific filter for multi-liquidation users).
    let other_vtoken = Address::from([0xC0; 20]);
    let other_burn = scaled_event(
        200,
        ScaledTokenEventType::DebtBurn,
        user,
        other_vtoken,
        U256::from(999),
        Some(U256::from(1_000_000_000u64)),
    );
    let events = vec![combined_burn, other_burn];
    let mut assigned = HashSet::new();
    let mut analysis = HashMap::new();
    analysis.insert((user, v_token), 2usize); // 2 liquidations for this asset
                                              // First liquidation (position 0): collects the single burn.
    let burns0 = TransactionOperationsParser::collect_debt_burns(
        user,
        Some(v_token),
        &events,
        &mut assigned,
        &analysis,
        2,
        0,
    );
    assert_eq!(
        burns0.len(),
        1,
        "COMBINED_BURN: first liquidation gets the burn"
    );
    assert_eq!(burns0[0].log_index, 100);
    // Second liquidation (position 1): no burns left (combined burn goes to position 0).
    let burns1 = TransactionOperationsParser::collect_debt_burns(
        user,
        Some(v_token),
        &events,
        &mut assigned,
        &analysis,
        2,
        1,
    );
    assert!(
        burns1.is_empty(),
        "second liquidation gets no burn (already taken)"
    );
    // Unrelated-vToken burn must NEVER have been touched.
    assert!(!assigned.contains(&200));
}

/// `SEPARATE_BURNS` pattern: N liquidations → N burns, one per liquidation.
/// `liquidation_count_for_asset == total_burn_count`; burn[i] belongs to
/// liquidation[i] (sequential matching via `liquidation_position`).
#[test]
fn collect_debt_burns_separate_burns_pattern() {
    let user = Address::from([0xA0; 20]);
    let v_token = Address::from([0xB0; 20]);
    let b0 = scaled_event(
        10,
        ScaledTokenEventType::DebtBurn,
        user,
        v_token,
        U256::from(1000),
        Some(U256::from(1_000_000_000u64)),
    );
    let b1 = scaled_event(
        20,
        ScaledTokenEventType::DebtBurn,
        user,
        v_token,
        U256::from(2000),
        Some(U256::from(1_000_000_000u64)),
    );
    let events = vec![b0, b1];
    let mut assigned = HashSet::new();
    let mut analysis = HashMap::new();
    analysis.insert((user, v_token), 2usize);
    // Position 0 → first burn (log_index 10).
    let burns0 = TransactionOperationsParser::collect_debt_burns(
        user,
        Some(v_token),
        &events,
        &mut assigned,
        &analysis,
        2,
        0,
    );
    assert_eq!(burns0.len(), 1);
    assert_eq!(burns0[0].log_index, 10);
    // Position 1 → second burn (log_index 20).
    let burns1 = TransactionOperationsParser::collect_debt_burns(
        user,
        Some(v_token),
        &events,
        &mut assigned,
        &analysis,
        2,
        1,
    );
    assert_eq!(burns1.len(), 1);
    assert_eq!(burns1[0].log_index, 20);
}

/// `analyze_user_liquidation_count`: counts `LiquidationCall` events per
/// user (not per asset-pair). When `user_liquidation_count == 1`, ALL
/// debt burns for that user are collected (no asset filter — bad-debt
/// `_burnBadDebt()` across multiple debt positions).
#[test]
fn analyze_user_liquidation_count_counts_per_user() {
    let user_a = Address::from([0xA0; 20]);
    let user_b = Address::from([0xB0; 20]);
    let debt_weth = Address::from([0xC0; 20]);
    let debt_dai = Address::from([0xD0; 20]);
    let collateral = Address::from([0xE0; 20]);
    // User A: 2 liquidations (multi-asset pair).
    let a1 = liquidation_call_log(10, collateral, debt_weth, user_a, U256::from(1000));
    let a2 = liquidation_call_log(20, collateral, debt_dai, user_a, U256::from(2000));
    // User B: 1 liquidation (single, bad-debt path).
    let b1 = liquidation_call_log(30, collateral, debt_weth, user_b, U256::from(500));
    let events: Vec<&Log> = vec![&a1, &a2, &b1];
    let counts = TransactionOperationsParser::analyze_user_liquidation_count(&events);
    assert_eq!(counts.get(&user_a), Some(&2));
    assert_eq!(counts.get(&user_b), Some(&1));
}

/// `collect_collateral_events`: the `collateral_burn` vs `collateral_transfers`
/// split. When collateral is moved to the liquidator (not burned), the
/// burn is `None` + `collateral_transfers` is non-empty.
#[test]
fn collect_collateral_events_burn_vs_transfers_split() {
    let user = Address::from([0xA0; 20]);
    let a_token = Address::from([0xB0; 20]);
    let other_atoken = Address::from([0xC0; 20]);
    let liquidator = Address::from([0xD0; 20]);
    // The burn for this liquidation's aToken.
    let burn = scaled_event(
        10,
        ScaledTokenEventType::CollateralBurn,
        user,
        a_token,
        U256::from(500),
        None,
    );
    // The transfer to a non-ZERO liquidator (NOT the burn-side pair).
    //
    // this Erc20CollateralTransfer is now SKIPPED by the LC-op filter — in
    // Liquidation ops, ALL ERC20 CollateralTransfers are filtered at
    // collect-time (the filter has the same behavior: `index is None
    // AND op=LC` → skip). The liquidator's net credit comes from the Mint
    // event (when `receiveAToken=true`) or from a paired `CollateralTransfer`
    // (BalanceTransfer variant — index=non-None) — NOT via the standard
    // ERC20 Transfer event. Marked assigned so the standalone Step-4e
    // Transfer path doesn't re-collect it.
    let transfer = transfer_to_event(
        11,
        ScaledTokenEventType::Erc20CollateralTransfer,
        user,
        liquidator,
        a_token,
        U256::from(500),
    );
    // A burn on a different aToken (user multi-liquidated) — must NOT be collected.
    let other_burn = scaled_event(
        12,
        ScaledTokenEventType::CollateralBurn,
        user,
        other_atoken,
        U256::from(999),
        None,
    );
    let events = vec![burn, transfer, other_burn];
    let mut assigned = HashSet::new();
    let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
        user,
        Some(a_token),
        &events,
        &mut assigned,
    );
    assert!(
        cb.is_some(),
        "collateral_burn found for this liquidation's aToken"
    );
    assert_eq!(cb.unwrap().log_index, 10);
    assert_eq!(
        ct.len(),
        0,
        "TYS5MS: Erc20CollateralTransfer to liquidator is now SKIPPED at \
         collect-time (the LC-op filter \
         LC-op filter — ERC20 Transfers in Liquidation ops are never applied; \
         the liquidator's credit comes via the Mint event or via the \
         `CollateralTransfer` BalanceTransfer variant, not via the standard \
         ERC20 Transfer event)"
    );
    assert!(
        assigned.contains(&11),
        "TYS5MS: filtered ERC20 transfer marked assigned so the standalone \
         Step-4e Transfer path doesn't re-collect it"
    );
}

/// TYS5MS regression guard — `collect_collateral_events` MUST skip
/// `Erc20CollateralTransfer` to a NON-ZERO recipient in LC ops (the
/// Aave V3 treasury protocol-fee Transfer shape). Aave V3 `LiquidationCall`
/// transfers the protocol-fee portion from the liquidated user to the
/// treasury via `transferOnLiquidation`, emitting BOTH:
///   - `Transfer(user, treasury, fee_underlying)` (`Erc20CollateralTransfer`,
///     index=None) — the standard ERC20 Transfer event
///   - `BalanceTransfer(user, treasury, fee_scaled, liquidity_index)`
///     (`CollateralTransfer` variant, index=non-None) — Aave V3's native
///     scaled-balance move event
///
/// The apply-path collateral-transfer filter skips the
/// ERC20 variant in LC ops so the user is debited + treasury credited
/// ONCE via the BT event's `value`. Rust mirrors the filter at
/// collect-time here. RED-verified: stripping the skip-guard causes
/// the assertion to fail (Erc20 collected, `ct.len()` == 1).
#[test]
fn collect_collateral_events_skips_erc20_fee_transfer_to_treasury_in_liquidation() {
    let user = Address::from([0xA0; 20]);
    let a_token = Address::from([0xB0; 20]);
    // The Aave V3 treasury (verified on mainnet as 0x464C71f6...e18c —
    // recipient of all protocol-fee Transfers in LiquidationCall logs).
    let treasury = Address::from([0xC0; 20]);
    // Collateral Burn (user's full liquidation scaled amount minus fee).
    let burn = scaled_event(
        10,
        ScaledTokenEventType::CollateralBurn,
        user,
        a_token,
        U256::from(500_000),
        Some(U256::from(1_000_500_000u64)),
    );
    // Standard ERC20 Transfer(user→treasury, fee_underlying) emitted
    // alongside `LiquidationCall` by `transferOnLiquidation`. Without the
    // TYS5MS skip-guard, this would be DOUBLE-APPLIED (once here as the
    // fee_underlying amount, AND once via the paired BT.value below),
    // over-debiting the user + over-crediting the treasury by exactly
    // the fee_underlying amount.
    let fee_underlying_amount = U256::from(5_815_314_991_815_639u64);
    let erc20_fee_transfer = transfer_to_event(
        11,
        ScaledTokenEventType::Erc20CollateralTransfer,
        user,
        treasury,
        a_token,
        fee_underlying_amount,
    );
    // Paired Aave V3 BalanceTransfer(user→treasury, fee_scaled, index) —
    // the SCALED-balance move that Python (and Rust post-TYS5MS) applies.
    // value × index / RAY ≈ erc20_fee_transfer.amount (within tiny
    // ray-floor rounding). Modeled here via the CollateralTransfer variant
    // (index is non-None, indicating a BalanceTransfer event_type).
    let fee_scaled_amount = U256::from(5_812_238_924_384_490u64);
    let bt_fee_transfer = transfer_to_event(
        12,
        ScaledTokenEventType::CollateralTransfer,
        user,
        treasury,
        a_token,
        fee_scaled_amount,
    );
    let events = vec![burn, erc20_fee_transfer, bt_fee_transfer];
    let mut assigned = HashSet::new();
    let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
        user,
        Some(a_token),
        &events,
        &mut assigned,
    );
    // Burn must be found.
    assert_eq!(cb.unwrap().log_index, 10);
    // ONLY the BT event is collected — the ERC20 fee Transfer is skipped.
    assert_eq!(
        ct.len(),
        1,
        "TYS5MS: only the BT fee transfer is collected; \
        the Erc20CollateralTransfer(user→treasury, fee) is SKIPPED at \
        collect-time (the LC-op filter)"
    );
    assert_eq!(
        ct[0].log_index, 12,
        "the collected transfer is the BT event"
    );
    // The skipped ERC20 fee Transfer MUST be marked assigned so the
    // standalone Step-4e Transfer path doesn't re-collect it (which
    // would re-instate the double-application the TYS5MS fix prevents).
    assert!(
        assigned.contains(&11),
        "TYS5MS: the skipped Erc20CollateralTransfer to treasury IS marked \
         assigned so the standalone Step-4e Transfer path skips it; the \
         paired BT event also IS marked assigned by being collected"
    );
    // The BT event is marked assigned by being collected into collateral_transfers
    // (the caller writes assigned.insert for collected items elsewhere).
}

/// `collect_collateral_events` EIWEPM filter (per the orchestrator's
/// fix directive): a burn-side pair ERC20 Transfer-to-ZERO (the Burn
/// event's operational companion) MUST be excluded from
/// `collateral_transfers` so the Liquidation path doesn't double-debit
/// the user (the p1198-class 4.33× divergence root cause).
#[test]
fn collect_collateral_events_filters_burn_side_pair_transfer_to_zero() {
    let user = Address::from([0xA0; 20]);
    let a_token = Address::from([0xB0; 20]);
    // The CollateralBurn event.
    let burn = scaled_event(
        10,
        ScaledTokenEventType::CollateralBurn,
        user,
        a_token,
        U256::from(500),
        None,
    );
    // The paired ERC20 Transfer user→0x0 (burn-side companion — same user +
    // token, value matches). MUST be filtered by `is_part_of_burn`.
    let pair_transfer = scaled_event(
        11,
        ScaledTokenEventType::Erc20CollateralTransfer,
        user,
        a_token,
        U256::from(500),
        None,
    );
    let events = vec![burn, pair_transfer];
    let mut assigned = HashSet::new();
    let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
        user,
        Some(a_token),
        &events,
        &mut assigned,
    );
    assert!(cb.is_some(), "burn collected");
    assert_eq!(cb.unwrap().log_index, 10);
    assert_eq!(
        ct.len(),
        0,
        "burn-side pair Transfer-to-ZERO filtered (EIWEPM fix)"
    );
    assert!(
        assigned.contains(&11),
        "filtered pair-transfer marked assigned so it isn't re-collected by standalone Step-4e"
    );
}

/// `collect_collateral_events`: when no events match (collateral was fully
/// transferred, no burn), the split is `(None, [...])`. The Python's caller
/// asserts at least one event exists; tested variant separately.
#[test]
fn collect_collateral_events_no_burn_only_transfers() {
    let user = Address::from([0xA0; 20]);
    let a_token = Address::from([0xB0; 20]);
    let transfer = scaled_event(
        11,
        ScaledTokenEventType::CollateralTransfer,
        user,
        a_token,
        U256::from(500),
        Some(U256::from(1_000_000_000u64)),
    );
    let events = vec![transfer];
    let mut assigned = HashSet::new();
    let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
        user,
        Some(a_token),
        &events,
        &mut assigned,
    );
    assert!(cb.is_none(), "no burn — collateral moved via transfer");
    assert_eq!(ct.len(), 1);
}

/// `LiquidationGroup::detect_pattern` —
/// the §4.2-critical pattern classifier.
#[test]
fn liquidation_group_detect_pattern() {
    // SINGLE: 1 liquidation, 1 burn.
    let mut single = LiquidationGroup::default();
    single.liquidations.push((0, U256::from(100), 0));
    single.burn_events.push((1, U256::from(100)));
    assert_eq!(single.detect_pattern(), LiquidationPattern::Single);

    // COMBINED_BURN: 3 liquidations, 1 burn.
    let combined = LiquidationGroup {
        liquidations: vec![
            (0, U256::from(100), 0),
            (1, U256::from(200), 10),
            (2, U256::from(300), 20),
        ],
        burn_events: vec![(5, U256::from(600))],
    };
    assert_eq!(combined.detect_pattern(), LiquidationPattern::CombinedBurn);

    // SEPARATE_BURNS: 3 liquidations, 3 burns.
    let separate = LiquidationGroup {
        liquidations: vec![(0, U256::from(100), 0), (1, U256::from(200), 10)],
        burn_events: vec![(5, U256::from(100)), (6, U256::from(200))],
    };
    assert_eq!(separate.detect_pattern(), LiquidationPattern::SeparateBurns);
}

// ── MintToTreasury DP3 must NOT pre-scale ─────────────────
//
// Root cause: `create_mint_to_treasury_operations` previously
// pre-converted the MintedToTreasury `amount_minted` via `ray_div(.., HALF_UP)`
// for `pool_revision < 9`, then `dispatch_mint_to_treasury` re-applied the
// SAME `ray_div(.., HALF_UP)` on the pre-scaled value, DOUBLE-converting
// by a factor of `idx/RAY` (treasury 0x464C WETH divergence -8.04e12 was the
// diagnosis). The fix aligns Rust `op.minted_to_treasury_amount`
// semantics with Python's `operation.minted_to_treasury_amount` (= raw
// UNDERLYING amount, regardless of `pool_revision`) so the single
// conversion lives in `dispatch_mint_to_treasury`.
//
// The WETH mainnet tuple below is the cold-boot block 16516952 evidence
// used to ROOT-CAUSE the divergence — asserted by hand-math before
// querying the DB:
//   amountMinted (raw underlying) = 64_746_517_106_584_784
//   liquidity_index at emit-time   = 1_000_124_218_031_532_223_928_748_283
//   expected SINGLE ray_div result = 64_738_475_420_603_639 (Python gold
//     = on-chain truth delta: treasury WETH scaledBalance went 0 →
//     64_738_475_420_603,639 across the block boundary)
//   buggy DOUBLE ray_div result    = 64_730_434_733_420_828 (= observed
//     pre-fix Rust DB value, 8_040_687_182_811 LESS than Python gold).
//
// The test creates an in-memory DB substrate + drives BOTH the DP3 builder
// (`create_mint_to_treasury_operations`) AND the apply dispatch
// (`dispatch_mint_to_treasury`), asserting the FINAL `balance_delta` ==
// Python gold's SINGLE conversion. RED on the unfixed version (the value
// would be the DOUBLE-conversion result); GREEN on the fixed version.
fn fresh_db_with_mint_to_treasury_asset() -> degenbot_db::DegenbotDb {
    use degenbot_db::DegenbotDb;
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, 1, 'mainnet', 1, NULL)",
            [],
        )
        .unwrap();
        // Pool contract revision 8 (the bug path — rev < 9).
        conn.execute(
            "INSERT INTO aave_v3_contracts (market_id, name, address, revision) \
             VALUES (1, 'POOL', '0xpool', 8)",
            [],
        )
        .unwrap();
        // ERC20 parent rows: underlying (id 1), aToken (id 2), vToken (id 3).
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES \
             (1, 1, '0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2'), \
             (2, 1, '0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8'), \
             (3, 1, '0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE')",
            [],
        )
        .unwrap();
        // a_token_revision 1 (rev < 9 pool / token pre-9 path).
        conn.execute(
            "INSERT INTO aave_v3_assets \
                (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                 v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                 borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 1, \
                     '1000124218031532223928748283', '0', \
                     '1000000000000000000000000000', '0')",
            [],
        )
        .unwrap();
    }
    db
}

/// Compose a synthetic `MintedToTreasury` Pool log: topics[0] = the
/// `AAVE_MINTED_TO_TREASURY_TOPIC`, topics[1] = reserve (WETH mainnet);
/// data word 0 = `amountMinted` (the raw underlying amount).
fn make_minted_to_treasury_log(idx: u64, reserve: Address, amount_minted: U256) -> Log {
    use alloy::primitives::{Bytes, Log as AlloyLog, B256};
    let topics = vec![
        aave_event_decoder::AAVE_MINTED_TO_TREASURY_TOPIC,
        B256::left_padding_from(reserve.as_slice()),
    ];
    let mut data = vec![0u8; 32];
    amount_minted
        .to_be_bytes::<32>()
        .iter()
        .enumerate()
        .for_each(|(i, b)| {
            data[i] = *b;
        });
    let inner = AlloyLog::new_unchecked(
        // Pool contract address (the MintedToTreasury emitter).
        alloy::primitives::address!("0x87870Bca3F3Fd6335c3f4ce8392D69350b4fA4e2"),
        topics,
        Bytes::from(data),
    );
    Log {
        inner,
        block_hash: None,
        block_number: Some(16_516_952),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: Some(idx),
        removed: false,
    }
}

/// Construct a `CollateralMint` `ScaledTokenEvent` aux `caller_address` set to
/// the Pool contract (the `MintToTreasury` indicator that the DP3 builder
/// filters on). The chunk parser detects a Mint-to-treasury
/// (the aToken Mint event's caller == Pool).
fn mint_to_treasury_collateral_mint_event(
    log_idx: u64,
    pool_address: Address,
    a_token_address: Address,
    user: Address,
    amount: U256,
    index: U256,
) -> ScaledTokenEvent<'static> {
    let log = Box::leak(Box::new(test_log(
        log_idx,
        &aave_event_decoder::AAVE_MINT_TOPIC,
        &[],
    )));
    ScaledTokenEvent {
        log,
        decoded: ScaledTokenEventData::Mint {
            caller: pool_address,
            on_behalf_of: user,
            value: amount,
            balance_increase: U256::ZERO,
            index,
        },
        event_type: ScaledTokenEventType::CollateralMint,
        token_address: a_token_address,
        user_address: user,
        caller_address: Some(pool_address),
        from_address: Some(pool_address),
        target_address: Some(user),
        amount,
        balance_increase: Some(U256::ZERO),
        index: Some(index),
        log_index: log_idx,
    }
}

/// Regression — asserts the FULLY-FIXED pipeline (DP3 + dispatch)
/// produces the SINGLE `ray_div` result (= Python's
/// `PoolMath::underlying_to_scaled_collateral` value), NOT the
/// pre-fix DOUBLE-conversion result. Pre-fix, DP3 would pre-ray_div the
/// `amount_minted`, then `dispatch_mint_to_treasury` would re-ray_div it,
/// shrinking by `idx/RAY` (= the -8.04e12 divergence observed on the
/// treasury WETH residual).
#[test]
fn mint_to_treasury_dp3_does_not_pre_scale_then_dispatch_single_converts() {
    let db = fresh_db_with_mint_to_treasury_asset();
    let conn = db.lock();

    // The Pool contract at 0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2.
    let pool_address = alloy::primitives::address!("0x87870Bca3F3Fd6335c3f4ce8392D69350b4fA4e2");
    let a_token_address = alloy::primitives::address!("0x4d5F47fA6A74757f35C14fD3a6Ef8E3C9Bc514E8");
    let reserve_weth = alloy::primitives::address!("0xC02aAA39b223FE8D0A0e5C4F27EAD9083C756Cc2");
    // Treasury 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c (the user that
    // receives the minted-to-treasury aToken shares).
    let treasury = alloy::primitives::address!("0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c");

    // the MintedToTreasury
    // emission that mints accrued yield to the treasury 0x464C's WETH
    // position.
    let amount_minted_raw = U256::from(64_746_517_106_584_784u128);
    let liquidity_index = U256::from(1_000_124_218_031_532_223_928_748_283u128);

    // Pre-registered hand-math prediction:
    //   Python gold / on-chain truth = ray_div_halfup(raw, idx) = 64_738_475_420_603_639
    //   Pre-fix Rust (DOUBLE-conv)   = ray_div(ray_div(raw, idx), idx) = 64_730_434_733_420_828
    //   Diff = Python gold - pre-fix Rust = 8_040_687_182_811 (= the observed divergence).
    let python_gold_scaled = U256::from(64_738_475_420_603_639u128);
    let buggy_double_conv_scaled = U256::from(64_730_434_733_420_828u128);

    let mint_log = make_minted_to_treasury_log(737, reserve_weth, amount_minted_raw);
    let minted_to_treasury_events: Vec<&Log> = vec![&mint_log];

    let scaled_event = mint_to_treasury_collateral_mint_event(
        736,
        pool_address,
        a_token_address,
        treasury,
        amount_minted_raw,
        liquidity_index,
    );
    let scaled_events = vec![scaled_event];

    // Crate the parser with the in-memory DB. The Pool contract revision 8
    // is configured in `aave_v3_contracts` → `lookup_pool_revision_on_conn`
    // returns 8 (= the bug path, `pool_revision < 9`).
    let parser = TransactionOperationsParser::new(
        1, // market_id
        1, // chain_id
        pool_address,
        Some(treasury),
        None, // gho_token_address
        None, // gho_vtoken_address
        &conn,
    )
    .unwrap();

    // === DP3 STEP (the root-cause site) ===
    let mut assigned_indices: HashSet<u64> = HashSet::new();
    let mut next_op_id: u32 = 0;
    let mut ops = parser.create_mint_to_treasury_operations(
        &scaled_events,
        &mut assigned_indices,
        &mut next_op_id,
        &minted_to_treasury_events,
    );
    assert_eq!(
        ops.len(),
        1,
        "the synthetic Mint event should yield 1 MintToTreasury op"
    );

    // The fix: DP3 must store the RAW UNDERLYING amount (= amount_minted_raw),
    // NOT pre-scaled via ray_div — that scaled conversion happens EXACTLY
    // ONCE in `dispatch_mint_to_treasury`. Without the fix, this value would
    // be `Some(python_gold_scaled)` (= `ray_div(raw, idx, HALF_UP)`), which
    // the dispatch then re-ray_div's → DOUBLE conversion.
    let op = ops.pop().unwrap();
    assert_eq!(
        op.minted_to_treasury_amount,
        Some(amount_minted_raw),
        "DP3 must store the raw underlying amountMinted (NOT pre-scaled); \
         pre-fix would pre-scale to {python_gold_scaled} (= ray_div(raw, idx, HALF_UP)) which \
         then DOUBLE-converts at dispatch",
    );
    assert_eq!(op.operation_type, OperationType::MintToTreasury);

    // === DISPATCH STEP (the apply-time conversion) ===
    // `dispatch_mint_to_treasury` does the single ray_div (`= Python's
    // `PoolMath::underlying_to_scaled_collateral`). With the FIX in place,
    // the final balance_delta == python_gold_scaled.
    //
    // Pre-fix, `op.minted_to_treasury_amount` was already the SCALED value,
    // so dispatch's ray_div would produce the DOUBLE-conv result =
    // buggy_double_conv_scaled.
    let mut events: Vec<crate::run::AaveChunkEvent> = Vec::new();
    crate::transaction_processor::dispatch_mint_to_treasury(
        &op,
        1, // market_id
        &conn,
        &mut events,
    )
    .unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        crate::run::AaveChunkEvent::ScaledTokenMint { balance_delta, .. } => {
            assert_eq!(
                *balance_delta,
                alloy::primitives::I256::try_from(python_gold_scaled).unwrap(),
                "final balance_delta must equal Python gold's SINGLE ray_div result \
                 (= on-chain truth); the pre-fix DOUBLE-conversion would have \
                 produced {} ({} LESS than Python gold)",
                buggy_double_conv_scaled,
                python_gold_scaled - buggy_double_conv_scaled,
            );
        }
        other => panic!("expected AaveChunkEvent::ScaledTokenMint, got {other:?}"),
    }
}
