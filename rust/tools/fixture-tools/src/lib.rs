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
//! resolution ([`paths`] — the repository root is resolved ONCE from a marker the
//! tree actually carries, and every fixture home is derived from that answer,
//! because a manifest-relative `join("../../../..")` silently names a different
//! directory for a package that moved from `rust/crates/<role>/<crate>` to
//! `rust/tools/<package>`), plus the record-versus-`--check` drift comparison both
//! generators print through the same function so the two gates cannot drift apart.
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
//! Lane feature: `sql-ledger`. The four programs this package currently carries — the two
//! offline capture generators (`generate_evm_oracle_captures`,
//! `generate_aave_adversarial_captures`) and the two live span recorders
//! (`generate_aave_upgrade_windows`, `generate_aave_config_windows`) — build only under it,
//! each declaring its own `[[bin]] required-features`, which is why the lane, not the
//! package, is what `just check-dev-tools fixture-sql-ledger` checks. That is a statement
//! about today's four binaries, not about the package: the fixture migrations still to come
//! add programs with their own feature requirements, and this sentence must be re-read then
//! rather than inherited.

pub mod paths;

#[cfg(feature = "sql-ledger")]
pub mod ledger;
