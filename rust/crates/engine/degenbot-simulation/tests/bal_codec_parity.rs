//! One list, two official representations, one seed.
//!
//! The raw RLP a `debug_getRawBlockAccessList` answer carries and the JSON array
//! `eth_getBlockAccessList` answers must be interchangeable below the codec: same
//! final writes (last-write-wins, zero values included), same read-only
//! identities, same plan filtering and capping, same closed failure on malformed
//! input.
//!
//! The fixtures are authored independently of the codecs: the raw one is
//! hand-encoded RLP, and the JSON ones spell the same list in execution-apis'
//! official field names and in the alternate names alloy's EIP-7928 types also
//! accept. The expected seed is written out in each test, so it comes from
//! outside the codecs under test.

#![expect(
    clippy::expect_used,
    reason = "the fixtures and their assertions fail loudly on a broken case"
)]

use alloy::primitives::{Address, B256, U256};
use degenbot_simulation::sim::evm2::bal_prefetch::{
    decode_bal_seed, decode_bal_seed_json, BalPlan, BalSeed,
};
use serde_json::json;

/// The account both representations carry.
const A: Address = Address::new([0x11; 20]);

/// A LIVE fixed-hash parity check: the archive node answers BOTH official
/// representations of one historical block, they decode to identical seeds, and
/// the production acquisition prefers the raw one when the node serves it.
///
/// Run through the fork lane (`just test-fork`), which sets the endpoint.
#[tokio::test]
#[ignore = "live: needs the mainnet archive node (just test-fork)"]
async fn live_raw_and_json_block_access_lists_of_one_hash_decode_to_identical_seeds() {
    use alloy::providers::ProviderBuilder;
    use degenbot_rpc::provider::{AlloyProvider, BlockAccessListResponse};
    use std::sync::Arc;

    let rpc_url =
        std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").expect("the fork lane sets the endpoint");
    let root = ProviderBuilder::default().connect_http(rpc_url.parse().expect("the endpoint URL"));
    let provider = AlloyProvider::from_provider(Arc::new(root));

    // A historical mainnet block that carries a block access list.
    let hash: B256 = "0x94aeabf2f5dc3d3c44897552aed94d49d3e28dee63d0c1a89ce50902675fcad6"
        .parse()
        .expect("the pinned block hash");

    let raw = provider
        .get_raw_block_access_list(hash)
        .await
        .expect("the node serves the raw list");
    let json = provider
        .get_block_access_list_json(hash)
        .await
        .expect("the node serves the official JSON list");

    let raw_seed = decode_bal_seed(&raw, &BalPlan::default()).expect("the raw list decodes");
    let json_seed =
        decode_bal_seed_json(&json, &BalPlan::default()).expect("the JSON list decodes");
    assert!(
        !raw_seed.writes.is_empty(),
        "the pinned block writes storage: {raw_seed:?}"
    );
    assert_eq!(
        raw_seed, json_seed,
        "one list, one seed: identical final writes and read identities"
    );

    // The acquisition prefers raw on a node that serves both.
    let acquired = provider
        .get_block_access_list(hash)
        .await
        .expect("the acquisition answers");
    let acquired_seed = match acquired {
        BlockAccessListResponse::Raw(bytes) => {
            assert_eq!(bytes, raw, "the same raw bytes");
            decode_bal_seed(&bytes, &BalPlan::default())
        }
        BlockAccessListResponse::Json(value) => decode_bal_seed_json(&value, &BalPlan::default()),
    }
    .expect("the acquired list decodes");
    assert_eq!(
        acquired_seed, raw_seed,
        "the acquisition answers the same list"
    );
}

/// The worked list as hand-encoded RLP: one account
/// (`0x1111...11`) whose slot `0x01` is written at index 0 (7) and index 2 (42)
/// — final 42 — whose slot `0x02` is written at index 1 (1000) and index 3 (0) —
/// final ZERO, a deletion — with read-only slots `0x03` and `0x04`, one balance
/// change, one nonce change, and one code change.
fn worked_rlp() -> Vec<u8> {
    alloy::hex::decode(
        "f845f843941111111111111111111111111111111111111111d4c801c6c28007c2022aca02c8c4018203e8c20380c20304cbca05880de0b6b3a7640000c3c2062ac5c407826000",
    )
    .expect("the fixture is valid hex")
}

