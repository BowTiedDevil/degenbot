use super::util::{CompanionSide, EventDisposition, EventOwnership, LiquidationWindow};
use super::*;
use crate::operations::{LiquidationGroup, LiquidationPattern};
use crate::updater::run::substrate::ChunkSubstrate;

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

/// A standard ERC20 `Transfer` log (emitter = the token contract) — the
/// classification seam's input shape.
fn erc20_transfer_log(idx: u64, token: Address, from: Address, to: Address, value: U256) -> Log {
    use alloy::primitives::{Bytes, Log as AlloyLog, B256};
    let topics = vec![
        aave_event_decoder::ERC20_TRANSFER_TOPIC,
        B256::left_padding_from(from.as_slice()),
        B256::left_padding_from(to.as_slice()),
    ];
    let mut data = vec![0u8; 32];
    value
        .to_be_bytes::<32>()
        .iter()
        .enumerate()
        .for_each(|(i, b)| data[i] = *b);
    let inner = AlloyLog::new_unchecked(token, topics, Bytes::from(data));
    Log {
        inner,
        block_hash: None,
        block_number: Some(100),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: Some(idx),
        removed: false,
    }
}

/// A market + POOL contract row — the minimum the parser's constructor
/// resolves (`pool_revision`) before any classification lookup runs.
fn fresh_db_for_classification() -> degenbot_db::DegenbotDb {
    use degenbot_db::DegenbotDb;
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let conn = db.lock();
    conn.execute(
        "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
         VALUES (1, 1, 'm', 1, NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO aave_v3_contracts (market_id, name, address, revision) \
         VALUES (1, 'POOL', '0xpool', 9)",
        [],
    )
    .unwrap();
    drop(conn);
    db
}

/// The `Ok(None)` contract: the transfer is skipped ONLY after every
/// classification lookup succeeds and proves the emitter unrelated.
#[test]
fn unrelated_transfer_skips_only_after_successful_classification() {
    let db = fresh_db_for_classification();
    let conn = db.lock();
    let log = erc20_transfer_log(
        0,
        Address::repeat_byte(0x77),
        Address::repeat_byte(0x01),
        Address::repeat_byte(0x02),
        U256::from(5),
    );
    let events = vec![&log];
    let mut substrate = ChunkSubstrate::lazy(1);
    let mut parser = TransactionOperationsParser::new(
        1,
        1,
        Address::repeat_byte(0x99),
        None,
        None,
        None,
        &conn,
        &mut substrate,
    )
    .unwrap();
    let parsed = parser.parse(&events, [0u8; 32]).unwrap();
    assert!(
        parsed.operations.is_empty(),
        "a proven-unrelated transfer is skipped, never parsed into an operation"
    );
}

/// A DB failure inside the classification lookups must abort the whole parse
/// (the chunk rolls back) — never degrade into the unrelated-token skip and
/// silently drop the transfer.
#[test]
fn parse_aborts_when_classification_db_lookup_fails() {
    let db = fresh_db_for_classification();
    let conn = db.lock();
    conn.execute("DROP TABLE aave_gho_tokens", []).unwrap();
    let log = erc20_transfer_log(
        0,
        Address::repeat_byte(0x77),
        Address::repeat_byte(0x01),
        Address::repeat_byte(0x02),
        U256::from(5),
    );
    let events = vec![&log];
    let mut substrate = ChunkSubstrate::lazy(1);
    let mut parser = TransactionOperationsParser::new(
        1,
        1,
        Address::repeat_byte(0x99),
        None,
        None,
        None,
        &conn,
        &mut substrate,
    )
    .unwrap();
    let Err(err) = parser.parse(&events, [0u8; 32]) else {
        panic!("a DB failure during classification must fail the parse, never skip the event")
    };
    assert!(matches!(err, ParseError::Substrate(_)), "got {err:?}");
}

// ── liquidation engine tests (parity-gate zero-drift surface) ──────────────
//
// The static pattern-detection fns (`analyze_user_liquidation_count`,
// `collect_debt_burns`, `collect_collateral_events`) are the parity-drift
// critical bit — covered here. The full `create_liquidation_operation`
// builder needs an in-memory DB substrate (the `get_a_token_for_asset` /
// `get_v_token_for_asset` sibling lookups); an integration-test fixture
// is the dispatch glue's concern (the apply dispatch glue + the parity-gate cross-check
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
    let scaled_index = index.unwrap_or(U256::from(1_000_000_000u64));
    let (decoded, from_address, target_address, caller_address) = match event_type {
        ScaledTokenEventType::CollateralMint
        | ScaledTokenEventType::DebtMint
        | ScaledTokenEventType::GhoDebtMint => (
            ScaledTokenEventData::Mint {
                caller: Address::ZERO,
                on_behalf_of: user,
                value: amount,
                balance_increase: U256::ZERO,
                index: scaled_index,
            },
            None,
            None,
            Some(Address::ZERO),
        ),
        ScaledTokenEventType::CollateralBurn
        | ScaledTokenEventType::DebtBurn
        | ScaledTokenEventType::GhoDebtBurn => (
            ScaledTokenEventData::Burn {
                from: user,
                target: Address::ZERO,
                value: amount,
                balance_increase: U256::ZERO,
                index: scaled_index,
            },
            Some(user),
            Some(Address::ZERO),
            None,
        ),
        other => panic!(
            "scaled_event builds semantic Mint/Burn fixtures only; use \
             balance_transfer_event/transfer_to_event for {other:?}"
        ),
    };
    ScaledTokenEvent {
        log,
        decoded,
        event_type,
        token_address,
        user_address: user,
        caller_address,
        from_address,
        target_address,
        amount,
        balance_increase: Some(U256::ZERO),
        index,
        log_index: log_idx,
    }
}

