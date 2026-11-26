//! The captured Aave report corpus: `tests/fixtures/aave_reports/report_role_captures.json`.
//!
//! One log per captured neutral report, with the contract role that emitted it,
//! the revision the capture reports, the exact block / transaction / log
//! identity, the complete topics and payload, the decoder's report name, and the
//! ownership role the market gives that report. The reader below is the only
//! interpreter of those bytes: it hands a test the PRODUCTION inputs — an alloy
//! `Log` plus the `ProtocolContext` the receipt is read against — so the
//! classification under test is `ReceiptOwnership::classify`'s, never a copy of
//! the catalog.

use std::collections::BTreeMap;

use alloy::primitives::Log as AlloyLog;
use alloy::primitives::{Address, Bytes, B256};
use alloy::rpc::types::Log;
use degenbot_decoders::aave_report_decoder::decode_aave_report_log;

use crate::operations::TokenType;
use crate::updater::operations_parser::ownership::NonOperationRole;
use crate::updater::protocol_events::{
    catalog_entry, ProtocolContext, ProtocolReport, ReportCatalogEntry, ReportedRevisions,
};

/// The schema this reader implements. A document that names another one is
/// refused: a new corpus shape is a new reader, not a reinterpretation.
pub(crate) const SCHEMA: &str = "degenbot.aave.report-role-captures/v1";

/// The committed capture document.
const CAPTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/aave_reports/report_role_captures.json"
);

/// Which contract's reported revision a case states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RevisionGoverns {
    Pool,
    GhoVtoken,
    DiscountToken,
}

/// One captured report log, with the role that emitted it and the ownership role
/// the market gives its report.
pub(crate) struct CapturedLog {
    pub(crate) role: Option<TokenType>,
    pub(crate) report: String,
    pub(crate) expected: NonOperationRole,
    pub(crate) log: Log,
}

/// One captured receipt: the reports one transaction emitted at one revision.
pub(crate) struct CapturedCase {
    pub(crate) id: String,
    pub(crate) reported_revision: u32,
    pub(crate) revision_governs: RevisionGoverns,
    pub(crate) pool: Address,
    pub(crate) logs: Vec<CapturedLog>,
}

/// Every captured case, in document order.
///
/// # Panics
///
/// When the committed document is missing, is not JSON, or states a schema this
/// reader does not implement: a fixture problem must stop the test, never
/// silently shrink its coverage.
pub(crate) fn cases() -> Vec<CapturedCase> {
    let bytes = std::fs::read(CAPTURES).expect("the committed role captures exist");
    let document: serde_json::Value =
        serde_json::from_slice(&bytes).expect("the role captures are JSON");
    assert_eq!(
        document["schema"]
            .as_str()
            .expect("the fixture states its schema"),
        SCHEMA,
        "the committed capture document states this reader's schema"
    );
    let pool: Address = document["market"]["pool"]
        .as_str()
        .expect("the market states its Pool")
        .parse()
        .expect("the Pool address parses");
    document["cases"]
        .as_array()
        .expect("the fixture carries its cases")
        .iter()
        .map(|case| captured_case(case, pool))
        .collect()
}

fn captured_case(case: &serde_json::Value, pool: Address) -> CapturedCase {
    let reported = &case["reported_revision"];
    let value = u32::try_from(
        reported["value"]
            .as_u64()
            .expect("the case states its reported revision"),
    )
    .expect("a revision fits in u32");
    let governs = match reported["governs"]
        .as_str()
        .expect("the case names its governor")
    {
        "POOL" => RevisionGoverns::Pool,
        "GHO_VTOKEN" => RevisionGoverns::GhoVtoken,
        "DISCOUNT_TOKEN" => RevisionGoverns::DiscountToken,
        other => panic!("the fixture names an unmodelled revision governor: {other}"),
    };
    CapturedCase {
        id: case["id"]
            .as_str()
            .expect("the case states its id")
            .to_string(),
        reported_revision: value,
        revision_governs: governs,
        pool,
        logs: case["logs"]
            .as_array()
            .expect("the case carries its logs")
            .iter()
            .map(captured_log)
            .collect(),
    }
}

