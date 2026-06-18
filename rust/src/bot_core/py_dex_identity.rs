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

use std::str::FromStr;
use std::sync::OnceLock;

use pyo3::prelude::*;

use crate::address_utils::{address_to_checksum_string, parse_address};
use crate::bot_core::dex_identity::{DexIdentity, DexVariant, ReservesAbi};

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

    /// Construct a custom `DexIdentity` view from explicit Python args.
    ///
    /// Intended for tests + ad-hoc identities (production should resolve
    /// presets via `dex_identity` or the `pool_type_registry`). Mirrors Polars'
    /// builder pattern for codec options — presets are the canonical source,
    /// but custom identities are constructable when needed.
    fn build(
        factory: &str,
        init_hash: &str,
        fee_token0: (u64, u64),
        fee_token1: (u64, u64),
        variant: &str,
        reserves_abi: Option<Vec<String>>,
    ) -> PyResult<Self> {
        let factory_addr = parse_address(factory)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let init_hash_b256 = alloy::primitives::B256::from_str(init_hash)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let variant_enum = DexVariant::from_kebab(variant).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!("unknown variant: {variant}"))
        })?;
        // `match` consumes `reserves_abi` (moves the Option); the inner Vec is
        // scoped to the arm that resolves `ReservesAbi` without escaping.
        let reserves_abi_enum = match reserves_abi {
            None => ReservesAbi::Standard,
            Some(v) => match v.as_slice() {
                [a, b] if [a.as_str(), b.as_str()] == ["uint112", "uint112"] => {
                    ReservesAbi::Standard
                }
                [a, b, c] if [a.as_str(), b.as_str(), c.as_str()]
                    == ["uint112", "uint112", "uint32"] =>
                {
                    ReservesAbi::PancakeswapStyle
                }
                other => {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "unknown reserves_abi shape: {other:?}"
                    )));
                }
            },
        };
        let ident = DexIdentity {
            factory: factory_addr,
            deployer: factory_addr,
            init_hash: init_hash_b256,
            fee_token0,
            fee_token1,
            reserves_abi: reserves_abi_enum,
            variant: variant_enum,
        };
        Ok(Self::from_core(&ident))
    }
}

#[pymethods]
impl PyDexIdentity {
    /// Construct a custom `DexIdentity` view from explicit Python args.
    ///
    /// Mirrors Polars' builder pattern for codec options — presets are the
    /// canonical source (resolve via `dex_identity`), but custom identities
    /// are constructable for tests / ad-hoc deployments.
    ///
    /// # Examples
    ///
    /// ```python
    /// from degenbot_rs import PyDexIdentity
    /// ident = PyDexIdentity(
    ///     factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
    ///     init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
    ///     fee_token0=(997, 1000),
    ///     fee_token1=(997, 1000),
    ///     variant="uniswap-v2",
    /// )
    /// ```
    #[new]
    #[pyo3(signature = (factory, init_hash, fee_token0, fee_token1, variant, reserves_abi=None))]
    #[allow(clippy::needless_pass_by_value)] // pyo3 extracts Python list → Option<Vec<String>>
    fn new(
        factory: &str,
        init_hash: &str,
        fee_token0: (u64, u64),
        fee_token1: (u64, u64),
        variant: &str,
        reserves_abi: Option<Vec<String>>,
    ) -> PyResult<Self> {
        Self::build(factory, init_hash, fee_token0, fee_token1, variant, reserves_abi)
    }

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