/// Variant of `scaled_event` for a Transfer whose `target_address` is NOT
/// the ZERO address — i.e. a collateral movement to a liquidator (not the
/// burn-side pair ERC20 Transfer-to-ZERO). Used by `collect_collateral_events`
/// tests to assert the pair-Transfer filter distinguishes the two cases.
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
        decoded: ScaledTokenEventData::Transfer {
            from,
            to: target,
            value: amount,
        },
        event_type,
        token_address,
        user_address: from,
        caller_address: None,
        from_address: Some(from),
        target_address: Some(target),
        amount,
        balance_increase: None,
        index: None,
        log_index: log_idx,
    }
}

/// Construct an index-bearing `BalanceTransfer` fixture — the SCALED report of
/// a balance movement. The same movement's underlying-unit report is
/// [`transfer_to_event`] (the index-less ERC20 `Transfer`).
fn balance_transfer_event(
    log_idx: u64,
    event_type: ScaledTokenEventType,
    from: Address,
    to: Address,
    token_address: Address,
    value: U256,
    index: U256,
) -> ScaledTokenEvent<'static> {
    let log = Box::leak(Box::new(test_log(
        log_idx,
        &aave_event_decoder::AAVE_SUPPLY_TOPIC,
        &[],
    )));
    ScaledTokenEvent {
        log,
        decoded: ScaledTokenEventData::BalanceTransfer {
            from,
            to,
            value,
            index,
        },
        event_type,
        token_address,
        user_address: from,
        caller_address: None,
        from_address: Some(from),
        target_address: Some(to),
        amount: value,
        balance_increase: Some(U256::ZERO),
        index: Some(index),
        log_index: log_idx,
    }
}

// ── per-log ownership classification ──────────────────────────────────────
//
// `EventOwnership::classify` resolves each decoded log's role ONCE: semantic
// Mint/Burn, zero-address companion, or a movement report (the index-less ERC20
// carrier + the scaled `BalanceTransfer`). The operation builders consume those
// dispositions; the tests here pin the decisions themselves.

/// One disposition per decoded log: the semantic event, the zero-address
/// companion mirroring it, or the two reports of a movement.
#[test]
fn classify_assigns_one_disposition_per_decoded_log() {
    let user = Address::from([0xA0; 20]);
    let recipient = Address::from([0xB0; 20]);
    let a_token = Address::from([0xC0; 20]);
    let ray = U256::from(1_000_000_000_u64);

    let events = vec![
        // Mint: Aave emits the companion ERC20 Transfer BEFORE the Mint.
        transfer_to_event(
            10,
            ScaledTokenEventType::Erc20CollateralTransfer,
            Address::ZERO,
            user,
            a_token,
            U256::from(5),
        ),
        scaled_event(
            11,
            ScaledTokenEventType::CollateralMint,
            user,
            a_token,
            U256::from(5),
            Some(ray),
        ),
        // A standalone movement: ERC20 report + its scaled BalanceTransfer.
        transfer_to_event(
            12,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            recipient,
            a_token,
            U256::from(7),
        ),
        balance_transfer_event(
            13,
            ScaledTokenEventType::CollateralTransfer,
            user,
            recipient,
            a_token,
            U256::from(7),
            ray,
        ),
        // Burn: the companion ERC20 Transfer(user→0x0) PRECEDES the Burn.
        transfer_to_event(
            14,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            Address::ZERO,
            a_token,
            U256::from(5),
        ),
        scaled_event(
            15,
            ScaledTokenEventType::CollateralBurn,
            user,
            a_token,
            U256::from(5),
            None,
        ),
    ];

    let ownership = EventOwnership::classify(&events);

    assert_eq!(
        ownership.len(),
        events.len(),
        "every decoded log has exactly one disposition"
    );
    assert_eq!(
        ownership.disposition(10),
        Some(EventDisposition::Companion {
            owner: 11,
            side: CompanionSide::Mint
        }),
        "the from-0x0 ERC20 Transfer is a typed benign companion of the Mint"
    );
    assert_eq!(
        ownership.disposition(11),
        Some(EventDisposition::SemanticMint)
    );
    assert_eq!(
        ownership.disposition(12),
        Some(EventDisposition::StandaloneTransfer {
            scaled_partner: Some(13)
        }),
        "the ERC20 report carries the movement and names its scaled report"
    );
    assert_eq!(
        ownership.disposition(13),
        Some(EventDisposition::BalanceTransfer {
            underpinned_by: Some(12)
        }),
        "the scaled report is underpinned by the ERC20 report of the same movement"
    );
    assert_eq!(
        ownership.disposition(14),
        Some(EventDisposition::Companion {
            owner: 15,
            side: CompanionSide::Burn
        })
    );
    assert_eq!(
        ownership.disposition(15),
        Some(EventDisposition::SemanticBurn)
    );
}

