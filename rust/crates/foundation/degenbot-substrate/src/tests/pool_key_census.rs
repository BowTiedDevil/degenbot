//! The pool-key census: one door per orchestration verb.
//!
//! A verb that mutates or reads a pool used to exist twice on the state owner —
//! once keyed by contract address, once by `(pool_manager, pool_id)` — and the
//! two bodies could drift. [`PoolLookup`] owns the key shape instead, so this
//! census pins the invariant at the source level: the address-keyed twin must
//! not reappear, and the door must exist for every verb that ever had one.
//!
//! The gate is a pure function over source text so its failure mode is
//! testable: a synthetic fixture with the old shape must go red (the
//! negative-probe idiom every census gate in this tree carries).

const ORCHESTRATION: &str = include_str!("../concentrated_liquidity_orchestration.rs");
const POOL_LOOKUP: &str = include_str!("../pool_lookup.rs");

/// Each retired `_by_key` twin and the `PoolLookup` doorway that replaced it.
///
/// The table encodes the target: no effect may exist twice on the state owner,
/// and every retired twin must leave a single door behind.
const DOORS: &[(&str, &str)] = &[
    ("pool_coverage_by_key", "coverage"),
    ("set_pool_live_by_key", "mark_registration_live"),
    (
        "set_pool_quarantined_by_key",
        "begin_registration_quarantine",
    ),
    ("apply_backfill_buffer_by_key", "backfill"),
    ("apply_pump_buffer_by_key", "pump"),
    ("take_snapshot_seed_by_key", "take_seed"),
    ("pin_post_drain_snapshot_by_key", "pin_post_drain"),
    ("take_post_drain_snapshot_by_key", "take_post_drain"),
];

fn signature(verb: &str) -> String {
    ["pub fn ", verb, "("].concat()
}

/// Violations across the two sources: a surviving address-keyed twin, or a
/// door that names no verb.
fn census(state_owner: &str, lookup: &str) -> Vec<String> {
    let mut violations = Vec::new();
    for (retired, door) in DOORS {
        if state_owner.contains(&signature(retired)) {
            violations.push(format!(
                "{retired}(address) is back: the PoolLookup door owns this verb"
            ));
        }
        if !lookup.contains(&signature(door)) {
            violations.push(format!("{retired} has no PoolLookup::{door} door"));
        }
    }
    violations
}

#[test]
fn the_pool_key_census_is_clean() {
    assert_eq!(census(ORCHESTRATION, POOL_LOOKUP), Vec::<String>::new());
}

#[test]
fn the_census_goes_red_on_the_old_shape() {
    let fixture = "    pub fn mark_registration_live(&mut self, address: Address) {}\n";
    let red = census(fixture, "");
    assert!(
        red.iter().any(|v| v.contains("mark_registration_live")),
        "the census must catch a restored address-keyed twin: {red:?}"
    );
    assert_eq!(
        red.iter()
            .filter(|v| v.contains("has no PoolLookup"))
            .count(),
        DOORS.len(),
        "an empty lookup source must report every door missing"
    );
}
