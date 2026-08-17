//! T1 (arch-review epic PZBGP7) module-boundary probe for the walker
//! structural decomposition.
//!
//! `derive_plan` must be a thin dispatcher over six shape modules; the
//! shape bodies live in `src/grammar_walker/shapes/*.rs`, one module per
//! enclosure block. RED before the split, GREEN after. Unlike the D6
//! tripwires (deleted by T2 — they measured a completed migration),
//! this is a hygiene pin for the persistent structure and stays.

#![expect(clippy::expect_used)]

use std::path::Path;

/// The six shape modules behind the dispatcher (one per enclosure block).
const SHAPE_MODULES: [&str; 6] = [
    "all_v2_chain",
    "two_hop_seed_v4",
    "two_hop_v4_led",
    "three_hop",
    "two_hop_uniswap_only",
    "tag_residual",
];

/// Maximum tolerable span (in lines) of `derive_plan` in `grammar_walker.rs`.
/// A dispatcher of six `if <gate> { return shapes::… }` arms plus the
/// documentation header fits well under this; the 3,848-line fused body
/// does not.
const MAX_DERIVE_PLAN_LINES: usize = 80;

#[test]
fn walker_shape_modules_exist() {
    for m in SHAPE_MODULES {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/grammar_walker/shapes")
            .join(format!("{m}.rs"));
        assert!(
            p.exists(),
            "missing walker shape module {} — T1 structural split wants \
             one module per enclosure block (arch-review epic PZBGP7)",
            p.display()
        );
    }
}

#[test]
fn derive_plan_is_a_thin_dispatcher() {
    let src = include_str!("../src/grammar_walker.rs");
    let start = src
        .find("pub(crate) fn derive_plan(")
        .expect("derive_plan exists");
    let body = &src[start..];
    // The function ends at the first column-0 closing brace.
    let end_off = body
        .split_inclusive('\n')
        .position_of_col0_close()
        .expect("derive_plan body closes at col 0");
    let span = body.split_inclusive('\n').take(end_off).count();
    assert!(
        span <= MAX_DERIVE_PLAN_LINES,
        "derive_plan spans {span} lines (max {MAX_DERIVE_PLAN_LINES}) — the \
         six enclosure blocks must live in walker/shapes/, one module each, \
         with derive_plan as a (len, repay-sequence) dispatcher only"
    );
}

trait Col0Close {
    fn position_of_col0_close(&mut self) -> Option<usize>;
}
impl<'a, I: Iterator<Item = &'a str>> Col0Close for I {
    fn position_of_col0_close(&mut self) -> Option<usize> {
        let mut i = 0;
        for line in self.by_ref() {
            i += 1;
            if line == "}\n" || line == "}" {
                return Some(i);
            }
        }
        None
    }
}
