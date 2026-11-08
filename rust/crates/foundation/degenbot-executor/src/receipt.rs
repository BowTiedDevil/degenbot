//! The built-in contract's own profit measurement — the evidence the
//! `cmd_executor` adapter assesses (ADR-098 D7).

//!
//! `execute(bytes,uint256)` returns this struct as TWO ABI words, in order:
//! `gross_profit` — the selected WETH-equivalent balance-domain increase after
//! the command stream and BEFORE the bribe — and `bribe_paid` — the native ETH
//! the executor actually transferred to the bribe recipient, after any
//! available-balance cap.
//!
//! The decode lives here, beside the release model that declares its shape
//! (`execute_abi.returns` in the release manifest), so a release's receipt
//! encoding travels with the release rather than with one consumer.

use alloy::primitives::{Bytes, U256};

use degenbot_execution::AssessError;

/// The built-in contract's reported measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionReceipt {
    /// The selected-domain increase after the commands, before the bribe.
    pub gross_profit: U256,
    /// The native ETH actually paid to the bribe recipient.
    pub bribe_paid: U256,
}

/// The receipt's ABI size: two `uint256` words and nothing else.
pub const EXECUTION_RECEIPT_LEN: usize = 64;

impl ExecutionReceipt {
    /// Decode a receipt from a completed `execute` call's return data.
    ///
    /// The encoding is exactly two ABI words. Anything else — absent data, a
    /// short payload, or trailing bytes — is a decode failure rather than a
    /// padded or partially-read receipt: a fabricated figure is
    /// indistinguishable from a measured one.
    ///
    /// # Errors
    ///
    /// [`AssessError::Evidence`] when the engine captured no return data;
    /// [`AssessError::Decode`] when the payload is not exactly
    /// [`EXECUTION_RECEIPT_LEN`] bytes.
    pub fn decode(return_data: Option<&Bytes>) -> Result<Self, AssessError> {
        let data = return_data.ok_or_else(|| {
            AssessError::evidence(
                "the execute call's return data was not captured: the receipt is unmeasured",
            )
        })?;
        let words: [u8; EXECUTION_RECEIPT_LEN] = data.as_ref().try_into().map_err(|_| {
            AssessError::decode(format!(
                "the execute receipt is two ABI words ({EXECUTION_RECEIPT_LEN} bytes); \
                 the call returned {}",
                data.len()
            ))
        })?;
        let mut gross = [0u8; 32];
        gross.copy_from_slice(&words[..32]);
        let mut bribe = [0u8; 32];
        bribe.copy_from_slice(&words[32..]);
        Ok(Self {
            gross_profit: U256::from_be_bytes(gross),
            bribe_paid: U256::from_be_bytes(bribe),
        })
    }

    /// The retained profit before gas: the measured increase minus the bribe
    /// actually paid.
    ///
    /// # Errors
    ///
    /// [`AssessError::Evidence`] when the bribe exceeds the measured gross. The
    /// contract REQUESTS `gross_profit * bribe_bips / 10000` with
    /// `bribe_bips <= 10000`, and its available-balance cap can only reduce what
    /// it sends (pre-existing funds raise the cap, never the requested share),
    /// so `bribe_paid <= gross_profit` holds for every receipt the contract can
    /// produce. A receipt that breaks it is inconsistent evidence; saturating
    /// the subtraction would report a figure the transaction never produced.
    pub fn retained(&self) -> Result<U256, AssessError> {
        self.gross_profit
            .checked_sub(self.bribe_paid)
            .ok_or_else(|| {
                AssessError::evidence(format!(
                    "the receipt reports bribe_paid {} above gross_profit {} — \
                     inconsistent evidence",
                    self.bribe_paid, self.gross_profit
                ))
            })
    }
}
