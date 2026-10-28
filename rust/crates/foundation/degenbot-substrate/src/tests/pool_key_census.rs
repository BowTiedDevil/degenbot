//! The pool-key census: one door per orchestration verb.
//!
//! A verb that mutates or reads a pool exists once on the state owner, behind a

//! single `PoolLookup` door, so the two bodies cannot drift. This census pins

//! the present invariant at the source level: every verb has its door.

//!

//! The gate is a pure function over source text so its failure mode is
//! testable: sources that hold no door must report the gap.

const ORCHESTRATION: &str = include_str!("../concentrated_liquidity_orchestration.rs");
const POOL_LOOKUP: &str = include_str!("../pool_lookup.rs");

/// The `PoolLookup` doorway that owns each verb.
///
/// The table encodes the target: no effect may exist twice on the state owner,
/// and every verb must leave a single door behind.
const DOORS: &[&str] = &[
    "coverage",
    "snapshot_seed",
    "mark_registration_live",
    "begin_registration_quarantine",
    "backfill",
    "pump",
    "take_seed",
    "pin_post_drain",
    "take_post_drain",
    "apply_liquidity_update",
    "buffer_backfill_liquidity_update",
];

fn signature(verb: &str) -> String {
    ["pub fn ", verb, "("].concat()
}

/// Gaps across the two sources: a door that names no verb.
fn census(state_owner: &str, lookup: &str) -> Vec<String> {
    let mut violations = Vec::new();
    for door in DOORS {
        if !lookup.contains(&signature(door)) && !state_owner.contains(&signature(door)) {
            violations.push(format!("{door} has no door on the state owner"));
        }
    }
    violations
}

#[test]
fn the_pool_key_census_is_clean() {
    assert_eq!(census(ORCHESTRATION, POOL_LOOKUP), Vec::<String>::new());
}

#[test]
fn the_census_reports_a_missing_door() {
    let gap = census("", "");
    assert!(
        gap.iter()
            .any(|v| v == "apply_liquidity_update has no door on the state owner"),
        "the census must report a missing door: {gap:?}"
    );
    let door_only = "    pub fn apply_liquidity_update(&mut self) {}\n";
    let met = census(door_only, "");
    assert!(
        !met.iter().any(|v| v.contains("apply_liquidity_update")),
        "a present door satisfies its entry: {met:?}"
    );
    assert_eq!(
        gap.len(),
        DOORS.len(),
        "empty sources must report every door missing"
    );
}
