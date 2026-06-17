//! Typed boundary for the {Slot0 Head / Tick Bookkeeping Map} split (ADR-004).
//!
//! The [`TickMap`] / [`TickMapMut`] traits expose **only** the tick bookkeeping
//! map side of a CL pool's state — the immutable pool identification
//! (`address` / `tick_spacing`), the read-only `active_tick` slot0 scalar
//! (needed for the ±2-word bitmap scan during verification), and the
//! `tick_data` map itself. The slot0 head's mutable scalars
//! (`sqrt_price_x96`, `liquidity`) are deliberately out of reach: callers
//! that take `&impl TickMap` (the [`verify_v3_pool`](crate::bot_core::liquidity_verifier::verify_v3_pool)
//! / [`verify_v4_pool`](crate::bot_core::liquidity_verifier::verify_v4_pool)
//! verifier entry points, and the `apply_v3_liquidity_update` /
//! `apply_v4_liquidity_update` apply entry points) cannot accidentally read
//! or mutate them — the rule is carried by the type, not a module doc comment.
//!
//! The Rust analogue of Python's `_HasTickData` duck-type (see
//! `src/degenbot/uniswap/concentrated/liquidity_map.py`). Per ADR-004, the
//! state structs themselves stay flat (the full-split candidate α was
//! rejected — zero `slot0-only` consumers exist); only the verifier/apply
//! **views** are typed-narrowed.
//!
//! See ADR-004 (`docs/adr/ADR-004-cl-tickmap-typed-boundary.md`) and the
//! {Slot0 Head / Tick Bookkeeping Map} term in `rust/CONTEXT.md`.

use std::collections::HashMap;

use alloy::primitives::Address;

use crate::bot_core::TickInfo;
use crate::bot_core::{V3PoolState, V4PoolState};

// ---------------------------------------------------------------------------
// The traits
// ---------------------------------------------------------------------------

/// Read-only view of a CL pool's tick bookkeeping map + the immutable
/// identification needed to verify/restore it on-chain.
///
/// Consumers take `&impl TickMap` to access only the tick map; the slot0 head
/// scalars (`sqrt_price_x96`, `liquidity`) are deliberately out of reach —
/// verify/apply paths must not touch them. See ADR-004.
pub trait TickMap {
    /// Pool contract address (V3) or `pool_manager` (V4) — the on-chain RPC
    /// target for V3 verification. (V4 verification uses `state_view` instead,
    /// so this method exists on the V4 impl only for trait conformance.)
    fn address(&self) -> Address;

    /// Tick spacing — used for bitmap word compression during verification.
    fn tick_spacing(&self) -> i32;

    /// The current active tick — a slot0 scalar, but read-only on this trait.
    /// Used only to seed the ±2 bitmap-word scan around the current tick
    /// during verification; NOT verified (would always be stale by the time
    /// the RPC round-trips).
    fn active_tick(&self) -> i32;

    /// The tick bookkeeping map: tick index → (`liquidity_gross`,
    /// `liquidity_net`). The actual verification content.
    fn tick_data(&self) -> &HashMap<i32, TickInfo>;
}

/// Mutable view of a CL pool's tick bookkeeping map. Extends [`TickMap`] with
/// `tick_data_mut` for the apply path (Mint/Burn V3, `ModifyLiquidity` V4)
/// which mutates only the tick map. The slot0 head scalars remain out of
/// reach under this trait — see ADR-004.
pub trait TickMapMut: TickMap {
    /// Mutable access to the tick bookkeeping map.
    fn tick_data_mut(&mut self) -> &mut HashMap<i32, TickInfo>;
}

// ---------------------------------------------------------------------------
// Concrete impls for the two CL pool families (V4 parity — ADR-004)
// ---------------------------------------------------------------------------

impl TickMap for V3PoolState {
    fn address(&self) -> Address {
        self.address
    }

    fn tick_spacing(&self) -> i32 {
        self.tick_spacing
    }

    fn active_tick(&self) -> i32 {
        self.tick
    }

    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }
}

impl TickMapMut for V3PoolState {
    fn tick_data_mut(&mut self) -> &mut HashMap<i32, TickInfo> {
        &mut self.tick_data
    }
}

impl TickMap for V4PoolState {
    fn address(&self) -> Address {
        // V4 verification uses `state_view` (a separate contract) for RPC calls,
        // not `pool_manager`; this method exists only for trait conformance on
        // the V4 impl. See ADR-004.
        self.pool_manager
    }

    fn tick_spacing(&self) -> i32 {
        self.pool_key.tick_spacing
    }

    fn active_tick(&self) -> i32 {
        self.tick
    }

    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }
}

impl TickMapMut for V4PoolState {
    fn tick_data_mut(&mut self) -> &mut HashMap<i32, TickInfo> {
        &mut self.tick_data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion that both CL pool families implement `TickMap`.
    /// If a future change to `V3PoolState` / `V4PoolState` drops a field that
    /// the impl reads, this test fails to compile.
    #[test]
    fn v3_and_v4_pool_state_impl_tickmap() {
        // Declare all helpers BEFORE any statement so clippy's
        // `items_after_statements` doesn't fire — items exist from the start
        // of the scope anyway, so declaring them up top is clearer.
        fn assert_impls_tickmap<T: TickMap>() {}
        fn assert_impls_tickmap_mut<T: TickMapMut>() {}
        assert_impls_tickmap::<V3PoolState>();
        assert_impls_tickmap::<V4PoolState>();
        assert_impls_tickmap_mut::<V3PoolState>();
        assert_impls_tickmap_mut::<V4PoolState>();
    }
}
