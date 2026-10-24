//! Aave V3 lending-market DB writers — the per-event apply fns +
//! `get_or_create_*` upsert substrate.
//!
//! Port of `src/degenbot/cli/aave/event_handlers.py::_process_*` handlers +
//! `db_market.py` / `db_assets.py` / `db_users.py` / `db_positions.py`
//! `get_or_create_*` upsert helpers. The Python `aave_update` driver loop +
//! RPC event fetch stay as the shell (`stays-python`); the Rust core owns the
//! pure-typed upsert substrate: get-or-create + event-decode→row-write.
//!
//! # The write-capable connection (`open_for_writes`)
//!
//! binding #2 hard-AC is "every **read** connection opened by
//! degenbot-db MUST set `query_only=on`" — [`DegenbotDb::open`] /
//! [`DegenbotDb::open_in_memory`] stay read-only. Writers use
//! [`DegenbotDb::open_for_writes`] / [`DegenbotDb::open_in_memory_for_writes`]:
//! the same concurrency PRAGMAs ([`crate::pragma::apply_open_pragmas`]) +
//! [`ensure_schema`][crate::migrate::ensure_schema]
//! sequence, but `query_only` is NEVER set — the connection is write-capable.
//! The writer methods on [`DegenbotDb`] (`get_or_create_*` / `process_*`)
//! execute `INSERT` / `UPDATE` directly on the locked connection; called on a
//! read-only handle they fail at the `SQLite` layer
//! (`attempt to write a readonly database`), surfacing [`DbError::Sqlite`].
//!
//! # The chunk-atomicity seam (one connection per write arm)
//!
//! Every write arm (`get_or_create_*` / `apply_*`) carries its SQL in exactly
//! one body: the `_on_conn` form, which takes a borrowed
//! [`rusqlite::Connection`]. A chunk loop owns one connection inside one
//! `Transaction` (a `Transaction` derefs to `Connection`) and calls the
//! `_on_conn` forms on it, so a chunk's Aave writes and its pool writes (the
//! end-of-chunk `last_update_block` stamp) commit or roll back atomically —
//! on rollback no chunk write advances, and a restart re-processes the chunk
//! clean. The chunk loop calls `_on_conn` on its owned connection rather than
//! re-locking: the connection `Mutex` is non-reentrant, so a re-lock from the
//! chunk task would deadlock.
//!
//! The `&self` forms serve one-shot callers: each locks the shared connection
//! via [`DegenbotDb::lock`] for the call and delegates to the `_on_conn`
//! body. No write arm opens a connection or manages transaction scope on its
//! own.
//!
//! Both forms of an operation are generated together by the `conn_writer!`
//! macro below, from one parameter list: the `_on_conn` body is written once
//! and the `&self` wrapper is derived from it, so the two surfaces cannot
//! drift apart. Only the `_on_conn` form carries the operation's semantics +
//! `# Errors` doc; the wrapper's doc points at it.
//!
//! # The bit-decode (the pure CPU seam)
//!
//! [`decode_reserve_configuration_bitmap`] ports
//! `_decode_reserve_configuration_bitmap` (`event_handlers.py` L133–L214) verbatim:
//! the Aave V3 reserve-config `uint256` bitmap bits → a typed
//! [`ReserveConfiguration`] (ltv / liquidation-threshold / -bonus / decimals /
//! active / frozen / borrowing-enabled / stable-rate / reserve-factor /
//! borrow-cap / supply-cap / debt-ceiling / liquidation-protocol-fee /
//! unbacked-mint-cap / e-mode-category-id / flash-loan / isolation-mode /
//! borrowable-in-isolation). Pure CPU, no I/O — the parity-gate pin ports the
//! exact bit masks + shifts.
//!
//! # The upsert substrate (`get_or_create_*`)
//!
//! Each `get_or_create_*` mirrors the Python `session.scalar(select(...)) →
//! mutate ORM → session.add()` trajectory as a single `SELECT … WHERE …` →
//! `INSERT` (or `UPDATE` for the mutate path). The Python `Erc20TokenTable` /
//! `AaveV3Asset` / `AaveV3User` / `AaveV3EModeCategory` / `AaveV3AssetConfig` /
//! `AaveV3UserCollateralConfig` / `AaveV3CollateralPosition` /
//! `AaveV3DebtPosition` row-create defaults are reproduced verbatim (field
//! defaults match the `SQLAlchemy` model column defaults so byte-identical DB
//! state vs the Python ORM trajectory holds — the parity gate's
//! acceptance criterion).
//!
//! # RPC-coupling carve-out (`stays-python` inside the upserts)
//!
//! The Python `get_or_create_erc20_token` + `get_or_create_user` fetch
//! on-chain metadata (name/symbol/decimals) / GHO discount via `raw_call`.
//! The Rust core fns take these as caller-supplied `Option` params (the
//! Python driver computes them + passes them in); no `RPC` in the core
//! (ADR-005 standalone — `degenbot-db` has no `degenbot-rpc` dependency).

