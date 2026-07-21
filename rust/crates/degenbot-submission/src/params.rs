//! `TxParams` — the EIP-1559 transaction field set the caller assembles, the
//! signer finalizes fees onto, and [`crate::signer::TxSigner`] signs.
//!
//! `chain_id` is held ONCE on [`crate::signer::TxSigner`] (the signer owns the
//! replay-protection context — the Python oracle's `tx_params["chainId"]` is
//! the signer's chain, not an independent per-tx field). This avoids a
//! duplicated-state smell: a `TxSigner` constructed for chain *C* always
//! produces type-2 txs replay-protected for *C*, regardless of what a caller
//! passes.

use alloy::eips::eip2930::AccessList;
use alloy::primitives::{Address, Bytes, U256};

/// The EIP-1559 transaction field set (minus `chain_id`, which the
/// [`TxSigner`](crate::signer::TxSigner) owns).
///
/// Mirrors the fields the Python oracle sets on `tx_params` in
/// `examples/eth_backrun_v2_v3_v4_rust.py` L2620–L2626 (`to`/`data`/`gas`/
/// `nonce`/`value=0`/`accessList` + the two fee fields finalized by
/// [`finalize_fees`](crate::fee::finalize_fees)). `data` is the `execute()`
/// calldata produced by the executor (the `degenbot-executor` / `degenbot-abi`
/// EIP-2718 typing — this crate consumes already-encoded `Bytes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxParams {
    /// Recipient contract address (the executor). `to` in `tx_params`.
    pub to: Address,
    /// The `execute()` calldata (selector + ABI-encoded swap path) ready for
    /// on-chain submission. `data` in `tx_params`.
    pub data: Bytes,
    /// Gas limit (units). `gas` in `tx_params`.
    pub gas_limit: u64,
    /// Sender account nonce. `nonce` in `tx_params` (the dispatcher's claimed
    /// nonce — [`TxSigner`](crate::signer::TxSigner) does NOT claim it; see
    /// the submit-orchestration sibling task).
    pub nonce: u64,
    /// Ether value to transfer — always zero for MEV swap execution.
    /// `value` in `tx_params`.
    pub value: U256,
    /// EIP-2930 access list (re-computed during simulation). `accessList` in
    /// `tx_params`.
    pub access_list: AccessList,
    /// `maxFeePerGas` (wei) — finalized by
    /// [`finalize_fees`](crate::fee::finalize_fees) (`int(1.5 *
    /// base_fee_next) + priority_fee`).
    pub max_fee_per_gas: u128,
    /// `maxPriorityFeePerGas` (wei) — finalized by
    /// [`finalize_fees`](crate::fee::finalize_fees) (= `priority_fee`).
    pub max_priority_fee_per_gas: u128,
}

impl TxParams {
    /// Build a `TxParams` with zero-value, empty data, empty access list, and
    /// zeroed fees — the caller fills `to`/`data`/`gas_limit`/`nonce`, then
    /// calls [`finalize_fees`](crate::fee::finalize_fees) to set the fee
    /// fields. Provided for ergonomic construction in tests + the `PyO3` seam.
    #[must_use]
    pub fn new(to: Address, data: Bytes, gas_limit: u64, nonce: u64) -> Self {
        Self {
            to,
            data,
            gas_limit,
            nonce,
            value: U256::ZERO,
            access_list: AccessList::default(),
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
        }
    }
}
