//! The complete transaction the Encode part returns (ADR-098 D2).
//!
//! Encode hands back **one complete transaction**, not bare calldata: `to`,
//! `data`, and the native `value` that travels with the call. A contract that
//! funds itself with `msg.value`, deploys at a fixed immutable address, or
//! interprets its own byte format is expressible here without any of the
//! built-in executor's settlement assumptions.
//!
//! One solved opportunity maps to this one transaction (ADR-098 D2). A contract
//! may batch internally; the seam carries no multi-transaction choreography.

use alloy::primitives::{Address, Bytes, U256};

/// The generic Encode result: one complete transaction for one opportunity.
///
/// `value` is the native currency sent with the call (wei). It is preserved
/// verbatim from the adapter through the neutral request; the engine's
/// simulation and submission mechanics submit exactly this transaction.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TransactionRequest {
    /// The contract that receives the call.
    pub to: Address,
    /// The calldata, in whatever ABI or byte format the adapter chose.
    pub data: Bytes,
    /// The native value sent with the call, in wei.
    pub value: U256,
}

impl TransactionRequest {
    /// Build a complete transaction from its three neutral parts.
    #[must_use]
    pub const fn new(to: Address, data: Bytes, value: U256) -> Self {
        Self { to, data, value }
    }

    /// Build a value-less call — the shape a flash-funded contract takes.
    #[must_use]
    pub fn call(to: Address, data: impl Into<Bytes>) -> Self {
        Self {
            to,
            data: data.into(),
            value: U256::ZERO,
        }
    }

    /// Set the native value sent with the call (wei).
    #[must_use]
    pub const fn with_value(mut self, value: U256) -> Self {
        self.value = value;
        self
    }
}