fn captured_log(entry: &serde_json::Value) -> CapturedLog {
    let topics: Vec<B256> = entry["topics"]
        .as_array()
        .expect("the log states its topics")
        .iter()
        .map(|topic| {
            topic
                .as_str()
                .expect("a topic is a hex string")
                .parse()
                .expect("a topic is 32 bytes")
        })
        .collect();
    let data = alloy::hex::decode(
        entry["data"]
            .as_str()
            .expect("the log states its data")
            .trim_start_matches("0x"),
    )
    .expect("the payload is hex");
    CapturedLog {
        role: token_role_named(
            entry["contract_role"]
                .as_str()
                .expect("the log states its contract role"),
        ),
        report: entry["report"]
            .as_str()
            .expect("the log states its report")
            .to_string(),
        expected: role_named(
            entry["expected_role"]
                .as_str()
                .expect("the log states its expected role"),
        ),
        log: Log {
            inner: AlloyLog::new_unchecked(
                entry["emitter"]
                    .as_str()
                    .expect("the log states its emitter")
                    .parse()
                    .expect("the emitter address parses"),
                topics,
                Bytes::from(data),
            ),
            block_hash: Some(
                entry["block_hash"]
                    .as_str()
                    .expect("the log states its block hash")
                    .parse()
                    .expect("the block hash parses"),
            ),
            block_number: Some(
                entry["block_number"]
                    .as_u64()
                    .expect("the log states its block number"),
            ),
            block_timestamp: None,
            transaction_hash: Some(
                entry["transaction_hash"]
                    .as_str()
                    .expect("the log states its transaction hash")
                    .parse()
                    .expect("the transaction hash parses"),
            ),
            transaction_index: Some(
                entry["transaction_index"]
                    .as_u64()
                    .expect("the log states its transaction index"),
            ),
            log_index: Some(
                entry["log_index"]
                    .as_u64()
                    .expect("the log states its index"),
            ),
            removed: false,
        },
    }
}

fn role_named(name: &str) -> NonOperationRole {
    match name {
        "market configuration" => NonOperationRole::MarketConfiguration,
        "rewards accounting" => NonOperationRole::RewardsAccounting,
        "allowance report" => NonOperationRole::AllowanceReport,
        "authorization report" => NonOperationRole::AuthorizationReport,
        other => panic!("the fixture names an unmodelled role: {other}"),
    }
}

fn token_role_named(name: &str) -> Option<TokenType> {
    match name {
        "pool" => None,
        "aToken" => Some(TokenType::Atoken),
        "vToken" => Some(TokenType::Vtoken),
        "ghoDiscount" => Some(TokenType::GhoDiscount),
        other => panic!("the fixture names an unmodelled contract role: {other}"),
    }
}

/// The market context a case is read against: its Pool, the contract roles its
/// logs name, and the revision the capture reports.
pub(crate) fn context_of(case: &CapturedCase) -> ProtocolContext {
    let tokens: BTreeMap<Address, TokenType> = case
        .logs
        .iter()
        .filter_map(|entry| entry.role.map(|role| (entry.log.address(), role)))
        .collect();
    let mut revisions = ReportedRevisions::default();
    match case.revision_governs {
        RevisionGoverns::Pool => revisions.pool = Some(case.reported_revision),
        RevisionGoverns::GhoVtoken => revisions.gho_vtoken = Some(case.reported_revision),
        RevisionGoverns::DiscountToken => {
            revisions.discount_token = Some(case.reported_revision);
        }
    }
    ProtocolContext::new(case.pool, tokens, revisions)
}

/// The catalog's entry for one captured log, through the neutral decoder the
/// production path uses.
///
/// # Panics
///
/// When the log is not a neutral report, or the catalog does not place it for
/// the emitter role the fixture records — both are fixture defects.
pub(crate) fn entry_of(case: &CapturedCase, log: &CapturedLog) -> ReportCatalogEntry {
    let report = decode_aave_report_log(&log.log).unwrap_or_else(|| {
        panic!(
            "{}: log {:?} is a neutral report, not a skipped log",
            case.id, log.log.log_index
        )
    });
    let role = context_of(case)
        .emitter_role(log.log.address())
        .expect("the fixture states a tracked emitter");
    *catalog_entry(ProtocolReport::of(&report), role).unwrap_or_else(|| {
        panic!(
            "{}: {} is placed for the {} role",
            case.id,
            log.report,
            role.label()
        )
    })
}
