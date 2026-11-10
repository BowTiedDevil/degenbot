//! The startup BAL capability probe: an ON session proves the node can SERVE and
//! this tree can fully DECODE a canonical block access list before anything is
//! armed.
//!
//! Every test name states the behavior it pins. The scripted chain stub answers
//! `eth_blockNumber`, `eth_getBlockByNumber`, and both official EIP-7928
//! representations at one exact hash, recording every dispatched request, so a
//! test asserts the SEQUENCE the probe produced: the exact hash (never a mutable
//! tag), the representation the negotiation chose, and whether a failure switched
//! representations behind the caller's back.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the stub chain and its assertions fail loudly on a broken case"
)]

use alloy::primitives::B256;
use degenbot_simulation::sim::evm2::bal_capability::{
    probe_bal_capability, probe_bal_capability_at, BalCapability, BalCapabilityError,
};
use degenbot_simulation::sim::evm2::bal_prefetch::{fetch_bal_seed, BalPlan, BalSkip};

#[path = "../examples/shared/chain_stub.rs"]
#[expect(dead_code)]
mod chain_stub;

use chain_stub::ChainStub;

/// The height every head test reports.
const HEAD: u64 = 21_000_000;

/// The exact canonical hash of [`HEAD`].
fn head_hash() -> B256 {
    B256::new([0x11; 32])
}

/// A second exact hash — the head one block later, or the hash a later fetch
/// asks about.
fn next_hash() -> B256 {
    B256::new([0x22; 32])
}

/// A historical mainnet block that carries a block access list (the same pin the
/// raw/JSON parity leg uses).
const HISTORICAL_NUMBER: u64 = 0x140_0000;

/// The pinned historical hash.
fn historical_hash() -> B256 {
    "0x94aeabf2f5dc3d3c44897552aed94d49d3e28dee63d0c1a89ce50902675fcad6"
        .parse()
        .expect("the pinned block hash")
}

/// The worked list the parity suite hand-encoded, taken from the SHARED fixture
/// module so the bytes have one home: one account, four slot changes, two
/// read-only slots, and the three account-level change lists. A list this tree
/// can fully decode, so a probe success is a decode too.
fn raw_list() -> Vec<u8> {
    chain_stub::worked_list()
}

/// A stub serving [`HEAD`] and a decodable list at [`head_hash`].
fn serving_head() -> ChainStub {
    let stub = ChainStub::serving();
    stub.serve_head(HEAD, head_hash());
    stub.serve_bal_at(head_hash(), raw_list());
    stub
}

/// A raw-only node: the canonical head proves capability through the raw RLP
/// representation, at the head's EXACT hash and never a tag.
#[tokio::test]
async fn a_raw_only_node_proves_capability_at_the_exact_canonical_hash() {
    let stub = serving_head();
    stub.set_bal_json_unsupported(true);

    let capability = probe_bal_capability(&stub.provider())
        .await
        .expect("the raw list decodes");

    assert_eq!(capability, BalCapability::new(HEAD, head_hash()));
    assert_eq!(
        stub.methods(),
        vec![
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "debug_getRawBlockAccessList"
        ],
        "one head read, one lineage read, one list read"
    );
    let header = &stub.dispatched()[1];
    assert!(
        header.starts_with(&format!("eth_getBlockByNumber [\"0x{HEAD:x}\",false]")),
        "the head's own number, never a tag: {header}"
    );
    assert_eq!(
        stub.dispatched()[2],
        format!("debug_getRawBlockAccessList [\"{}\"]", head_hash()),
        "the list is asked at the exact hash the probe resolved"
    );
}

