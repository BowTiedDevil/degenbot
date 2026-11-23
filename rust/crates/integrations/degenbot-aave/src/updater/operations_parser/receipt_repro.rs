//! The captured block-26_458_346 liquidation receipt: one transaction whose
//! collateral legs are split across two liquidations of the same user and the
//! same collateral asset.
//!
//! The recorded receipt (`tests/fixtures/aave_receipts/`) is a live
//! `eth_getTransactionReceipt` answer plus the market rows the parse reads.
//! Six `LiquidationCall` events; user `0x21ebC7C1…` is liquidated three times,
//! twice against the same `aEthWETH` collateral (log indices 96 and 196).
//!
//! Each liquidation's collateral leg is its OWN log-range: Aave emits the
//! seized-collateral events between the start of the `liquidationCall` and the
//! `LiquidationCall` event it finishes with. These assertions pin that
//! ownership, because a collector that keys only on `(user, collateral aToken)`
//! hands every event of the pair to whichever liquidation is built first: the
//! earlier one over-debits, and the later one aborts the chunk with
//! "Expected at least 1 collateral event (burn or transfer) for liquidation".

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::operations::OperationType;
use crate::updater::run::substrate::ChunkSubstrate;
use std::collections::BTreeMap;

/// The committed capture (machine-emitted from a live archive node + the
/// operator DB's market rows; see the file's `provenance` member).
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/aave_receipts/liquidation_collateral_ownership_26458346.json"
);

/// The `LiquidationCall` log indices this tx carries, in log order.
const LIQUIDATION_LOG_INDICES: [u64; 6] = [78, 96, 160, 172, 184, 196];

/// The captured tx's market rows, as the fixture stores them.
struct CapturedMarket {
    chain_id: i64,
    pool: Address,
    pool_revision: u32,
    configurator: Address,
    configurator_revision: u32,
    price_oracle: Address,
    gho_token: Address,
    gho_vtoken: Address,
    reserves: Vec<CapturedReserve>,
}

struct CapturedReserve {
    underlying: Address,
    a_token: Address,
    a_token_revision: u32,
    v_token: Address,
    v_token_revision: u32,
}

fn captured() -> (serde_json::Value, CapturedMarket, Vec<Log>) {
    let bytes = std::fs::read(FIXTURE).expect("the committed receipt capture must exist");
    let document: serde_json::Value = serde_json::from_slice(&bytes).expect("fixture is JSON");
    let market = &document["market"];
    let parse_address = |value: &serde_json::Value| -> Address {
        value
            .as_str()
            .unwrap_or_else(|| panic!("expected an address string, got {value}"))
            .parse()
            .unwrap_or_else(|e| panic!("bad address {value}: {e}"))
    };
    let parse_u32 = |value: &serde_json::Value| -> u32 {
        u32::try_from(value.as_i64().unwrap_or_else(|| panic!("{value}"))).unwrap()
    };

    let reserves: Vec<CapturedReserve> = document["reserves"]
        .as_array()
        .expect("fixture carries the captured reserve rows")
        .iter()
        .map(|row| CapturedReserve {
            underlying: parse_address(&row["underlying"]),
            a_token: parse_address(&row["a_token"]),
            a_token_revision: parse_u32(&row["a_token_revision"]),
            v_token: parse_address(&row["v_token"]),
            v_token_revision: parse_u32(&row["v_token_revision"]),
        })
        .collect();

    let captured_market = CapturedMarket {
        chain_id: market["chain_id"].as_i64().expect("chain id"),
        pool: parse_address(&market["pool"]),
        pool_revision: parse_u32(&market["pool_revision"]),
        configurator: parse_address(&market["pool_configurator"]),
        configurator_revision: parse_u32(&market["configurator_revision"]),
        price_oracle: parse_address(&market["price_oracle"]),
        gho_token: parse_address(&market["gho_token"]),
        gho_vtoken: parse_address(&market["gho_vtoken"]),
        reserves,
    };

    let mut logs: Vec<Log> = document["receipt"]["logs"]
        .as_array()
        .expect("the receipt carries its logs")
        .iter()
        .map(|value| {
            serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("receipt log is not an alloy Log: {e}"))
        })
        .collect();
    logs.sort_by_key(|log| log.log_index.unwrap_or_default());
    (document, captured_market, logs)
}

