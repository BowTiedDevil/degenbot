use super::*;
use crate::state_lock::LockSite;
use degenbot_core::canonical_head::ChainAnchor;
use std::sync::Arc;

fn anchor(number: u64, tag: u8) -> ChainAnchor {
    ChainAnchor::new(
        1,
        number,
        alloy::primitives::B256::with_last_byte(tag),
        alloy::primitives::B256::with_last_byte(tag.wrapping_sub(1)),
    )
    .unwrap()
}

#[test]
fn a_cold_state_view_names_no_block() {
    let core = BotState::new();
    assert_eq!(
        core.state_anchor(),
        None,
        "a state that has seen no header names no block: never a zero default"
    );
}

#[test]
fn the_cheap_read_view_exposes_the_state_anchor_without_cloning() {
    let lock = Arc::new(crate::state_lock::StateLock::new(BotState::new()));
    let head = anchor(101, 0xaa);
    lock.write_at(LockSite::Pump).set_state_anchor(head);
    let view = lock.read_at(LockSite::Solver);
    assert_eq!(
        view.state_anchor(),
        Some(head),
        "the read guard serves the exact anchor of the block it was told about"
    );
    assert_eq!(view.state_anchor().map(|a| a.number()), Some(101));
}

#[test]
fn a_newer_header_replaces_the_state_anchor_and_a_rewound_one_may_too() {
    let mut core = BotState::new();
    let first = anchor(100, 0x10);
    core.set_state_anchor(first);
    let next = anchor(101, 0x20);
    core.set_state_anchor(next);
    assert_eq!(core.state_anchor(), Some(next), "the newest header wins");
    let rewritten = anchor(101, 0x30);
    core.set_state_anchor(rewritten);
    assert_eq!(
        core.state_anchor(),
        Some(rewritten),
        "a same-height replacement names the new branch, not the old one"
    );
}
