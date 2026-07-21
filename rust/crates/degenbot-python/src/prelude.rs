//! Binding-crate prelude.
//!
//! Mirrors `polars-python/src/prelude.rs`: a small, curated re-export surface so
//! wrapper modules open with `use crate::prelude::*;` instead of repeating the same
//! ~10–15 lines of imports at the top of every file.
//!
//! What's here:
//! - [`pyo3::prelude`] — the FFI prelude (`Python`, `Bound`, `PyResult`, `PyAny`,
//!   `IntoPyObjectExt`, `wrap_pyfunction`, …); the single most-repeated wrapper
//!   import (appears in 10 of the 15 wrapper files).
//! - The [`conversion`] module surface: `alloy as alloy_py` (the alias name is kept
//!   to avoid shadowing the external `alloy` crate — `alloy::hex` /
//!   `alloy::primitives` must keep resolving), plus `cache`, `json`, `rpc_types` as
//!   bare module namespaces so `crate::conversion::cache::create_hexbytes` can be
//!   written `cache::create_hexbytes` via the glob.
//! - The most-used crate-root re-exports of `degenbot_core` modules:
//!   [`address_utils`], [`errors`], [`runtime`].
//!
//! What is deliberately NOT here:
//! - `std::sync::Arc`, `std::collections::HashMap`, etc. — std items stay local
//!   (matches Polars: a prelude re-exports the FFI + domain prelude, not std).
//! - `pyo3::exceptions::*` / specific `pyo3::types::*` items — specific items stay
//!   local; only the broad `pyo3::prelude` glob is curated here.
//! - Domain wrapper types (`PyBot`, `PyAlloyProvider`, …) and external `alloy`
//!   items — file-specific, imported where used.
//!
//! (ergo UG6FKN task ZNCAWD.)

pub use crate::conversion::{alloy as alloy_py, cache, json, rpc_types};
pub use crate::{address_utils, errors, runtime};
pub use pyo3::prelude::*;