/// Seed the rows the parse reads: the market, the contract revisions the
/// parser's tolerance gate needs (`POOL`), the GHO pair, and every reserve's
/// underlying to aToken/vToken triple the sibling lookups resolve.
fn seeded_market(market: &CapturedMarket) -> degenbot_db::DegenbotDb {
    use degenbot_db::DegenbotDb;
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().expect("in-memory DB");
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, ?1, 'Aave Ethereum Market', 1, NULL)",
            rusqlite::params![market.chain_id],
        )
        .expect("market row");
        for (name, address, revision) in [
            ("POOL", market.pool, Some(i64::from(market.pool_revision))),
            (
                "POOL_CONFIGURATOR",
                market.configurator,
                Some(i64::from(market.configurator_revision)),
            ),
            ("PRICE_ORACLE", market.price_oracle, None),
        ] {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn,
                1,
                name,
                &address.to_checksum(None),
                revision,
            )
            .expect("contract row");
        }
        let _gho_id = DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            market.chain_id,
            &market.gho_token.to_checksum(None),
            None,
            None,
            None,
        )
        .expect("gho token row");
        DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            market.chain_id,
            &market.gho_vtoken.to_checksum(None),
            None,
            None,
            None,
        )
        .expect("gho vToken row");
        DegenbotDb::get_or_create_gho_token_on_conn(
            &conn,
            market.chain_id,
            &market.gho_token.to_checksum(None),
        )
        .expect("aave_gho_tokens row");
        for reserve in &market.reserves {
            let underlying_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                market.chain_id,
                &reserve.underlying.to_checksum(None),
                None,
                None,
                None,
            )
            .expect("underlying row");
            let a_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                market.chain_id,
                &reserve.a_token.to_checksum(None),
                None,
                None,
                None,
            )
            .expect("aToken row");
            let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                market.chain_id,
                &reserve.v_token.to_checksum(None),
                None,
                None,
                None,
            )
            .expect("vToken row");
            DegenbotDb::apply_reserve_initialized_on_conn(
                &conn,
                1,
                underlying_id,
                a_token_id,
                i64::from(reserve.a_token_revision),
                v_token_id,
                i64::from(reserve.v_token_revision),
                None,
                None,
            )
            .expect("reserve row");
        }
    }
    db
}

/// The `LiquidationCall` anchor log index of a liquidation operation.
fn anchor(op: &crate::operations::Operation<'_>) -> u64 {
    op.pool_event
        .map(|log| log.log_index.unwrap_or_default())
        .expect("a liquidation operation carries its LiquidationCall")
}

/// The recorded tx parses, and every liquidation owns its own collateral leg.
#[test]
fn every_collateral_leg_belongs_to_the_liquidation_that_emitted_it() {
    let (_document, market, _logs) = captured();
    let db = seeded_market(&market);
    let conn = db.lock();
    let mut substrate = ChunkSubstrate::lazy(market.chain_id);
    let mut parser = TransactionOperationsParser::new(
        1,
        market.chain_id,
        market.pool,
        Some(market.gho_token),
        Some(market.gho_vtoken),
        &conn,
        &mut substrate,
    )
    .expect("the parser resolves the market's pool revision");
    let evidence = captured_evidence();

    let parsed = parser
        .parse_evidence(&evidence, evidence.tx().hash().0)
        .unwrap_or_else(|e| {
            panic!("the captured block must parse (the chunk aborts otherwise): {e}")
        });

    let mut owned: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for op in &parsed.operations {
        if matches!(
            op.operation_type,
            OperationType::Liquidation | OperationType::GhoLiquidation
        ) {
            owned.insert(
                anchor(op),
                op.scaled_events.iter().map(|ev| ev.log_index).collect(),
            );
        }
    }
    assert_eq!(
        owned.keys().copied().collect::<Vec<_>>(),
        LIQUIDATION_LOG_INDICES,
        "every LiquidationCall in the tx becomes one operation"
    );

    // Liquidation 78 seizes WBTC: its CollateralBurn and its protocol-fee
    // BalanceTransfer are the logs emitted before its own LiquidationCall.
    let wbtc = &owned[&78];
    assert!(wbtc.contains(&73), "the WBTC leg owns its burn: {wbtc:?}");
    assert!(
        wbtc.contains(&76),
        "the WBTC leg owns its protocol-fee transfer: {wbtc:?}"
    );

    // Liquidation 96 seizes dust against a larger interest accrual, so its
    // collateral debit is the interest-exceeds Mint at log 89, not a Burn.
    let first_weth = &owned[&96];
    assert!(
        first_weth.contains(&89),
        "the interest-exceeds Mint is the collateral debit of the WETH leg: {first_weth:?}"
    );
    assert!(
        first_weth.contains(&94),
        "the WETH leg owns its own protocol-fee transfer: {first_weth:?}"
    );

    // Liquidation 196 seizes WETH: the Burn at 191 and the fee transfer at
    // 194 are emitted between liquidation 96 and 196, so they belong to 196 -
    // not to the earlier WETH liquidation.
    let second_weth = &owned[&196];
    assert!(
        second_weth.contains(&191),
        "the second WETH leg owns the burn emitted in its own range: {second_weth:?}"
    );
    assert!(
        second_weth.contains(&194),
        "the second WETH leg owns the fee transfer emitted in its own range: {second_weth:?}"
    );
    assert!(
        !second_weth.contains(&94),
        "the second WETH leg must not inherit the earlier leg's fee transfer: {second_weth:?}"
    );
    for absent in [191_u64, 194] {
        assert!(
            !first_weth.contains(&absent),
            "the first WETH leg must not claim the later leg's log {absent}: {first_weth:?}"
        );
    }
}

