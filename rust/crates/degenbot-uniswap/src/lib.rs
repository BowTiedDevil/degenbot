//! Uniswap-protocol domain crate — DEX identity presets + V2 swap callldata
//! encoding.
//!
//! This crate holds pure-Rust value objects for the Uniswap V2-style protocol
//! family: a `DexIdentity` (factory, deployer, CREATE2 init hash, default fee
//! params, ABI struct shape, `DexVariant` tag) with `pub const` presets per
//! supported DEX+variant, plus the V2 swap-call encoder (`encode_v2_swap`,
//! `EncodedCall`, `V2_SWAP_SELECTOR`).
//!
//! It depends only on `alloy::primitives`, `degenbot-abi` (the encoder +
//! `AbiValue` for `encode_v2_swap`), and `degenbot-core` (for
//! `AbiDecodeError`). It has **no `pyo3`, no `tokio`, no `degenbot-rpc`,
//! no `degenbot-bot`** — a standalone Rust consumer can look up a Sushiswap
//! V2 preset and encode a V2 swap call without pulling the engine/pump/RPC
//! stack (ADR-005 "standalone constraint").
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
//! - [`v2_encoding`] — V2 `swap(uint256,uint256,address,bytes)` callldata
//!   encoding (`EncodedCall`, `V2_SWAP_SELECTOR`, `encode_v2_swap`).

pub mod dex_identity;
pub mod v2_encoding;
