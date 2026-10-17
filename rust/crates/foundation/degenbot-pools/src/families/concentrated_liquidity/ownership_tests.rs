//! Structural gate for the concentrated-liquidity state module's ownership:
//! each shared state verb is DEFINED once, in its `state_*` module, and the
//! two design files (`v3_state.rs` / `v4_state.rs`) carry only forwarders —
//! a re-introduced verb body anywhere else is a fold violation.

/// Counts definition-shaped `fn <verb>` lines: after any visibility
/// qualifier, a line starting with `fn <verb>` whose name token (bounded by
/// `(`, `<`, `:`, or whitespace) is exactly `verb`. Doc mentions and
/// call sites never match; the needles are concat-built so this test's own
/// source never matches them either.
fn count_definitions(src: &str, verb: &str) -> usize {
    src.lines()
        .filter(|line| {
            let mut line = line.trim_start();
            loop {
                if let Some(rest) = line.strip_prefix("pub(crate) ") {
                    line = rest;
                } else if let Some(rest) = line.strip_prefix("pub ") {
                    line = rest;
                } else if let Some(rest) = line.strip_prefix("pub(super) ") {
                    line = rest;
                } else {
                    break;
                }
            }
            let Some(rest) = line.strip_prefix("fn ") else {
                return false;
            };
            let name = rest
                .split(|c: char| c == '(' || c == '<' || c == ':' || c.is_whitespace())
                .next()
                .unwrap_or("");
            name == verb
        })
        .count()
}

#[test]
fn the_cl_state_module_owns_each_shared_state_verb_and_the_design_files_only_forward() {
    let scanned: [(&str, &str); 5] = [
        ("state_journal.rs", include_str!("state_journal.rs")),
        ("state_seeding.rs", include_str!("state_seeding.rs")),
        ("state_walk.rs", include_str!("state_walk.rs")),
        ("v3_state.rs", include_str!("../../v3_state.rs")),
        ("v4_state.rs", include_str!("../../v4_state.rs")),
    ];
    // (owning module, verb needle, forwarder definitions expected in
    // v3_state, in v4_state). Needles concat-built so this file's own
    // source never contains them contiguously.
    #[rustfmt::skip]
    let verbs: [(&str, &str, usize, usize); 16] = [
        ("state_journal.rs", concat!("restore_", "before_block"), 1, 1),
        ("state_journal.rs", concat!("discard_", "before_block"), 1, 1),
        ("state_journal.rs", concat!("journal_", "len"), 1, 1),
        ("state_journal.rs", concat!("newest_", "block"), 1, 1),
        ("state_seeding.rs", concat!("word_", "of"), 1, 1),
        ("state_seeding.rs", concat!("seed_known_bitmap_", "words"), 1, 1),
        ("state_seeding.rs", concat!("seed_", "genesis"), 1, 1),
        ("state_seeding.rs", concat!("registration_lifecycle_", "for"), 0, 0),
        ("state_seeding.rs", concat!("snapshot_seed_", "for"), 0, 0),
        ("state_walk.rs", concat!("swap_is_", "viable"), 1, 1),
        ("state_walk.rs", concat!("advance_update_", "block"), 1, 1),
        ("state_walk.rs", concat!("advance_tick_data_", "block"), 1, 1),
        ("state_walk.rs", concat!("invalidate_tick_range_", "cache"), 1, 1),
        ("state_walk.rs", concat!("get_cached_tick_", "ranges"), 0, 0),
        ("state_walk.rs", concat!("build_int_v3_", "sequence"), 1, 0),
        ("state_walk.rs", concat!("build_int_v4_", "sequence"), 0, 1),
    ];
    for (home, verb, want_v3, want_v4) in verbs {
        for (file, src) in scanned {
            let want = if file == home {
                1
            } else if file == "v3_state.rs" {
                want_v3
            } else if file == "v4_state.rs" {
                want_v4
            } else {
                0
            };
            let defs = count_definitions(src, verb);
            assert_eq!(
                defs, want,
                "{file} defines {verb} {defs} time(s), expected {want} — the ONE {verb} body lives in {home}; the design files carry only forwarders"
            );
        }
    }
}