// ── total receipt ownership over the captured receipt (ADR-105 D5) ────────

/// The paired call trace of the captured transaction.
const TRACE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/aave_receipts/liquidation_trace_26458346.json"
);

/// The captured transaction's bound evidence: its complete receipt joined to its
/// call trace, exactly as `bind_chunk_evidence` hands it to the parse.
fn captured_evidence() -> crate::updater::evidence::TransactionEvidence {
    use crate::updater::evidence::bind_transaction_evidence;
    use degenbot_core::canonical_head::ChainAnchor;
    use degenbot_rpc::tx_evidence::{ReceiptTransaction, TracedTransaction};

    let trace_document: serde_json::Value = serde_json::from_slice(
        &std::fs::read(TRACE_FIXTURE).expect("the committed trace capture must exist"),
    )
    .expect("fixture is JSON");
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(FIXTURE).expect("fixture is JSON"))
            .expect("fixture is JSON");
    let header = &trace_document["header"];
    let anchor = ChainAnchor::new(
        trace_document["chain_id"].as_u64().expect("chain id"),
        header["number"].as_u64().expect("block number"),
        header["hash"].as_str().unwrap().parse().unwrap(),
        header["parent_hash"].as_str().unwrap().parse().unwrap(),
    )
    .expect("the captured header is a complete identity");
    let receipt = ReceiptTransaction::from_receipt_json(anchor, &document["receipt"])
        .expect("the captured receipt carries its complete identity");
    let traced = TracedTransaction::from_call_tracer_json(*receipt.tx(), &trace_document["trace"])
        .expect("the captured trace is a call tree");
    let owned: Vec<Log> = receipt.logs().to_vec();
    let filtered: Vec<&Log> = owned.iter().collect();
    bind_transaction_evidence(&filtered, receipt, Some(traced))
        .expect("the committed captures are one execution")
}

/// Drive the capture through the production entry point: its complete receipt,
/// joined to its call trace, parsed against the fixture's market rows.
fn with_parsed_capture<R>(
    f: impl FnOnce(
        TransactionOperations<'_>,
        &crate::updater::evidence::TransactionEvidence,
        &[Log],
    ) -> R,
) -> R {
    let (_document, market, logs) = captured();
    let db = seeded_market(&market);
    let conn = db.lock();
    let mut substrate = ChunkSubstrate::lazy(market.chain_id);
    let mut parser = TransactionOperationsParser::new(
        1,
        market.chain_id,
        market.pool,
        Some(market.gho_token),
        Some(market.gho_vtoken),
        &conn,
        &mut substrate,
    )
    .expect("the parser resolves the market's pool revision");
    let evidence = captured_evidence();
    let parsed = parser
        .parse_evidence(&evidence, evidence.tx().hash().0)
        .unwrap_or_else(|e| {
            panic!("the captured block must parse (the chunk aborts otherwise): {e}")
        });
    f(parsed, &evidence, &logs)
}