/// The worked list in the ALTERNATE field names alloy's EIP-7928 types also
/// accept (`slot`, `slotChanges`, `blockAccessIndex`, `postValue`,
/// `postBalance`, `postNonce`, `newCode`). Not execution-apis' spelling — that is
/// [`worked_json_official`] — and kept apart so a node answering either wording
/// is covered.
fn worked_json() -> serde_json::Value {
    json!([{
        "address": "0x1111111111111111111111111111111111111111",
        "storageChanges": [
            {
                "slot": "0x01",
                "slotChanges": [
                    {"blockAccessIndex": "0x00", "postValue": "0x07"},
                    {"blockAccessIndex": "0x02", "postValue": "0x2a"},
                ],
            },
            {
                "slot": "0x02",
                "slotChanges": [
                    {"blockAccessIndex": "0x01", "postValue": "0x03e8"},
                    {"blockAccessIndex": "0x03", "postValue": "0x00"},
                ],
            },
        ],
        "storageReads": ["0x03", "0x04"],
        "balanceChanges": [{"blockAccessIndex": "0x05", "postBalance": "0x0de0b6b3a7640000"}],
        "nonceChanges": [{"blockAccessIndex": "0x06", "postNonce": "0x2a"}],
        "codeChanges": [{"blockAccessIndex": "0x07", "newCode": "0x6000"}],
    }])
}

/// The same list in the field names execution-apis specifies for
/// `eth_getBlockAccessList`: `key`/`changes` for a written slot, `index`/`value`
/// for one change, `code` for a code change, and bare slot keys for a read.
/// Authored from that spec, never serialized from the type under test.
fn worked_json_official() -> serde_json::Value {
    json!([{
        "address": "0x1111111111111111111111111111111111111111",
        "storageChanges": [
            {
                "key": "0x1",
                "changes": [
                    {"index": "0x0", "value": "0x7"},
                    {"index": "0x2", "value": "0x2a"},
                ],
            },
            {
                "key": "0x2",
                "changes": [
                    {"index": "0x1", "value": "0x3e8"},
                    {"index": "0x3", "value": "0x0"},
                ],
            },
        ],
        "storageReads": ["0x3", "0x4"],
        "balanceChanges": [{"index": "0x5", "value": "0xde0b6b3a7640000"}],
        "nonceChanges": [{"index": "0x6", "value": "0x2a"}],
        "codeChanges": [{"index": "0x7", "code": "0x6000"}],
    }])
}

/// The seed the worked list must produce: the LAST change of each written slot
/// (42, and the zero a deletion writes), then the read-only identities, in list
/// order.
fn worked_seed() -> BalSeed {
    BalSeed {
        writes: vec![
            (A, U256::from(1u64), U256::from(42u64)),
            (A, U256::from(2u64), U256::from(0u64)),
        ],
        reads: vec![(A, U256::from(3u64)), (A, U256::from(4u64))],
        filtered_entries: 0,
        capped_entries: 0,
    }
}

/// The raw RLP and BOTH JSON wordings of one list decode to the SAME seed.
#[test]
fn the_raw_rlp_and_every_json_wording_of_one_list_decode_to_the_same_seed() {
    let raw = decode_bal_seed(&worked_rlp(), &BalPlan::default())
        .expect("the hand-encoded RLP is one list");
    let official = decode_bal_seed_json(&worked_json_official(), &BalPlan::default())
        .expect("execution-apis' JSON is one list");
    let aliases = decode_bal_seed_json(&worked_json(), &BalPlan::default())
        .expect("the alternate field names are the same list");
    assert_eq!(raw, worked_seed(), "the raw seed is the expected one");
    assert_eq!(official, raw, "execution-apis' JSON: identical seed");
    assert_eq!(aliases, raw, "alloy's alternate names: identical seed");
}

/// An empty list seeds nothing in either representation, and is NOT a failure.
#[test]
fn an_empty_list_is_an_empty_seed_in_both_representations() {
    let raw = decode_bal_seed(&[0xc0], &BalPlan::default()).expect("the empty RLP is one list");
    let official =
        decode_bal_seed_json(&json!([]), &BalPlan::default()).expect("the empty JSON is one list");
    assert_eq!(raw, BalSeed::default());
    assert_eq!(official, raw);
}

