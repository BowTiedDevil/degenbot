//! Shared support for the fixture-tools development package.
//!
//! This package owns the record / check / regenerate workflows for the committed
//! fixtures, plus the test-input generators. Two rules keep it a tool rather
//! than a product crate:
//!
//! - **Nothing a product build needs may live here.** No product crate depends
//!   on a tool package, and no `cargo test` lane may require one of these
//!   programs to run: a durable assertion belongs in the owning crate's tests.
//! - **A heavy lane is a feature, not a default.** The SQL-ledger forward is
//!   opt-in, and the programs that need it declare their own
//!   `required-features`, so a default `cargo check` skips them visibly in the
//!   manifest instead of failing on a `cfg`'d-out module.
//!
//! The library target is the shared support the programs build on: fixture path
//! resolution (an explicit path argument, never an inferred one, so a moved
//! program cannot silently read or write a different directory), the
//! record-versus-`--check` driver, and the canonical writers.
//!
//! **Isolation, stated precisely.** This package's `sql-ledger` forward is the only place the
//! statement-ledger *feature* is turned on, but the lane's resolved tree is not this package
//! alone: it reaches the `degenbot` facade, and the facade's Linux target-scoped
//! `degenbot-simulation/evm2-jit` dependency puts the vendored JIT family in the tree (20
//! `evm2-jit` lines, with no fixture-tools feature in the chain). That is the product graph
//! the facade already had, inherited rather than forwarded — closing it would take a facade
//! feature or dependency redesign. `cargo tree` here proves which forwards are active; it
//! does not prove transitive absence.
//!
//! Lane feature: `sql-ledger`.