/// Every log of the captured receipt has exactly one owner, and the census is the
/// recorded one: 218 logs = 27 operation-owned + 35 recognized roles + 156 out of
/// scope.
#[test]
fn the_capture_is_owned_exactly_once_per_log() {
    with_parsed_capture(|parsed, _evidence, logs| {
        let census = parsed.ownership.census();
        assert_eq!(
            parsed.ownership.len(),
            logs.len(),
            "every log of the complete receipt is in the ledger"
        );
        assert_eq!(census.total(), logs.len(), "every log is owned: {census:?}");
        assert_eq!(
            census,
            OwnershipCensus {
                operation_owned: 27,
                role_owned: 35,
                out_of_scope: 156,
            },
            "the recorded census"
        );
        let claimed: std::collections::BTreeSet<u64> = parsed
            .operations
            .iter()
            .flat_map(Operation::claimed_log_indices)
            .collect();
        assert_eq!(
            claimed.len(),
            census.operation_owned,
            "the ledger's operation-owned count is the operations' own claim count"
        );
        for log in logs {
            let log_index = log.log_index.unwrap();
            assert!(
                parsed.ownership.owner(log_index).is_some(),
                "log {log_index} has no owner"
            );
        }
    });
}

/// The receipt's own vocabulary gets a role, not an operation: 13 reserve accrual
/// reports, 2 accrual-only mints, 14 zero-address companions, and 6 index-less
/// mirrors of movements the operations account for.
#[test]
fn the_capture_recognizes_aaves_non_operation_roles() {
    with_parsed_capture(|parsed, _evidence, _logs| {
        let roles: Vec<NonOperationRole> = parsed
            .ownership
            .owners()
            .filter_map(|(_, owner)| match owner {
                LogOwner::Role(role) => Some(role),
                LogOwner::Operation { .. } | LogOwner::OutOfScope(_) => None,
            })
            .collect();
        let count = |predicate: fn(&NonOperationRole) -> bool| {
            roles.iter().filter(|role| predicate(role)).count()
        };
        assert_eq!(
            count(|role| *role == NonOperationRole::ReserveReport),
            13,
            "one reserve accrual report per touched reserve"
        );
        assert_eq!(
            count(|role| *role == NonOperationRole::AccrualOnly),
            2,
            "two mints report accrued interest alone"
        );
        assert_eq!(
            count(|role| matches!(role, NonOperationRole::Companion { .. })),
            14,
            "every semantic Mint/Burn's zero-address report is a companion"
        );
        assert_eq!(
            count(|role| matches!(role, NonOperationRole::MovementMirror { .. })),
            6,
            "every liquidation movement's index-less mirror is a role"
        );

        assert_eq!(
            parsed.ownership.owner(70),
            Some(LogOwner::Role(NonOperationRole::ReserveReport)),
            "a ReserveDataUpdated report is a recognized non-operation role"
        );
        assert_eq!(
            parsed.ownership.owner(92),
            Some(LogOwner::Role(NonOperationRole::AccrualOnly)),
            "a mint whose value equals its balance increase reports accrual alone"
        );
        assert_eq!(
            parsed.ownership.owner(72),
            Some(LogOwner::Role(NonOperationRole::Companion {
                owner: 73,
                side: super::util::CompanionSide::Burn
            })),
            "the zero-address report of the Burn at 73 is that burn's companion"
        );
        assert_eq!(
            parsed.ownership.owner(90),
            Some(LogOwner::OutOfScope(OutOfScope::UntrackedToken)),
            "a reserve underlying's movement is not this market's scaled balance"
        );
        assert_eq!(
            parsed.ownership.owner(99),
            Some(LogOwner::OutOfScope(OutOfScope::ForeignEmitter)),
            "another protocol's log in the same transaction is deliberately ignored"
        );
        assert_eq!(
            parsed.ownership.owner(78),
            Some(LogOwner::Operation { operation_id: 0 }),
            "the first LiquidationCall is its operation's anchor"
        );
    });
}

