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
//!
//! The balance-vector structural family (Curve stableswap, Balancer weighted,
//! Balancer stable) shares its family-blind event/probe vocabulary in
//! [`balance_vector`] (ADR-059 D6) — the per-family projections, swap reads,
//! and calc behavior never cross. The reserve-pair structural family (V2 +
//! Aerodrome V2 volatile) is one module carrying its own event/probe
//! vocabulary — both designs' apply bodies are the ADR-017 D3 collapse, so
//! nothing sits beside it.
pub mod balance_vector;
pub mod balancer_stable;
pub mod balancer_weighted;
pub mod concentrated_liquidity;
pub mod curve_stableswap;
pub mod reserve_pair;
