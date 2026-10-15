//! The family read module trees (ADR-073 D1) — the foundation-layer homes of
//! everything one hop family contributes to being READ and RESOLVED.
//!
//! One tree per family under [`families`]; each tree is a GLOSSARY.md
//! "Family read module": it reads no state owner and consumes no solver
//! kernel, so the family's read story is one file named for the family. The
//! solve-side half of each family stays in `degenbot-solvers` (GLOSSARY.md
//! "Family module").
//!
//! The central dispatchers keep one dispatch row per family (ADR-073 D2) and
//! call ONLY the family tree's intent doors.
pub mod concentrated_liquidity;