/// Every liquidation's collateral legs were emitted inside the call frame that
/// closed it, and the same-user/same-collateral pair (96, 196) lives in disjoint
/// frames: the call path - not a transaction-wide log window - names each leg's
/// owner.
#[test]
fn every_collateral_leg_is_bound_to_its_own_call() {
    use crate::updater::evidence::TransactionEvidence;
    use crate::updater::operations_parser::liquidation::{CallOwnership, LiquidationCallScope};

    with_parsed_capture(|parsed, evidence, _logs| {
        let TransactionEvidence::TraceBacked(joined) = evidence else {
            panic!("a liquidation transaction is trace-backed");
        };
        let calls = CallOwnership::of(joined);

        let liquidations: Vec<_> = parsed
            .operations
            .iter()
            .filter(|op| {
                matches!(
                    op.operation_type,
                    OperationType::Liquidation | OperationType::GhoLiquidation
                )
            })
            .collect();
        assert_eq!(liquidations.len(), 6, "six LiquidationCall events");
        for op in &liquidations {
            let anchor = op
                .pool_event
                .and_then(|log| log.log_index)
                .expect("a liquidation carries its LiquidationCall");
            let scope = LiquidationCallScope::closing_at(anchor, &calls)
                .unwrap_or_else(|| panic!("the trace attributes LiquidationCall {anchor}"));
            for leg in op.scaled_events.iter().filter(|event| {
                matches!(
                    event.event_type,
                    ScaledTokenEventType::CollateralBurn
                        | ScaledTokenEventType::CollateralMint
                        | ScaledTokenEventType::CollateralTransfer
                )
            }) {
                let log_index = leg.log_index;
                assert!(
                    scope.owns(log_index, &calls),
                    "log {log_index} belongs to the call that closed liquidation {anchor} \
                     (call {:?})",
                    scope.call_path()
                );
            }
        }

        let first_weth = LiquidationCallScope::closing_at(96, &calls).unwrap();
        let second_weth = LiquidationCallScope::closing_at(196, &calls).unwrap();
        assert_ne!(
            first_weth.call_path(),
            second_weth.call_path(),
            "two liquidations of one user against one collateral asset are two calls"
        );
        assert!(
            first_weth.owns(94, &calls),
            "the first WETH liquidation's fee transfer is inside its own call"
        );
        assert!(
            second_weth.owns(191, &calls),
            "the second WETH liquidation owns the burn emitted in its own call"
        );
        assert!(
            !second_weth.owns(94, &calls) && !first_weth.owns(194, &calls),
            "each WETH liquidation's legs are outside the other's call"
        );
    });
}

/// The capture's operation outputs are the oracle: this is what the parse produced
/// before the ownership ledger and call-backed legs landed.
#[test]
fn the_capture_keeps_its_parsed_operations() {
    with_parsed_capture(|parsed, _evidence, _logs| {
        let produced: Vec<(OperationType, Option<u64>, Vec<u64>)> = parsed
            .operations
            .iter()
            .map(|op| {
                (
                    op.operation_type,
                    op.pool_event.and_then(|log| log.log_index),
                    op.scaled_events.iter().map(|ev| ev.log_index).collect(),
                )
            })
            .collect();
        assert_eq!(
            produced,
            vec![
                (OperationType::Liquidation, Some(78), vec![73, 85, 69, 76]),
                (OperationType::Liquidation, Some(96), vec![89, 94]),
                (OperationType::Withdraw, Some(120), vec![118]),
                (OperationType::Liquidation, Some(160), vec![153, 149, 158]),
                (OperationType::Liquidation, Some(172), vec![167, 163, 170]),
                (OperationType::Liquidation, Some(184), vec![179, 175, 182]),
                (OperationType::Liquidation, Some(196), vec![191, 187, 194]),
            ],
            "the pre-ledger operation outputs are the oracle"
        );
    });
}

/// A receipt-only parse of the captured transaction refuses: its six liquidations
/// name their legs by CALL, and a receipt cannot state a call path. The refusal
/// is `MissingCallEvidence`, never a silently window-attributed parse.
#[test]
fn the_capture_cannot_be_parsed_receipt_only() {
    let (_document, market, logs) = captured();
    let db = seeded_market(&market);
    let conn = db.lock();
    let mut substrate = ChunkSubstrate::lazy(market.chain_id);
    let mut parser = TransactionOperationsParser::new(
        1,
        market.chain_id,
        market.pool,
        Some(market.gho_token),
        Some(market.gho_vtoken),
        &conn,
        &mut substrate,
    )
    .expect("the parser resolves the market's pool revision");
    let events: Vec<&Log> = logs.iter().collect();

    match parser.parse(&events, [0u8; 32]) {
        Err(ParseError::MissingCallEvidence { log_index }) => assert_eq!(
            log_index, 78,
            "the first LiquidationCall's legs are named by a call"
        ),
        other => panic!("a receipt-only parse of a liquidation refuses: {other:?}"),
    }
}
