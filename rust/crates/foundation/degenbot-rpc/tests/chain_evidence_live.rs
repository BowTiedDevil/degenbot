//! The live proof of the exact-read request shape, at the captured liquidation
//! anchor.
//!
//! One `#[ignore]`d case (the fork-lane convention): it needs a chain-1
//! archive endpoint behind `DEGENBOT_RPC_HTTP_CHAINID_1` (or
//! `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`; the repo's `tests.env` declares the
//! devcontainer's reth). It proves three things the offline cases can only model:
//! the node serves a COMPLETE parent-linked header chain for the captured range,
//! it answers every EIP-1898 hash-pinned read at that anchor, and it refuses a
//! block hash it does not hold — so the pin is honored rather than ignored in
//! favour of a height.

#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

use std::str::FromStr;

use alloy::primitives::{Address, Bytes, B256, U256};
use degenbot_core::canonical_head::ChainAnchor;
use degenbot_rpc::chain_evidence::{
    acquire_header_chain, resolve_parent_anchor, BlockReads, ExactReadError,
};
use degenbot_rpc::provider::AlloyProvider;

/// The block the captured liquidation executed in
/// (`tests/fixtures/aave_receipts/liquidation_collateral_ownership_26458346.json`).
const ANCHOR_BLOCK: u64 = 26_458_346;
/// That block's own hash, as the receipt records it.
const ANCHOR_HASH: &str = "0xf65781d42c4d3240192a1f12dc69fb054641e65110b3749637cf433fe741d2b4";
/// The hash its header names as parent.
const ANCHOR_PARENT: &str = "0xcd8bf124ddaed7d896d2addd5a6979f78503efa0716558b3bff4c2eb71808890";
/// Aave V3 mainnet `POOL` — the contract the captured liquidation called.
const AAVE_V3_POOL: &str = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2";
/// `getReservesList()` — an ordinary state read, proving the call shape.
const GET_RESERVES_LIST: [u8; 4] = [0xd1, 0x94, 0x6d, 0xbc];

fn b256(raw: &str) -> B256 {
    B256::from_str(raw).expect("the fixture hash is 32 bytes")
}

fn pool() -> Address {
    Address::from_str(AAVE_V3_POOL).expect("the fixture address is 20 bytes")
}

#[tokio::test]
#[ignore = "live network: needs a chain-1 archive endpoint (DEGENBOT_RPC_HTTP_CHAINID_1 or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI)"]
async fn live_headers_and_hash_pinned_reads_hold_at_the_captured_liquidation_anchor() {
    let uri = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .expect(
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) to a \
             chain-1 archive endpoint",
        );
    let provider = AlloyProvider::new(&uri, 3)
        .await
        .expect("the endpoint answers");
    provider
        .bind_to_chain(1)
        .await
        .expect("the endpoint serves chain 1");

    let chain = acquire_header_chain(&provider, ANCHOR_BLOCK - 2, ANCHOR_BLOCK)
        .await
        .expect("the node serves the captured range's complete header chain");
    let anchor = chain.terminal();
    assert_eq!(
        chain.len(),
        3,
        "every height of the [anchor-2, anchor] range is named"
    );
    assert_eq!(chain.from_block(), ANCHOR_BLOCK - 2);
    assert_eq!(anchor.hash(), b256(ANCHOR_HASH));
    assert_eq!(anchor.parent_hash(), b256(ANCHOR_PARENT));

    let reads = BlockReads::at(&provider, anchor);
    let code = reads
        .code_at(pool())
        .await
        .expect("the node serves a hash-pinned code read");
    assert!(!code.is_empty(), "the pool holds code at the anchor");
    let reserves = reads
        .call(pool(), Bytes::from_static(&GET_RESERVES_LIST))
        .await
        .expect("the node serves a hash-pinned call");
    assert!(
        !reserves.is_empty(),
        "getReservesList answers at the anchor"
    );
    let _ = reads
        .storage_at(pool(), U256::ZERO)
        .await
        .expect("the node serves a hash-pinned storage read");
    let _ = reads
        .balance_at(pool())
        .await
        .expect("the node serves a hash-pinned balance read");

    // The pinned Multicall3 batch: the Aave verification fan-out's own shape.
    let batch = reads
        .batch(&[
            (pool(), Bytes::from_static(&GET_RESERVES_LIST)),
            (pool(), Bytes::from_static(&GET_RESERVES_LIST)),
        ])
        .await
        .expect("the node serves a hash-pinned aggregate3 batch");
    assert_eq!(batch.len(), 2, "one outcome per sub-call");
    assert!(
        batch.iter().all(|outcome| matches!(
            outcome,
            degenbot_rpc::multicall3::MulticallOutcome::Returned(_)
        )),
        "both `getReservesList` sub-calls return: {batch:?}"
    );

    // The parent read position an intentional pre-block stkAAVE seed uses.
    let parent = resolve_parent_anchor(&provider, anchor)
        .await
        .expect("the node states the anchor's parent header");
    assert_eq!(parent.number(), ANCHOR_BLOCK - 1);
    assert_eq!(parent.hash(), anchor.parent_hash());
    let parent_reads = BlockReads::at(&provider, parent);
    let _ = parent_reads
        .call(pool(), Bytes::from_static(&GET_RESERVES_LIST))
        .await
        .expect("the node serves a hash-pinned call at the parent anchor");

    let fabricated = ChainAnchor::new(
        1,
        ANCHOR_BLOCK,
        B256::repeat_byte(0x11),
        B256::repeat_byte(0x10),
    )
    .expect("the fixture anchor carries every identity field");
    let refusal = BlockReads::at(&provider, fabricated)
        .call(pool(), Bytes::new())
        .await
        .expect_err("a block hash the node does not hold is never served by height");
    assert!(
        matches!(refusal, ExactReadError::Failed { .. }),
        "a missing block is a failed read, not an unsupported capability: {refusal:?}"
    );
}
