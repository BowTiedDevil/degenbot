//! The pool-state capability: per-family value structs and storage slots, the
//! tick map/bitmap machinery, spec-bound validators, the reorg-journal data,
//! and the shared family-capability declarations (ADR-084 W5b).

pub mod aerodrome_v2_state;

pub mod balancer_stable_state;

pub mod balancer_weighted_state;

pub mod capability;

pub mod curve_state;

#[expect(clippy::doc_markdown)]
pub mod int_v3_hop;

pub mod slot_layout;

pub mod spec_bounds;

pub mod state_history;

pub mod tick_bitmap;

pub mod tick_map;

pub mod tick_map_verify;

pub mod v2_state;

pub mod v3_pancakeswap_storage_slots;

pub mod v3_state;

pub mod v3_storage_slots;

pub mod v4_state;

pub mod v4_storage_slots;