/// Multiple burns in one transaction: every companion names its own burn, so a
/// liquidation's collateral debit can never be confused with a debt burn.
#[test]
fn classify_multiple_burns_each_own_their_companion() {
    let user = Address::from([0xA0; 20]);
    let a_token = Address::from([0xB0; 20]);
    let v_token = Address::from([0xC0; 20]);
    let ray = U256::from(1_000_000_000_u64);

    let events = vec![
        transfer_to_event(
            10,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            Address::ZERO,
            a_token,
            U256::from(500),
        ),
        scaled_event(
            11,
            ScaledTokenEventType::CollateralBurn,
            user,
            a_token,
            U256::from(500),
            None,
        ),
        transfer_to_event(
            12,
            ScaledTokenEventType::Erc20DebtTransfer,
            user,
            Address::ZERO,
            v_token,
            U256::from(2_000),
        ),
        scaled_event(
            13,
            ScaledTokenEventType::DebtBurn,
            user,
            v_token,
            U256::from(2_000),
            Some(ray),
        ),
    ];

    let ownership = EventOwnership::classify(&events);

    assert_eq!(
        ownership.disposition(10),
        Some(EventDisposition::Companion {
            owner: 11,
            side: CompanionSide::Burn
        })
    );
    assert_eq!(
        ownership.disposition(12),
        Some(EventDisposition::Companion {
            owner: 13,
            side: CompanionSide::Burn
        })
    );
}

/// Two burns of the SAME user and token each get their own companion: companion
/// assignment is injective, so the second companion can never name a burn that
/// is already owned.
#[test]
fn classify_same_token_burns_claim_distinct_companions() {
    let user = Address::from([0xA0; 20]);
    let a_token = Address::from([0xB0; 20]);

    let events = vec![
        transfer_to_event(
            10,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            Address::ZERO,
            a_token,
            U256::from(500),
        ),
        scaled_event(
            11,
            ScaledTokenEventType::CollateralBurn,
            user,
            a_token,
            U256::from(500),
            None,
        ),
        transfer_to_event(
            12,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            Address::ZERO,
            a_token,
            U256::from(500),
        ),
        scaled_event(
            13,
            ScaledTokenEventType::CollateralBurn,
            user,
            a_token,
            U256::from(500),
            None,
        ),
    ];

    let ownership = EventOwnership::classify(&events);

    assert_eq!(
        ownership.disposition(10),
        Some(EventDisposition::Companion {
            owner: 11,
            side: CompanionSide::Burn
        })
    );
    assert_eq!(
        ownership.disposition(12),
        Some(EventDisposition::Companion {
            owner: 13,
            side: CompanionSide::Burn
        }),
        "the second companion must claim the second burn, never the already-owned first burn"
    );
}

/// stkAAVE (the GHO discount token) emits a single ERC20 `Transfer` and no
/// scaled report: the movement is an unpaired standalone carrier.
#[test]
fn classify_discount_transfer_is_an_unpaired_standalone_carrier() {
    let sender = Address::from([0xA0; 20]);
    let recipient = Address::from([0xB0; 20]);
    let stk_aave = Address::from([0xC0; 20]);

    let events = vec![transfer_to_event(
        10,
        ScaledTokenEventType::DiscountTransfer,
        sender,
        recipient,
        stk_aave,
        U256::from(42),
    )];

    let ownership = EventOwnership::classify(&events);

    assert_eq!(
        ownership.disposition(10),
        Some(EventDisposition::StandaloneTransfer {
            scaled_partner: None
        })
    );
}