/// An eth-only node proves capability through the official JSON representation,
/// and the provider's sticky negotiation keeps every LATER fetch on JSON rather
/// than paying the raw refusal again per head.
#[tokio::test]
async fn an_eth_only_node_proves_capability_and_the_session_stays_on_json() {
    let stub = serving_head();
    stub.set_bal_raw_unsupported(true);
    let provider = stub.provider();

    probe_bal_capability(&provider)
        .await
        .expect("the JSON list decodes");
    assert_eq!(
        stub.methods(),
        vec![
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "debug_getRawBlockAccessList",
            "eth_getBlockAccessList"
        ],
        "the raw refusal costs one request, then the JSON representation answers"
    );

    stub.clear();
    stub.serve_bal_at(next_hash(), raw_list());
    let seed = fetch_bal_seed(&provider, next_hash(), &BalPlan::default())
        .await
        .expect("a later head's list decodes");

    assert!(!seed.writes.is_empty(), "the later list seeded: {seed:?}");
    assert_eq!(
        stub.methods(),
        vec!["eth_getBlockAccessList"],
        "the session is sticky: no second raw attempt"
    );
}

/// A node serving both representations answers from the raw one first — the
/// preference the probe inherits from the shared acquisition.
#[tokio::test]
async fn a_dual_endpoint_node_proves_capability_through_the_raw_method_first() {
    let stub = serving_head();

    probe_bal_capability(&stub.provider())
        .await
        .expect("both lists decode");

    assert_eq!(
        stub.methods(),
        vec![
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "debug_getRawBlockAccessList"
        ],
        "raw first, and no JSON request at all"
    );
    assert_eq!(stub.bal_requests(), 1, "one acquisition");
}

/// An EMPTY list is a WELL-FORMED EIP-7928 answer, not missing or malformed
/// data: the specification permits a block with no account changes, so a list
/// this tree fully decodes proves capability however few entries it carries.
/// Emptiness is deliberately NOT a refusal here — a future change that conflates
/// "no changes" with "no answer" fails this test.
#[tokio::test]
async fn a_well_formed_empty_raw_list_proves_capability() {
    let stub = ChainStub::serving();
    stub.serve_head(HEAD, head_hash());
    // One RLP empty list: zero accounts, a complete and valid list.
    stub.serve_bal_at(head_hash(), vec![0xc0]);

    let capability = probe_bal_capability(&stub.provider())
        .await
        .expect("an empty list is a valid list");

    assert_eq!(capability, BalCapability::new(HEAD, head_hash()));
    assert_eq!(
        stub.methods(),
        vec![
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "debug_getRawBlockAccessList"
        ],
        "the empty list was acquired and decoded, not refused"
    );
}

/// The same emptiness through the OFFICIAL JSON representation: `[]` decodes as
/// zero accounts, so an eth-only node serving it is capable.
#[tokio::test]
async fn a_well_formed_empty_json_list_proves_capability() {
    let stub = ChainStub::serving();
    stub.serve_head(HEAD, head_hash());
    stub.set_bal_raw_unsupported(true);
    stub.serve_bal_json_at(head_hash(), serde_json::json!([]));

    let capability = probe_bal_capability(&stub.provider())
        .await
        .expect("an empty JSON list is a valid list");

    assert_eq!(capability, BalCapability::new(HEAD, head_hash()));
    assert_eq!(
        stub.methods(),
        vec![
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "debug_getRawBlockAccessList",
            "eth_getBlockAccessList"
        ],
        "the raw refusal costs one request, then the empty JSON list answers"
    );
}

/// A node serving neither representation is a startup refusal that names the
/// operation, the block, BOTH official methods, the cause, and the remediation.
#[tokio::test]
async fn a_node_serving_neither_method_fails_with_both_methods_and_the_remediation() {
    let stub = serving_head();
    stub.set_bal_unsupported(true);

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("neither representation works");

    assert!(
        matches!(
            error,
            BalCapabilityError::Unusable {
                cause: BalSkip::Rpc(_),
                ..
            }
        ),
        "{error:?}"
    );
    let message = error.to_string();
    for needle in [
        "debug_getRawBlockAccessList",
        "eth_getBlockAccessList",
        "21000000",
        &format!("{:#x}", head_hash()),
        "simulation.delta_dispatch",
    ] {
        assert!(message.contains(needle), "{needle} missing from: {message}");
    }
}

