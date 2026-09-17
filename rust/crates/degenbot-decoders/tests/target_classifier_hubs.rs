#![allow(clippy::unwrap_used, clippy::panic)]

use alloy::hex::FromHex;
use alloy::primitives::Address;
use degenbot_decoders::target_classifier::{classify, OpaqueReason, RouterRegistry, TargetClass};

fn addr(s: &str) -> Address {
    Address::from_hex(s.trim_start_matches("0x")).unwrap()
}

#[test]
fn t13_live_observed_aggregator_hub_is_opaque_not_inert() {
    // 1inch v5 router observed live in the 30-min soak (logs/backrun/first_samples.json).
    let mut cd = vec![0xde, 0xad, 0xbe, 0xef];
    cd.extend_from_slice(&[0u8; 64]);
    let out = classify(
        addr("0x111111125421cA6dc452d289314280a0f8842A65"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    assert_eq!(out, TargetClass::Opaque(OpaqueReason::HubSelectorUnknown));
}

#[test]
fn t14_live_observed_v4_router_is_registered_hub() {
    // The v4 router appeared live on the searcher feed; unknown selectors are
    // HubInnerUndecodable (actionable hub, decode is a follow-up) - NOT Inert.
    let mut cd = vec![0x3c, 0xce, 0xcf, 0x5e];
    cd.extend_from_slice(&[0u8; 64]);
    let out = classify(
        addr("0x66a9893cC07D91D95644AEDD05D03f95e1dBA8Af"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    assert_eq!(out, TargetClass::Opaque(OpaqueReason::HubInnerUndecodable));
}
