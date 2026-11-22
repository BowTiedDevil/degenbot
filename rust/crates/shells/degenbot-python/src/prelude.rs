//! Binding-crate prelude.
//!
//! Mirrors `polars-python/src/prelude.rs`: a small, curated re-export surface so
//! wrapper modules open with `use crate::prelude::*;` instead of repeating the same
//! ~10–15 lines of imports at the top of every file.
//!
//! Carries the FFI prelude ([`pyo3::prelude`] — the single most-repeated
//! wrapper import), the [`conversion`] module surface (`alloy as alloy_py` —
//! the alias name is kept to avoid shadowing the external `alloy` crate,
//! whose `alloy::hex` / `alloy::primitives` paths must keep resolving — plus
//! `cache`, `json`, `rpc_types` as bare module namespaces), and the most-used
//! crate-root re-exports of `degenbot_core` ([`address_utils`], [`errors`],
//! [`runtime`]).
//!
//! Deliberately NOT here: std items (`std::sync::Arc`, `std::collections::HashMap`,
//! … stay local — a prelude re-exports the FFI + domain prelude, not std),
//! specific `pyo3::exceptions` / `pyo3::types` items (only the broad
//! `pyo3::prelude` glob is curated), and domain wrapper types (`PyBot`,
//! `PyAlloyProvider`, …) plus external `alloy` items (file-specific, imported
//! where used).

pub use crate::conversion::{alloy as alloy_py, cache, json, rpc_types};
pub use crate::{address_utils, errors, runtime};
pub use pyo3::prelude::*;