/// Define one write operation's two call surfaces from a single parameter
/// list.
///
/// Every write arm has exactly one body, in its `_on_conn` form: it takes a
/// borrowed [`rusqlite::Connection`] so a chunk loop can call it on the
/// connection its `Transaction` owns (the connection `Mutex` is
/// non-reentrant, so a chunk task that re-locked would deadlock). The `&self`
/// form is the one-shot caller's surface: it locks the shared connection for
/// the call and delegates. This macro is the only place an operation's
/// parameter list is written down.
///
/// The generated `_on_conn` variant carries the operation's semantics +
/// `# Errors` doc; the generated `&self` wrapper's doc points at it instead of
/// repeating it. The parameter list starts with the connection parameter
/// (`macro_rules` binds locals hygienically, so a connection binding the macro
/// introduced would be invisible to the invocation's body — the body's
/// `conn` must name a parameter the invocation wrote down).
///
/// An argument list that trips `clippy::too_many_arguments` trips it on both
/// generated fns, so such an operation adds `;` after the return type and puts
/// the wrapper-only attributes after it (the leading attribute group lands on
/// the `_on_conn` fn only).
macro_rules! conn_writer {
    (
        $(#[$conn_meta:meta])*
        fn $wrapper:ident, $on_conn:ident (
            $conn_arg:ident : $conn_ty:ty,
            $($arg:ident : $ty:ty),* $(,)?
        ) -> $ret:ty
        ; $(#[$wrapper_meta:meta])*
        $body:block
    ) => {
        conn_writer!(@emit
            ( $(#[$conn_meta])* )
            ( $(#[$wrapper_meta])* )
            ($wrapper, $on_conn) [$conn_arg : $conn_ty] [$($arg : $ty),*] -> $ret
            $body
        );
    };
    (
        $(#[$conn_meta:meta])*
        fn $wrapper:ident, $on_conn:ident (
            $conn_arg:ident : $conn_ty:ty,
            $($arg:ident : $ty:ty),* $(,)?
        ) -> $ret:ty
        $body:block
    ) => {
        conn_writer!(@emit
            ( $(#[$conn_meta])* )
            ()
            ($wrapper, $on_conn) [$conn_arg : $conn_ty] [$($arg : $ty),*] -> $ret
            $body
        );
    };
    (@emit
        ( $(#[$conn_meta:meta])* )
        ( $(#[$wrapper_meta:meta])* )
        ($wrapper:ident, $on_conn:ident) [$conn_arg:ident : $conn_ty:ty] [$($arg:ident : $ty:ty),*]
        -> $ret:ty
        $body:block
    ) => {
        $(#[$conn_meta])*
        pub fn $on_conn(
            $conn_arg : $conn_ty,
            $($arg : $ty),*
        ) -> $ret
        $body

        #[doc = concat!(
            "Locks the shared connection for the call and delegates to ",
            "[`Self::", stringify!($on_conn), "`], which owns this operation's ",
            "body, semantics, and error contract."
        )]
        ///
        /// # Errors
        ///
        #[doc = concat!("See [`Self::", stringify!($on_conn), "`].")]
        $(#[$wrapper_meta])*
        pub fn $wrapper(
            &self,
            $($arg : $ty),*
        ) -> $ret {
            let conn = self.lock();
            Self::$on_conn(&conn, $($arg),*)
        }
    };
}

mod aave;
mod erc20;
mod gho;
mod pools;
mod positions;
mod purge;
mod util;

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod purge_tests;
#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests;

pub use aave::*;
pub use positions::*;
pub use purge::*;

pub(crate) use alloy::primitives::U256;
pub(crate) use rusqlite::{params, OptionalExtension};

pub(crate) use crate::connection::DegenbotDb;
pub(crate) use crate::error::DbError;
