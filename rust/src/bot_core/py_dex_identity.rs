//! Python seam for the Rust-core [`DexIdentity`](crate::bot_core::dex_identity)
//! presets (ADR-005 slice 6).
//!
//! [`PyDexIdentity`] is a frozen read-only view over a `DexIdentity` preset.
//! The free function [`dex_identity`] resolves a preset by its kebab-case
//! variant string. Slice 7 (DEX subclass collapse) will be the first real
//! consumer; this seam exists so the standalone claim is testable from Python:
//!
//! ```python
//! from degenbot_rs import dex_identity
//! ident = dex_identity("camelot-v2-stable")
//! assert ident is not None
//! ident.factory  # 0x6EcCab422D763aC031210895C81787E87B43A652
//! ```
//!
//! Lives in this `py_dex_identity.rs` file (not `dex_identity.rs`) so the Rust
//! core stays `pyo3`-free per the ADR-005 standalone constraint. The view is a
//! COPY of the preset fields (addresses as checksummed hex strings, fees as
//! Python int tuples, init hash as a hex digest) — built once at lookup time.

use std::sync::OnceLock;

use pyo3::prelude::*;

use crate::address_utils::address_to_checksum_string;
use crate::bot_core::dex_identity::DexIdentity;

/// Frozen Python view over a `DexIdentity` preset (ADR-005 slice 6).
///
/// Read-only — all fields are `#[getter]`. Carries the preset's deployment
/// identity (factory, deployer, init hash, default fees, reserve ABI shape,
/// variant string) so a Python builder can resolve a DEX's deployment data
/// without a Python-side preset table. The values mirror the `DexIdentity`
/// core struct; only the representation differs (hex strings for addresses +
/// init hash, since `pyo3` doesn't expose `alloy::Address`/`B256` directly
/// without a converter).
#[pyclass(frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyDexIdentity {
    factory: String,
    deployer: String,
    init_hash: String,
    fee_token0: (u64, u64),
    fee_token1: (u64, u64),
    reserves_abi: Vec<String>,
    variant: String,
}

impl PyDexIdentity {
    /// Build a Python view from a core `DexIdentity` preset.
    fn from_core(ident: &DexIdentity) -> Self {
        Self {
            factory: address_to_checksum_string(&ident.factory),
            deployer: address_to_checksum_string(&ident.deployer),
            init_hash: format!("{:#x}", ident.init_hash),
            fee_token0: ident.fee_token0,
            fee_token1: ident.fee_token1,
            reserves_abi: ident.reserves_abi.as_types().iter().map(|s| (*s).to_string()).collect(),
            variant: ident.variant.as_str().to_string(),
        }
    }
}

#[pymethods]
impl PyDexIdentity {
    /// Factory contract address (EIP-55 checksummed hex).
    #[getter]
    fn factory(&self) -> &str {
        &self.factory
    }

    /// CREATE2 deployer address (EIP-55 checksummed hex).
    #[getter]
    fn deployer(&self) -> &str {
        &self.deployer
    }

    /// CREATE2 init code hash (lowercase hex with `0x` prefix, 64 hex chars).
    #[getter]
    fn init_hash(&self) -> &str {
        &self.init_hash
    }

    /// `token0→token1` fee parameters: `(gamma_numer, fee_denom)` — the
    /// retained post-fee fraction (e.g. `(997, 1000)` for a 0.3% fee). NOT the
    /// fee numerator. Slice-5 convention; matches `register_v2_pool`'s
    /// `gamma_numerN` parameter meaning.
    #[getter]
    fn fee_token0(&self) -> (u64, u64) {
        self.fee_token0
    }

    /// `token1→token0` fee parameters: `(gamma_numer, fee_denom)`.
    #[getter]
    fn fee_token1(&self) -> (u64, u64) {
        self.fee_token1
    }

    /// Solidity struct types for `Sync`-event reserve decoding
    /// (e.g. `["uint112", "uint112"]` for most V2 DEXes; the 3-tuple for
    /// `PancakeSwap`).
    #[getter]
    fn reserves_abi(&self) -> Vec<String> {
        self.reserves_abi.clone()
    }

    /// The kebab-case variant string (the lookup key).
    #[getter]
    fn variant(&self) -> &str {
        &self.variant
    }

    fn __repr__(&self) -> String {
        format!(
            "PyDexIdentity(variant={:?}, factory={})",
            self.variant, self.factory
        )
    }
}

/// Look up a `DexIdentity` preset by its kebab-case variant string
/// (e.g. `"uniswap-v2"`, `"camelot-v2-stable"`). Case-insensitive. Returns
/// `None` for an unrecognized variant so a stray string doesn't raise
/// `PanicException`.
///
/// # Examples
///
/// ```python
/// from degenbot_rs import dex_identity
/// ident = dex_identity("uniswap-v2")
/// assert ident is not None
/// assert ident.factory == "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
/// assert dex_identity("nonexistent") is None
/// ```
#[pyfunction]
fn dex_identity(variant: &str) -> Option<PyDexIdentity> {
    // Cache the eight preset views in a OnceLock so repeated lookups are O(1)
    // (the preset data is compile-time constant; only the Python-string view
    // allocation is amortized).
    static PRESETS: OnceLock<std::collections::HashMap<&'static str, PyDexIdentity>> =
        OnceLock::new();
    let presets = PRESETS.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        for v in crate::bot_core::dex_identity::DexVariant::ALL {
            let ident = crate::bot_core::dex_identity::preset_for_variant(v);
            m.insert(v.as_str(), PyDexIdentity::from_core(&ident));
        }
        m
    });
    // Case-insensitive: normalize then look up against the kebab-case keys.
    let lower = variant.to_ascii_lowercase();
    presets.get(lower.as_str()).cloned()
}

/// Register the `dex_identity` free function + the `PyDexIdentity` class on
/// the top-level `degenbot_rs` module.
pub(crate) fn add_dex_identity(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(dex_identity, m)?)?;
    m.add_class::<PyDexIdentity>()?;
    Ok(())
}