/// A failure of the raw representation on a REAL block is reported as-is: the
/// probe never retries the acquisition through the other representation, exactly
/// like the shared acquisition it reuses.
#[tokio::test]
async fn a_real_block_failure_on_the_raw_method_does_not_switch_to_json() {
    let stub = ChainStub::serving();
    stub.serve_head(HEAD, head_hash());
    // JSON could answer this hash; the raw method answers a real block failure.
    stub.serve_bal_json_at(
        head_hash(),
        serde_json::json!([{
            "address": "0x1111111111111111111111111111111111111111",
            "storageChanges": [{"key": "0x1", "changes": [{"index": "0x1", "value": "0x7"}]}],
            "storageReads": [],
            "balanceChanges": [],
            "nonceChanges": [],
            "codeChanges": [],
        }]),
    );

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("the chosen representation failed");

    assert!(
        matches!(
            error,
            BalCapabilityError::Unusable {
                cause: BalSkip::Rpc(_),
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        stub.methods(),
        vec![
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "debug_getRawBlockAccessList"
        ],
        "a real block failure is NOT a method-absence fallback"
    );
}

/// The current head's NUMBER is unreadable: construction fails naming the
/// operation, and no list is ever asked for.
#[tokio::test]
async fn an_unreadable_head_number_fails_the_probe() {
    let stub = serving_head();
    stub.set_head_unreadable(true);

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("the head number cannot be read");

    assert!(
        matches!(error, BalCapabilityError::HeadNumber { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains("canonical-header availability"),
        "a lineage failure directs the operator at the node, not at BAL support: {message}"
    );
    assert!(
        !message.contains("simulation.delta_dispatch"),
        "no BAL-setting remediation on a failure that never reached a BAL method: {message}"
    );
    assert_eq!(
        stub.methods(),
        vec!["eth_blockNumber"],
        "nothing else asked"
    );
}

/// The head's exact lineage is refused by the node: no guessed hash, no list
/// request, a startup error naming the height.
#[tokio::test]
async fn an_unreadable_head_lineage_fails_the_probe() {
    let stub = serving_head();
    stub.set_lineage_unreadable(true);

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("the lineage cannot be read");

    assert!(
        matches!(error, BalCapabilityError::Lineage { number, .. } if number == HEAD),
        "{error:?}"
    );
    assert_eq!(stub.bal_requests(), 0, "no hash, no list request");
}

/// A node with no canonical block at the reported height has no head to prove
/// capability at: the probe refuses rather than asking a tag.
#[tokio::test]
async fn an_unknown_canonical_head_fails_the_probe() {
    let stub = ChainStub::serving();
    // The head number is reported; its header is unknown.
    stub.set_head_number(HEAD);

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("no canonical block at the head number");

    assert!(
        matches!(error, BalCapabilityError::UnknownHead { number } if number == HEAD),
        "{error:?}"
    );
    let message = error.to_string();
    for needle in [
        "no complete canonical lineage",
        "no block at that number, or a header missing its hash or parentHash",
        "canonical-header availability",
    ] {
        assert!(message.contains(needle), "{needle} missing from: {message}");
    }
    assert!(
        !message.contains("simulation.delta_dispatch"),
        "an absent/incomplete header is not a BAL-support problem: {message}"
    );
    assert_eq!(stub.bal_requests(), 0);
}

/// A transport that answers nothing is a startup failure, not a silent OFF.
#[tokio::test]
async fn a_transport_failure_fails_the_probe() {
    let stub = serving_head();
    stub.set_down(true);

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("the node is down");

    assert!(
        matches!(error, BalCapabilityError::HeadNumber { .. }),
        "{error:?}"
    );
}

/// The official JSON method answering `null` (no list at that hash) fails the
/// probe after the raw method was refused.
#[tokio::test]
async fn a_null_json_answer_fails_the_probe() {
    let stub = serving_head();
    stub.set_bal_raw_unsupported(true);
    stub.set_bal_null(true);

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("null is not a list");

    assert!(
        matches!(
            error,
            BalCapabilityError::Unusable {
                cause: BalSkip::Rpc(_),
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains("eth_getBlockAccessList"));
}

/// A non-array JSON answer is refused at the transport seam.
#[tokio::test]
async fn a_non_array_json_answer_fails_the_probe() {
    let stub = serving_head();
    stub.set_bal_raw_unsupported(true);
    stub.serve_bal_json_at(head_hash(), serde_json::json!({"accounts": []}));

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("an object is not a list");

    assert!(
        matches!(
            error,
            BalCapabilityError::Unusable {
                cause: BalSkip::Rpc(_),
                ..
            }
        ),
        "{error:?}"
    );
}

/// A malformed RAW list is a DECODE failure: the fetch succeeded, the bytes are
/// not one complete EIP-7928 list.
#[tokio::test]
async fn a_malformed_raw_list_fails_the_probe() {
    let stub = ChainStub::serving();
    stub.serve_head(HEAD, head_hash());
    stub.serve_bal_at(head_hash(), vec![0xff, 0x00]);

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("the bytes are not one list");

    assert!(
        matches!(
            error,
            BalCapabilityError::Unusable {
                cause: BalSkip::Decode(_),
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains("simulation.delta_dispatch"));
}

/// A JSON list missing an official account field is a DECODE failure — the same
/// closed refusal malformed RLP gets.
#[tokio::test]
async fn a_malformed_json_list_fails_the_probe() {
    let stub = serving_head();
    stub.set_bal_raw_unsupported(true);
    stub.serve_bal_json_at(
        head_hash(),
        serde_json::json!([{"address": "0x1111111111111111111111111111111111111111"}]),
    );

    let error = probe_bal_capability(&stub.provider())
        .await
        .expect_err("a missing field is not one list");

    assert!(
        matches!(
            error,
            BalCapabilityError::Unusable {
                cause: BalSkip::Decode(_),
                ..
            }
        ),
        "{error:?}"
    );
}

/// The explicit-hash helper proves capability at ONE named block and reads no
/// head at all — the seam a fixed historical pin (and the live leg) uses.
#[tokio::test]
async fn the_explicit_hash_helper_probes_one_named_block_without_reading_the_head() {
    let stub = ChainStub::serving();
    stub.serve_bal_at(historical_hash(), raw_list());

    let capability =
        probe_bal_capability_at(&stub.provider(), HISTORICAL_NUMBER, historical_hash())
            .await
            .expect("the named block's list decodes");

    assert_eq!(
        capability,
        BalCapability::new(HISTORICAL_NUMBER, historical_hash())
    );
    assert_eq!(
        stub.methods(),
        vec!["debug_getRawBlockAccessList"],
        "no head read, no lineage read, no tag"
    );
}

/// LIVE: the production probe proves capability at the node's CURRENT canonical
/// head, and the explicit-hash helper proves it at the fixed historical block the
/// raw/JSON parity leg pins.
///
/// Run through the fork lane (`just test-fork`), or directly with
/// `DEGENBOT_RPC_HTTP_CHAINID_1` set.
#[tokio::test]
#[ignore = "live: needs the mainnet archive node (just test-fork)"]
async fn live_the_capability_probe_reads_the_canonical_head_and_one_fixed_historical_block() {
    use alloy::providers::ProviderBuilder;
    use degenbot_rpc::provider::AlloyProvider;
    use std::sync::Arc;

    let rpc_url =
        std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").expect("the fork lane sets the endpoint");
    let root = ProviderBuilder::default().connect_http(rpc_url.parse().expect("the endpoint URL"));
    let provider = AlloyProvider::from_provider(Arc::new(root));

    let fixed = probe_bal_capability_at(&provider, HISTORICAL_NUMBER, historical_hash())
        .await
        .expect("the pinned historical block's list decodes");
    assert_eq!(fixed.hash, historical_hash());
    assert_eq!(fixed.number, HISTORICAL_NUMBER);

    let head = probe_bal_capability(&provider)
        .await
        .expect("the node's current canonical head carries a readable list");
    assert_ne!(head.hash, B256::ZERO, "the probe resolved a real hash");
}