/// Same-value distinct movements are independently owned: ownership keys on the
/// movement's addresses, never on its amount.
#[test]
fn classify_same_value_distinct_movements_stay_independent() {
    let user = Address::from([0xA0; 20]);
    let recipient = Address::from([0xB0; 20]);
    let a_token = Address::from([0xC0; 20]);
    let amount = U256::from(1_000);

    let events = vec![
        transfer_to_event(
            10,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            Address::ZERO,
            a_token,
            amount,
        ),
        scaled_event(
            11,
            ScaledTokenEventType::CollateralBurn,
            user,
            a_token,
            amount,
            None,
        ),
        transfer_to_event(
            12,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            recipient,
            a_token,
            amount,
        ),
    ];

    let ownership = EventOwnership::classify(&events);

    assert_eq!(
        ownership.disposition(10),
        Some(EventDisposition::Companion {
            owner: 11,
            side: CompanionSide::Burn
        }),
        "the zero-side report of the burn is the burn's companion"
    );
    assert_eq!(
        ownership.disposition(12),
        Some(EventDisposition::StandaloneTransfer {
            scaled_partner: None
        }),
        "an equal-valued transfer to a real recipient is its own movement"
    );
}

/// Duplicate ownership rejection: two identical ERC20 reports of one movement
/// share exactly one scaled report. The first log-order carrier owns it; the
/// second is a typed duplicate of that carrier and can never emit.
#[test]
fn classify_types_a_duplicate_carrier_and_keeps_one_owner() {
    let sender = Address::from([0xA0; 20]);
    let recipient = Address::from([0xB0; 20]);
    let a_token = Address::from([0xC0; 20]);
    let ray = U256::from(1_000_000_000_u64);

    let events = vec![
        transfer_to_event(
            10,
            ScaledTokenEventType::Erc20CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
        ),
        transfer_to_event(
            11,
            ScaledTokenEventType::Erc20CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
        ),
        balance_transfer_event(
            12,
            ScaledTokenEventType::CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
            ray,
        ),
    ];

    let ownership = EventOwnership::classify(&events);

    assert_eq!(
        ownership.disposition(10),
        Some(EventDisposition::StandaloneTransfer {
            scaled_partner: Some(12)
        })
    );
    assert_eq!(
        ownership.disposition(11),
        Some(EventDisposition::DuplicateCarrier { owner: 10 }),
        "the second report of the same movement names the first carrier and \
         never becomes a second carrier"
    );
    assert_eq!(
        ownership.disposition(12),
        Some(EventDisposition::BalanceTransfer {
            underpinned_by: Some(10)
        })
    );
}

/// Log order is not the ownership key: a scaled report emitted BEFORE its ERC20
/// report still pairs into the one movement, with the ERC20 as the carrier.
#[test]
fn classify_pairs_scaled_report_emitted_before_its_erc20_report() {
    let sender = Address::from([0xA0; 20]);
    let recipient = Address::from([0xB0; 20]);
    let a_token = Address::from([0xC0; 20]);
    let ray = U256::from(1_000_000_000_u64);

    let events = vec![
        balance_transfer_event(
            10,
            ScaledTokenEventType::CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
            ray,
        ),
        transfer_to_event(
            11,
            ScaledTokenEventType::Erc20CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
        ),
    ];

    let ownership = EventOwnership::classify(&events);

    assert_eq!(
        ownership.disposition(10),
        Some(EventDisposition::BalanceTransfer {
            underpinned_by: Some(11)
        })
    );
    assert_eq!(
        ownership.disposition(11),
        Some(EventDisposition::StandaloneTransfer {
            scaled_partner: Some(10)
        })
    );
}

// ── standalone-transfer operation ownership (step 4e) ─────────────────────

/// The standalone operation carries the movement's ERC20 report and rides the
/// scaled report as its scaling partner.
#[test]
fn standalone_transfer_builds_one_operation_from_the_paired_reports() {
    let sender = Address::from([0xA0; 20]);
    let recipient = Address::from([0xB0; 20]);
    let a_token = Address::from([0xC0; 20]);
    let ray = U256::from(1_000_000_000_u64);

    let events = vec![
        transfer_to_event(
            12,
            ScaledTokenEventType::Erc20CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
        ),
        balance_transfer_event(
            13,
            ScaledTokenEventType::CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
            ray,
        ),
    ];
    let ownership = EventOwnership::classify(&events);
    let assigned = HashSet::new();
    let mut next_op_id = 0;

    let ops = TransactionOperationsParser::create_transfer_operations(
        9,
        &events,
        &ownership,
        &assigned,
        &mut next_op_id,
    );

    assert_eq!(ops.len(), 1, "one movement → exactly one operation");
    assert_eq!(ops[0].operation_type, OperationType::BalanceTransfer);
    assert_eq!(
        ops[0].scaled_events[0].log_index, 12,
        "the ERC20 report is the carrier"
    );
    assert_eq!(ops[0].balance_transfer_events.len(), 1);
    assert_eq!(log_idx_value(ops[0].balance_transfer_events[0]), 13);
}

