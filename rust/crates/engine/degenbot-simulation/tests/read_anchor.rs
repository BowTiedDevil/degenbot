//! Behavior pins for [`ReadAnchor`]: the two observation forms, the one total
//! block-id conversion, and the identity the memo and lifecycle maps key on.

#![expect(
    clippy::expect_used,
    reason = "the fixture chain anchors fail loudly on an incomplete identity"
)]

use std::collections::HashSet;
use std::sync::Arc;

use alloy::eips::{BlockId, RpcBlockHash};
use alloy::primitives::B256;
use degenbot_core::canonical_head::ChainAnchor;
use degenbot_simulation::sim::{ReadAnchor, StorageMemoCache};

const HEIGHT: u64 = 21_000_000;
const CANONICAL: B256 = B256::new([0xaa; 32]);
const REPLACEMENT: B256 = B256::new([0xbb; 32]);
const PARENT: B256 = B256::new([0x01; 32]);

#[test]
fn an_identified_anchor_converts_to_a_canonical_hash_pin() {
    let anchor = ReadAnchor::identified(HEIGHT, CANONICAL);
    assert_eq!(
        anchor.block_id(),
        BlockId::Hash(RpcBlockHash::from_hash(CANONICAL, Some(true))),
        "an identified anchor names its block by hash, canonically required"
    );
}

#[test]
fn an_unresolved_anchor_converts_to_its_number() {
    let anchor = ReadAnchor::unresolved(HEIGHT);
    assert_eq!(
        anchor.block_id(),
        BlockId::number(HEIGHT),
        "the ingress observation names only the height it could resolve"
    );
}

#[test]
fn an_identified_anchor_never_downgrades_to_a_number_pin() {
    let anchor = ReadAnchor::identified(HEIGHT, CANONICAL);
    assert_ne!(
        anchor.block_id(),
        BlockId::number(HEIGHT),
        "a named block is never requested as its height"
    );
}

#[test]
fn the_accessors_report_the_form_that_was_constructed() {
    let identified = ReadAnchor::identified(HEIGHT, CANONICAL);
    assert_eq!(identified.number(), HEIGHT);
    assert_eq!(identified.hash(), Some(CANONICAL));
    assert!(identified.is_identified());
    assert!(!identified.is_unresolved());

    let unresolved = ReadAnchor::unresolved(HEIGHT);
    assert_eq!(unresolved.number(), HEIGHT);
    assert_eq!(
        unresolved.hash(),
        None,
        "an unresolved anchor names no block hash"
    );
    assert!(unresolved.is_unresolved());
    assert!(!unresolved.is_identified());

    assert_ne!(
        identified, unresolved,
        "an unnamed hash is not the named block at the same height"
    );
}

#[test]
fn a_chain_anchor_converts_only_to_the_identified_form() {
    let chain =
        ChainAnchor::new(1, HEIGHT, CANONICAL, PARENT).expect("the fixture names every field");
    let anchor = ReadAnchor::from(chain);

    assert_eq!(anchor, ReadAnchor::identified(HEIGHT, CANONICAL));
    assert!(
        anchor.is_identified(),
        "a resolved chain anchor can never convert to the unresolved form"
    );
    assert_eq!(anchor.hash(), Some(CANONICAL));
    assert_eq!(
        anchor.block_id(),
        BlockId::Hash(RpcBlockHash::from_hash(CANONICAL, Some(true)))
    );
}

#[test]
fn the_anchor_hash_identity_follows_the_whole_observation() {
    let canonical = ReadAnchor::identified(HEIGHT, CANONICAL);
    let replacement = ReadAnchor::identified(HEIGHT, REPLACEMENT);
    let unresolved = ReadAnchor::unresolved(HEIGHT);

    let mut seen = HashSet::new();
    assert!(
        seen.insert(canonical),
        "the canonical block is one identity"
    );
    assert!(
        seen.insert(replacement),
        "the same-height replacement is a different identity"
    );
    assert!(
        seen.insert(unresolved),
        "the unresolved observation is a third identity"
    );
    assert!(
        !seen.insert(ReadAnchor::identified(HEIGHT, CANONICAL)),
        "the same observation is the same identity"
    );
}

#[test]
fn same_height_anchors_on_different_hashes_stay_unequal_and_scope_separately() {
    let canonical = ReadAnchor::identified(HEIGHT, CANONICAL);
    let replacement = ReadAnchor::identified(HEIGHT, REPLACEMENT);
    assert_eq!(canonical.number(), replacement.number());
    assert_ne!(
        canonical, replacement,
        "the hash is half of the anchor's identity"
    );

    let cache = StorageMemoCache::new();
    let first = cache.for_anchor(canonical);
    let second = cache.for_anchor(replacement);
    assert!(
        !Arc::ptr_eq(&first, &second),
        "a same-height replacement mints its own memo scope"
    );
    assert_eq!(cache.anchor(), Some(replacement));
}
