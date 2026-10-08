// The build receipt's counter-advance decision, split from `build_scan.rs` so
// the console's build script can include the scanner WITHOUT it: the console
// must never write the receipt (a plain `cargo build -p degenbot-cli` must not
// bless the installed extension as fresh), and an unused-but-included function
// would trip dead_code in that build-script binary. Included by the receipt
// writer's build.rs and by tests/build_scan.rs, which pins the invariant:
// advance exactly one count when the scanned fingerprint is new or
// unknowable, keep the stored count otherwise.

/// The next receipt count for the stored `<count> <fingerprint>` state under
/// the freshly scanned `fingerprint`. Unknown state must advance: no receipt
/// yet (`None` stored), a legacy receipt without a stored fingerprint, or a
/// failed scan (`None` fingerprint) all leave a build unable to prove it is
/// byte-identical to its predecessor, and an unprovable build must not bless
/// an installed artifact as fresh.
#[must_use]
pub fn advance_count(stored: Option<(u64, Option<u64>)>, fingerprint: Option<u64>) -> u64 {
    let changed = stored.is_none()
        || fingerprint.is_none()
        || stored.as_ref().and_then(|(_, fp)| *fp) != fingerprint;
    let prior_count = stored.map_or(0, |(count, _)| count);
    if changed {
        prior_count.saturating_add(1)
    } else {
        prior_count
    }
}