/// A movement whose scaled report is already claimed by an earlier operation
/// (the liquidation protocol-fee movement) belongs to that operation. Building
/// a standalone operation here would apply the fee a second time.
#[test]
fn standalone_transfer_skips_a_movement_owned_by_another_operation() {
    let user = Address::from([0xA0; 20]);
    let treasury = Address::from([0xB0; 20]);
    let a_token = Address::from([0xC0; 20]);
    let ray = U256::from(1_000_000_000_u64);

    let events = vec![
        transfer_to_event(
            11,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            treasury,
            a_token,
            U256::from(5_815_314_991_815_639_u64),
        ),
        balance_transfer_event(
            12,
            ScaledTokenEventType::CollateralTransfer,
            user,
            treasury,
            a_token,
            U256::from(5_812_238_924_384_490_u64),
            ray,
        ),
    ];
    let ownership = EventOwnership::classify(&events);
    let mut assigned = HashSet::new();
    assigned.insert(12); // collected by the liquidation operation
    let mut next_op_id = 0;

    let ops = TransactionOperationsParser::create_transfer_operations(
        9,
        &events,
        &ownership,
        &assigned,
        &mut next_op_id,
    );

    assert!(
        ops.is_empty(),
        "the liquidation owns the fee movement; a standalone op would double-apply it"
    );
}

/// Duplicate carriers of one movement emit exactly one operation: the repeat is
/// a typed benign ignore, never a second standalone carrier.
#[test]
fn standalone_transfer_emits_one_operation_for_duplicate_carriers() {
    let sender = Address::from([0xA0; 20]);
    let recipient = Address::from([0xB0; 20]);
    let a_token = Address::from([0xC0; 20]);
    let ray = U256::from(1_000_000_000_u64);

    let events = vec![
        transfer_to_event(
            10,
            ScaledTokenEventType::Erc20CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
        ),
        transfer_to_event(
            11,
            ScaledTokenEventType::Erc20CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
        ),
        balance_transfer_event(
            12,
            ScaledTokenEventType::CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
            ray,
        ),
    ];
    let ownership = EventOwnership::classify(&events);
    let assigned = HashSet::new();
    let mut next_op_id = 0;

    let ops = TransactionOperationsParser::create_transfer_operations(
        9,
        &events,
        &ownership,
        &assigned,
        &mut next_op_id,
    );

    assert_eq!(ops.len(), 1, "duplicate carriers describe one movement");
    assert_eq!(ops[0].scaled_events[0].log_index, 10);
    assert_eq!(ops[0].balance_transfer_events.len(), 1);
}

/// A duplicate carrier never emits, even when the movement's scaled report is
/// already claimed by another operation (the liquidation protocol-fee case):
/// the duplicate is ignored regardless of claim state, so the apply path never
/// sees the same movement twice.
#[test]
fn standalone_transfer_never_emits_a_duplicate_of_a_claimed_movement() {
    let sender = Address::from([0xA0; 20]);
    let recipient = Address::from([0xB0; 20]);
    let a_token = Address::from([0xC0; 20]);
    let ray = U256::from(1_000_000_000_u64);

    let events = vec![
        transfer_to_event(
            10,
            ScaledTokenEventType::Erc20CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
        ),
        transfer_to_event(
            11,
            ScaledTokenEventType::Erc20CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
        ),
        balance_transfer_event(
            12,
            ScaledTokenEventType::CollateralTransfer,
            sender,
            recipient,
            a_token,
            U256::from(7),
            ray,
        ),
    ];
    let ownership = EventOwnership::classify(&events);
    let mut assigned = HashSet::new();
    assigned.insert(12); // collected by the liquidation operation
    let mut next_op_id = 0;

    let ops = TransactionOperationsParser::create_transfer_operations(
        9,
        &events,
        &ownership,
        &assigned,
        &mut next_op_id,
    );

    assert!(
        ops.is_empty(),
        "a claimed movement's duplicate must not emit; the apply path already saw the movement"
    );
}

