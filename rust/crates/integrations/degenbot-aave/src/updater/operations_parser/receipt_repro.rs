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

    let parsed = parser.parse(&events, [0u8; 32]).unwrap_or_else(|e| {
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
