//! Uniswap-protocol domain crate — DEX identity presets + V2 swap callldata
//! encoding.
//!
//! This crate holds pure-Rust value objects for the Uniswap V2-style protocol
//! family: a `DexIdentity` (factory, deployer, CREATE2 init hash, default fee
//! params, ABI struct shape, `DexVariant` tag) with `pub const` presets per
//! supported DEX+variant, plus the V2 swap-call encoder (`encode_v2_swap`,
//! `EncodedCall`, `V2_SWAP_SELECTOR`).
//!
//! It depends on `alloy::primitives`, `degenbot-abi` (the encoder +
//! `AbiValue` for `encode_v2_swap`), `degenbot-core` (for `AbiDecodeError`),
//! and `degenbot-db` (for the ADR-059 D3 species manifest). It has **no
//! `pyo3`, no `tokio`, no `degenbot-rpc`, no `degenbot-bot`** — a standalone
//! Rust consumer can look up a Sushiswap V2 preset and encode a V2 swap call
//! without pulling the engine/pump/RPC stack (ADR-005 "standalone
//! constraint").
//!
//! # Why not `degenbot-core`?
//!
//! DEX identity presets (factory/init-hash/fees) are Uniswap-V2-domain data,
//! not foundational utilities. `degenbot-core` stays leaf-focused (errors,
//! hex, addresses, runtime); this crate owns the protocol-domain value
//! objects. Plan 103's target diagram originally placed `dex_identity` in
//! `degenbot-core`; this crate lands it in a more honest home.
//!
//! # Modules
//!
//! - [`dex_identity`] — `DexIdentity` / `DexVariant` / `ReservesAbi` value
//!   objects + `pub const` per-DEX presets.
//! - [`deployments`] — `(chain, factory)`-keyed CREATE2 identity lookup over
//!   the embedded canonical `deployments.json` (init hash + deployer).
//! - [`manager_deployments`] — `(chain, manager)`-keyed V4 species lookup over
//!   the embedded species manifest (V4 has no CREATE2 factory).
//! - [`v2_encoding`] — V2 `swap(uint256,uint256,address,bytes)` callldata
//!   encoding (`EncodedCall`, `V2_SWAP_SELECTOR`, `encode_v2_swap`).
//! - [`create2`] — CREATE2 pool-address derivation (pure-Rust mirror of the
//!   Python `generate_v2/v3_pool_address`).

pub mod create2;
pub mod deployments;
pub mod dex_identity;
pub mod manager_deployments;
pub mod v2_encoding;
