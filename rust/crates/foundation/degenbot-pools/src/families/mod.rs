//! The family read module trees (ADR-073 D1) — the foundation-layer homes of
//! everything one hop family contributes to being READ and RESOLVED.
//!
//! One tree per family under [`families`]; each tree is a GLOSSARY.md
//! "Family read module": it reads no state owner and consumes no solver
//! kernel, so the family's read story is one file named for the family.
//! Since the ADR-073 endgame re-home each family file ALSO carries the
//! family's solve entry (its nested `solve` module; the concentrated liquidity tree's
//! [`concentrated_liquidity::solve`]) — the two-homes pattern is literally
//! ONE file per family.
//!
//! The central dispatchers keep one dispatch row per family (ADR-073 D2) and
//! call ONLY the family tree's intent doors.
//!
//! The doors themselves share ONE family-blind contract vocabulary in
//! [`door`] — one swap-read request, one read outcome, one observe probe
//! pair, one apply contract — with each family's design bits riding as
//! additive variants. The reserve-pair structural family (V2 + Aerodrome V2
//! volatile) and the Solidly structural family (Aerodrome V2 stable + Camelot
//! `stable_swap`) share the state slices and the Sync shape, so one
//! [`door::SyncEvent`] and one [`door::FamilyApply`] impl serve both.
pub mod balance_vector;

pub mod buffered_event;

pub mod liquidity_event;

pub mod liquidity_event_buffer;

pub mod balancer_stable;
pub mod balancer_weighted;
pub mod concentrated_liquidity;
pub mod curve_stableswap;
pub mod door;
pub mod reserve_pair;
pub mod solidly;
pub mod state_update;