/// A semantic Mint/Burn's zero-address companion never becomes a standalone
/// operation, even when no Pool-event builder claimed either event.
#[test]
fn standalone_transfer_never_applies_a_semantic_companion() {
    let user = Address::from([0xA0; 20]);
    let a_token = Address::from([0xC0; 20]);

    let events = vec![
        transfer_to_event(
            10,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            Address::ZERO,
            a_token,
            U256::from(500),
        ),
        scaled_event(
            11,
            ScaledTokenEventType::CollateralBurn,
            user,
            a_token,
            U256::from(500),
            None,
        ),
    ];
    let ownership = EventOwnership::classify(&events);
    let assigned = HashSet::new();
    let mut next_op_id = 0;

    let ops = TransactionOperationsParser::create_transfer_operations(
        9,
        &events,
        &ownership,
        &assigned,
        &mut next_op_id,
    );

    assert!(
        ops.is_empty(),
        "the Burn carries the debit; its companion Transfer is benign-ignored"
    );
}

/// stkAAVE transfers keep their dedicated operation type and have no scaling
/// partner.
#[test]
fn standalone_discount_transfer_builds_stk_aave_operation() {
    let sender = Address::from([0xA0; 20]);
    let recipient = Address::from([0xB0; 20]);
    let stk_aave = Address::from([0xC0; 20]);

    let events = vec![transfer_to_event(
        10,
        ScaledTokenEventType::DiscountTransfer,
        sender,
        recipient,
        stk_aave,
        U256::from(42),
    )];
    let ownership = EventOwnership::classify(&events);
    let assigned = HashSet::new();
    let mut next_op_id = 0;

    let ops = TransactionOperationsParser::create_transfer_operations(
        9,
        &events,
        &ownership,
        &assigned,
        &mut next_op_id,
    );

    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].operation_type, OperationType::StkAaveTransfer);
    assert!(ops[0].balance_transfer_events.is_empty());
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

/// `LiquidationWindow` is half-open and anchored on the previous
/// `LiquidationCall`: consecutive liquidations own disjoint ranges, and the
/// first one owns everything before it.
#[test]
fn liquidation_windows_partition_a_transaction_between_its_anchors() {
    let anchors = [
        liquidation_call_log(40, Address::ZERO, Address::ZERO, Address::ZERO, U256::ZERO),
        liquidation_call_log(90, Address::ZERO, Address::ZERO, Address::ZERO, U256::ZERO),
    ];
    let all_events: Vec<&Log> = anchors.iter().collect();

    let first = LiquidationWindow::ending_at(&all_events, 40);
    let second = LiquidationWindow::ending_at(&all_events, 90);

    assert!(
        first.owns(0),
        "the first liquidation owns the transaction's opening logs"
    );
    assert!(first.owns(39));
    assert!(!first.owns(40), "an anchor belongs to no window");
    assert!(!first.owns(41));
    assert!(second.owns(41), "the second starts after the first anchor");
    assert!(second.owns(89));
    assert!(!second.owns(90));
    assert!(!second.owns(39), "windows never overlap");
}

/// `collect_collateral_events`: the `collateral_burn` vs `collateral_transfers`
/// split. A liquidator's credit arrives as the movement's scaled
/// `BalanceTransfer` report (or a `Mint` when `receiveAToken=true`); the
/// index-less ERC20 report of that movement is never an operation source inside
/// a liquidation.
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
    // The underlying-unit report of collateral moved to the liquidator. The
    // Burn carries the user's debit; an index-less report never carries a
    // liquidation movement (the scaled report does).
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
    let ownership = EventOwnership::classify(&events);
    let assigned = HashSet::new();
    let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
        user,
        Some(a_token),
        &events,
        &ownership,
        LiquidationWindow::ending_at(&[], 100),
        &assigned,
    );
    assert!(
        cb.is_some(),
        "collateral_burn found for this liquidation's aToken"
    );
    assert_eq!(cb.unwrap().log_index, 10);
    assert_eq!(
        ct.len(),
        0,
        "the index-less ERC20 report of a collateral movement is not a \
         liquidation collateral event: the Burn carries the debit and the \
         scaled BalanceTransfer report carries the moved balance"
    );
    assert!(
        assigned.is_empty(),
        "the collector consumes the disposition; it never writes claims back"
    );
}