/// A plan filters and caps identically whichever representation answered: the
/// omitted entries are counted, never silently dropped.
#[test]
fn a_plan_filters_and_caps_both_representations_identically() {
    const B: Address = Address::new([0x22; 20]);
    let filtered = BalPlan {
        addresses: Some(vec![B]),
        ..BalPlan::default()
    };
    let capped = BalPlan {
        max_write_seeds: Some(1),
        max_read_prefetches: Some(1),
        ..BalPlan::default()
    };

    for plan in [&filtered, &capped, &BalPlan::default()] {
        let raw = decode_bal_seed(&worked_rlp(), plan).expect("the raw list decodes");
        let aliases =
            decode_bal_seed_json(&worked_json(), plan).expect("the alias-named JSON decodes");
        assert_eq!(raw, aliases, "plan {plan:?}: identical seeds");
    }

    let raw = decode_bal_seed(&worked_rlp(), &filtered).expect("the raw list decodes");
    assert_eq!(raw.filtered_entries, 4, "slots plus reads of one account");
    assert!(raw.writes.is_empty() && raw.reads.is_empty());

    let raw = decode_bal_seed(&worked_rlp(), &capped).expect("the raw list decodes");
    assert_eq!(raw.writes.len(), 1, "{raw:?}");
    assert_eq!(raw.reads.len(), 1, "{raw:?}");
    assert_eq!(raw.capped_entries, 2, "{raw:?}");
}

/// All six official account fields decode: the three non-storage change lists are
/// accepted, and they contribute no storage identity (those accounts stay on the
/// dynamic path).
#[test]
fn the_official_json_decodes_all_six_account_fields() {
    let seed = decode_bal_seed_json(&worked_json_official(), &BalPlan::default())
        .expect("all six fields are present");
    assert_eq!(seed.writes.len(), 2, "storage writes only: {seed:?}");
    assert_eq!(seed.reads.len(), 2, "storage reads only: {seed:?}");
}

/// A JSON object that only carries a balance change still yields NO account
/// identity — and no failure.
#[test]
fn an_account_with_only_account_level_changes_seeds_nothing() {
    let seed = decode_bal_seed_json(
        &json!([{
            "address": "0x2222222222222222222222222222222222222222",
            "storageChanges": [],
            "storageReads": [],
            "balanceChanges": [{"index": "0x1", "value": "0x1"}],
            "nonceChanges": [{"index": "0x2", "value": "0x1"}],
            "codeChanges": [{"index": "0x3", "code": "0x6000"}],
        }]),
        &BalPlan::default(),
    )
    .expect("the official shape decodes");
    assert_eq!(seed, BalSeed::default());
}

/// Malformed input is a CLOSED failure in either representation: nothing is
/// seeded, and no partial seed survives.
#[test]
fn malformed_input_fails_closed_in_both_representations() {
    for (name, raw) in [
        ("not RLP at all", vec![0xff, 0x00]),
        ("a valid list plus trailing junk", {
            let mut bytes = worked_rlp();
            bytes.push(0x00);
            bytes
        }),
    ] {
        let result = decode_bal_seed(&raw, &BalPlan::default());
        assert!(result.is_err(), "{name}: must not decode: {result:?}");
    }

    for (name, json) in [
        ("an object, not a list", json!({"accounts": []})),
        ("a string", json!("0xdeadbeef")),
        ("null", json!(null)),
        (
            "a missing account field",
            json!([{"address": "0x1111111111111111111111111111111111111111"}]),
        ),
        (
            "an ill-typed address",
            json!([{
                "address": 17,
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [],
                "nonceChanges": [],
                "codeChanges": [],
            }]),
        ),
        (
            "an unparseable slot",
            json!([{
                "address": "0x1111111111111111111111111111111111111111",
                "storageChanges": [{"key": "0xzz", "changes": []}],
                "storageReads": [],
                "balanceChanges": [],
                "nonceChanges": [],
                "codeChanges": [],
            }]),
        ),
        (
            "a non-numeric block access index",
            json!([{
                "address": "0x1111111111111111111111111111111111111111",
                "storageChanges": [{
                    "key": "0x1",
                    "changes": [{"index": "later", "value": "0x1"}],
                }],
                "storageReads": [],
                "balanceChanges": [],
                "nonceChanges": [],
                "codeChanges": [],
            }]),
        ),
    ] {
        let result = decode_bal_seed_json(&json, &BalPlan::default());
        assert!(result.is_err(), "{name}: must not decode: {result:?}");
    }
}