/// The Aave V3 protocol fee is one balance movement reported twice: the
/// standard ERC20 `Transfer(user→treasury, fee_underlying)` and Aave's native
/// `BalanceTransfer(user→treasury, fee_scaled, liquidity_index)`.
/// `collect_collateral_events` collects the scaled report only — collecting both
/// reports would debit the user and credit the treasury twice.
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
    // The fee's underlying-unit report: applying this as well as the scaled
    // report below would over-debit the user and over-credit the treasury by
    // exactly the fee.
    let fee_underlying_amount = U256::from(5_815_314_991_815_639u64);
    let erc20_fee_transfer = transfer_to_event(
        11,
        ScaledTokenEventType::Erc20CollateralTransfer,
        user,
        treasury,
        a_token,
        fee_underlying_amount,
    );
    // The same movement's scaled report — value × index / RAY ≈ the ERC20
    // report's amount (within ray-floor rounding).
    let fee_scaled_amount = U256::from(5_812_238_924_384_490u64);
    let bt_fee_transfer = balance_transfer_event(
        12,
        ScaledTokenEventType::CollateralTransfer,
        user,
        treasury,
        a_token,
        fee_scaled_amount,
        U256::from(1_000_000_000u64),
    );
    let events = vec![burn, erc20_fee_transfer, bt_fee_transfer];
    let ownership = EventOwnership::classify(&events);
    let assigned = HashSet::new();
    let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
        user,
        Some(a_token),
        &events,
        &ownership,
        LiquidationWindow::ending_at(&[], 100),
        &assigned,
    );
    // Burn must be found.
    assert_eq!(cb.unwrap().log_index, 10);
    // ONLY the scaled report is collected — the ERC20 report is a benign mirror.
    assert_eq!(
        ct.len(),
        1,
        "only the scaled report of the fee movement is collected; \
         the ERC20 report is the same movement's underlying-unit mirror"
    );
    assert_eq!(
        ct[0].log_index, 12,
        "the collected transfer is the BalanceTransfer report"
    );
    assert_eq!(
        ownership.disposition(12),
        Some(EventDisposition::BalanceTransfer {
            underpinned_by: Some(11)
        }),
        "the classification ties the fee's two reports into one movement"
    );
    assert!(
        assigned.is_empty(),
        "the collector no longer claims the mirror report: step 4e skips the \
         movement because the liquidation owns its scaled report"
    );
}

/// A burn's zero-side companion report (the ERC20 `Transfer user→0x0`) is
/// benign-ignored: the `Burn` is the sole operational debit. The collector must
/// neither collect the companion nor rewrite the claim set to remember it.
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
    // The paired ERC20 Transfer user→0x0 (the burn-side companion).
    let pair_transfer = transfer_to_event(
        11,
        ScaledTokenEventType::Erc20CollateralTransfer,
        user,
        Address::ZERO,
        a_token,
        U256::from(500),
    );
    let events = vec![burn, pair_transfer];
    let ownership = EventOwnership::classify(&events);
    let assigned = HashSet::new();
    let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
        user,
        Some(a_token),
        &events,
        &ownership,
        LiquidationWindow::ending_at(&[], 100),
        &assigned,
    );
    assert!(cb.is_some(), "burn collected");
    assert_eq!(cb.unwrap().log_index, 10);
    assert_eq!(
        ct.len(),
        0,
        "the burn's companion report is benign-ignored by disposition"
    );
    assert_eq!(
        ownership.disposition(11),
        Some(EventDisposition::Companion {
            owner: 10,
            side: CompanionSide::Burn
        })
    );
    assert!(
        assigned.is_empty(),
        "no write-back: the companion is excluded by its disposition, not by a claim"
    );
}

/// `collect_collateral_events`: when collateral is moved (no burn), the scaled
/// report is collected and the burn is `None`.
#[test]
fn collect_collateral_events_no_burn_only_transfers() {
    let user = Address::from([0xA0; 20]);
    let a_token = Address::from([0xB0; 20]);
    let liquidator = Address::from([0xD0; 20]);
    let transfer = balance_transfer_event(
        11,
        ScaledTokenEventType::CollateralTransfer,
        user,
        liquidator,
        a_token,
        U256::from(500),
        U256::from(1_000_000_000u64),
    );
    let events = vec![transfer];
    let ownership = EventOwnership::classify(&events);
    let assigned = HashSet::new();
    let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
        user,
        Some(a_token),
        &events,
        &ownership,
        LiquidationWindow::ending_at(&[], 100),
        &assigned,
    );
    assert!(cb.is_none(), "no burn — collateral moved via transfer");
    assert_eq!(ct.len(), 1);
    assert_eq!(ct[0].log_index, 11);
}

/// `LiquidationGroup::detect_pattern` —
/// the parity-critical pattern classifier.
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

// ── MintToTreasury must NOT pre-scale ─────────────────────
//
// `create_mint_to_treasury_operations` stores the MintedToTreasury
// `amount_minted` as the RAW UNDERLYING amount (Python's
// `operation.minted_to_treasury_amount` semantics, regardless of
// `pool_revision`); the one scaled conversion (`ray_div(.., HALF_UP)`) lives in
// `dispatch_mint_to_treasury`. Pre-scaling in the MintToTreasury builder
// makes the
// dispatch apply the SAME `ray_div(.., HALF_UP)` a second time, shrinking the
// result by `idx/RAY` (the treasury 0x464C WETH residual).
//
// The WETH mainnet tuple below is the arithmetic the divergence is derived
// from:
//   amountMinted (raw underlying) = 64_746_517_106_584_784
//   liquidity_index at emit-time   = 1_000_124_218_031_532_223_928_748_283
//   expected SINGLE ray_div result = 64_738_475_420_603_639 (Python gold
//     = on-chain truth delta: treasury WETH scaledBalance went 0 →
//     64_738_475_420_603,639 across the block boundary)
//   double ray_div result          = 64_730_434_733_420_828 (= the value a
//     pre-scaling builder stores, 8_040_687_182_811 LESS than Python gold).
//
// The test creates an in-memory DB substrate + drives BOTH the builder
// (`create_mint_to_treasury_operations`) AND the apply dispatch
// (`dispatch_mint_to_treasury`), asserting the FINAL `balance_delta` ==
// Python gold's SINGLE conversion; a pre-scaling builder produces the
// DOUBLE-conversion value instead.
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
/// the Pool contract (the `MintToTreasury` indicator that the builder
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

/// Asserts the pipeline (builder + dispatch) produces the SINGLE `ray_div` result
/// (= Python's `PoolMath::underlying_to_scaled_collateral` value), NOT a
/// DOUBLE-conversion. Pre-scaling the `amount_minted` in the builder makes
/// `dispatch_mint_to_treasury` re-ray_div it, shrinking by `idx/RAY`
/// (= the -8.04e12 residual on the treasury WETH position).
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

    // Hand-math prediction:
    //   Python gold / on-chain truth = ray_div_halfup(raw, idx) = 64_738_475_420_603_639
    //   DOUBLE-conv                  = ray_div(ray_div(raw, idx), idx) = 64_730_434_733_420_828
    //   Diff = Python gold - DOUBLE-conv = 8_040_687_182_811.
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
    let mut substrate = ChunkSubstrate::lazy(1);
    let mut parser = TransactionOperationsParser::new(
        1, // market_id
        1, // chain_id
        pool_address,
        Some(treasury),
        None, // gho_token_address
        None, // gho_vtoken_address
        &conn,
        &mut substrate,
    )
    .unwrap();

    // === BUILDER STEP (the parse-time site) ===
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

    // The builder must store the RAW UNDERLYING amount (= amount_minted_raw),
    // NOT pre-scaled via ray_div — that scaled conversion happens EXACTLY
    // ONCE in `dispatch_mint_to_treasury`. A pre-scaling builder stores
    // `Some(python_gold_scaled)` (= `ray_div(raw, idx, HALF_UP)`), which the
    // dispatch then re-ray_div's → DOUBLE conversion.
    let op = ops.pop().unwrap();
    assert_eq!(
        op.minted_to_treasury_amount,
        Some(amount_minted_raw),
        "DP3 must store the raw underlying amountMinted (NOT pre-scaled); \
         pre-scaling to {python_gold_scaled} (= ray_div(raw, idx, HALF_UP)) \
         DOUBLE-converts at dispatch",
    );
    assert_eq!(op.operation_type, OperationType::MintToTreasury);

    // === DISPATCH STEP (the apply-time conversion) ===
    // `dispatch_mint_to_treasury` does the single ray_div (`= Python's
    // `PoolMath::underlying_to_scaled_collateral`). The final balance_delta ==
    // python_gold_scaled.
    //
    // An already-SCALED `op.minted_to_treasury_amount` makes dispatch's ray_div
    // produce the DOUBLE-conv result = buggy_double_conv_scaled.
    let mut events: Vec<crate::run::AaveChunkEvent> = Vec::new();
    crate::transaction_processor::dispatch_mint_to_treasury(
        &op,
        1, // market_id
        &conn,
        &mut events,
        &mut ChunkSubstrate::lazy(1),
    )
    .unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        crate::run::AaveChunkEvent::ScaledTokenMint { balance_delta, .. } => {
            assert_eq!(
                *balance_delta,
                alloy::primitives::I256::try_from(python_gold_scaled).unwrap(),
                "final balance_delta must equal Python gold's SINGLE ray_div result \
                 (= on-chain truth); the DOUBLE-conversion produces {} \
                 ({} LESS than Python gold)",
                buggy_double_conv_scaled,
                python_gold_scaled - buggy_double_conv_scaled,
            );
        }
        other => panic!("expected AaveChunkEvent::ScaledTokenMint, got {other:?}"),
    }
}
