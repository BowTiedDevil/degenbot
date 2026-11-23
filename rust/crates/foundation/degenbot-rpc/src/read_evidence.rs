//! Typed read evidence: what one exact read carried at its anchor.
//!
//! A read that carries no value is a fact, and different facts must not
//! collapse into "zero" or "absent". [`ReadEvidence`] is that vocabulary:
//! [`ReadEvidence::Value`] is a decoded answer, [`ReadEvidence::Reverted`] is the
//! EVM refusing the call at the anchor, [`ReadEvidence::Malformed`] is an answer
//! whose bytes do not decode as the declared type, and [`ReadEvidence::NoCode`]
//! is a target that holds no code at the anchor. A transport failure is not
//! evidence about the chain, so it stays the outer [`ExactReadError`].
//!
//! Both producers classify the same way: the [`BlockReads`] view reads one
//! hash-pinned call ([`BlockReads::read_evidence`]), and a Multicall3 sub-call
//! outcome is classified against its target's code presence
//! ([`ReadEvidence::from_outcome`]). A batched and a sequential read therefore
//! name the same evidence for the same chain state.

use alloy::primitives::{Address, Bytes, U256};
use degenbot_core::errors::ProviderError;

use crate::chain_evidence::{BlockReads, ExactReadError};
use crate::erc20::decode_string_return;
use crate::multicall3::MulticallOutcome;

/// A value one exact read's payload decodes to.
///
/// The declared type is what makes [`ReadEvidence::Malformed`] a statement
/// about the ANSWER rather than about the caller: a decode failure is named
/// against the shape the caller asked for.
pub trait ReadValue: Sized {
    /// What this value's return looks like, named in the malformed evidence.
    const EXPECTED: &'static str;

    /// Decode the payload a served read carried, or `None` when the bytes are
    /// not this value's return shape.
    fn decode(data: &[u8]) -> Option<Self>;
}

impl ReadValue for U256 {
    const EXPECTED: &'static str = "uint256 word";

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 32 {
            return None;
        }
        Some(U256::from_be_slice(&data[..32]))
    }
}

impl ReadValue for u64 {
    const EXPECTED: &'static str = "uint256 word";

    fn decode(data: &[u8]) -> Option<Self> {
        u64::try_from(<U256 as ReadValue>::decode(data)?).ok()
    }
}

impl ReadValue for String {
    const EXPECTED: &'static str = "ABI string";

    fn decode(data: &[u8]) -> Option<Self> {
        decode_string_return(data)
    }
}

/// What one exact read carried at its anchor.
///
/// Every variant but [`Self::Value`] is evidence that the read established
/// nothing, and each names WHY: the chain refused the call, the answer is not the
/// declared shape, or the target has no code there. A consumer that needs the
/// value refuses on the other three instead of substituting one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadEvidence<T> {
    /// The call returned and its payload decoded as the declared type.
    Value(T),
    /// The EVM refused the call at the anchor, carrying whatever revert payload
    /// the transport exposed (empty when the revert arrived as a provider error,
    /// which states its reason in the error message).
    Reverted(Bytes),
    /// The call returned, and the payload is not the declared type: the shape
    /// the caller asked for and the bytes the node served.
    Malformed {
        /// The return shape the caller declared.
        expected: &'static str,
        /// The bytes the call carried.
        data: Bytes,
    },
    /// The target holds no code at the anchor, so there is nothing to call.
    NoCode {
        /// The address the read was issued for.
        address: Address,
    },
}

impl<T> ReadEvidence<T> {
    /// Whether this evidence carries a value.
    #[must_use]
    pub const fn is_value(&self) -> bool {
        matches!(self, Self::Value(_))
    }

    /// The value this evidence carries, if it carries one.
    #[must_use]
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Reverted(_) | Self::Malformed { .. } | Self::NoCode { .. } => None,
        }
    }

    /// The value this evidence carries, consuming the evidence.
    #[must_use]
    pub fn into_value(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Reverted(_) | Self::Malformed { .. } | Self::NoCode { .. } => None,
        }
    }

    /// Re-state the value without changing the evidence class.
    #[must_use]
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> ReadEvidence<U> {
        match self {
            Self::Value(value) => ReadEvidence::Value(f(value)),
            Self::Reverted(payload) => ReadEvidence::Reverted(payload),
            Self::Malformed { expected, data } => ReadEvidence::Malformed { expected, data },
            Self::NoCode { address } => ReadEvidence::NoCode { address },
        }
    }
}

impl<T: ReadValue> ReadEvidence<T> {
    /// Classify one Multicall3 sub-call's outcome, given whether its target
    /// holds code at the anchor.
    ///
    /// A code-less target is [`Self::NoCode`] whatever the batch answered: an
    /// EVM call to an address with no code succeeds with an empty return, which
    /// would otherwise read as a malformed answer to a question never asked.
    #[must_use]
    pub fn from_outcome(address: Address, code_present: bool, outcome: &MulticallOutcome) -> Self {
        if !code_present {
            return Self::NoCode { address };
        }
        match outcome {
            MulticallOutcome::Returned(data) => Self::from_payload(data.clone()),
            MulticallOutcome::Reverted(payload) => Self::Reverted(payload.clone()),
        }
    }

    /// Classify one served payload: a value, or the malformed evidence naming
    /// the declared shape and the bytes served.
    #[must_use]
    pub fn from_payload(data: Bytes) -> Self {
        match T::decode(&data) {
            Some(value) => Self::Value(value),
            None => Self::Malformed {
                expected: T::EXPECTED,
                data,
            },
        }
    }
}

impl<T> std::fmt::Display for ReadEvidence<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(_) => write!(f, "a decoded value"),
            Self::Reverted(payload) => {
                write!(f, "an execution revert ({} payload bytes)", payload.len())
            }
            Self::Malformed { expected, data } => write!(
                f,
                "a {expected} return of {} byte(s) that does not decode",
                data.len()
            ),
            Self::NoCode { address } => write!(f, "no code at {address}"),
        }
    }
}

impl BlockReads<'_> {
    /// Whether `address` holds code at the view's anchor.
    ///
    /// # Errors
    ///
    /// [`ExactReadError::Unsupported`] when the node rejects the hash-pinned
    /// request shape, [`ExactReadError::Failed`] when the read failed.
    pub async fn code_present(&self, address: Address) -> Result<bool, ExactReadError> {
        Ok(!self.code_at(address).await?.is_empty())
    }

    /// One exact `eth_call` as [`ReadEvidence`], with the target's code presence
    /// established first so a code-less address is [`ReadEvidence::NoCode`].
    ///
    /// # Errors
    ///
    /// [`ExactReadError`] when the node cannot serve the read at the anchor: a
    /// transport fault is never evidence about the chain.
    pub async fn read_evidence<T: ReadValue>(
        &self,
        to: Address,
        data: Bytes,
    ) -> Result<ReadEvidence<T>, ExactReadError> {
        if !self.code_present(to).await? {
            return Ok(ReadEvidence::NoCode { address: to });
        }
        self.call_evidence(to, data).await
    }

    /// One exact `eth_call` as [`ReadEvidence`], for a caller that already holds
    /// its target's code presence.
    ///
    /// # Errors
    ///
    /// [`ExactReadError`] when the node cannot serve the read at the anchor.
    pub async fn call_evidence<T: ReadValue>(
        &self,
        to: Address,
        data: Bytes,
    ) -> Result<ReadEvidence<T>, ExactReadError> {
        match self.call(to, data).await {
            Ok(payload) => Ok(ReadEvidence::from_payload(payload)),
            Err(ExactReadError::Failed {
                cause: ProviderError::ExecutionReverted { .. },
                ..
            }) => Ok(ReadEvidence::Reverted(Bytes::new())),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    use alloy::primitives::address;

    fn word(value: u8) -> Bytes {
        let mut out = [0u8; 32];
        out[31] = value;
        Bytes::copy_from_slice(&out)
    }

    /// The ABI dynamic-`string` return for `text`.
    fn dynamic_string(text: &str) -> Bytes {
        let raw = text.as_bytes();
        let mut out = Vec::new();
        out.extend_from_slice(&[0u8; 31]);
        out.push(0x20);
        let mut length = [0u8; 32];
        length[24..].copy_from_slice(&u64::try_from(raw.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&length);
        let mut data = raw.to_vec();
        data.resize(raw.len().div_ceil(32) * 32, 0);
        out.extend_from_slice(&data);
        Bytes::from(out)
    }

    /// A served word decodes to its value.
    #[test]
    fn a_served_word_is_a_value() {
        let evidence = ReadEvidence::<U256>::from_payload(word(42));
        assert!(evidence.is_value());
        assert_eq!(evidence.value(), Some(&U256::from(42u64)));
        assert_eq!(evidence.into_value(), Some(U256::from(42u64)));
    }

    /// An empty or short payload is malformed evidence naming the declared shape
    /// and the bytes served - never zero.
    #[test]
    fn a_short_payload_is_malformed_evidence_not_zero() {
        for payload in [Bytes::new(), Bytes::from(vec![0x01u8, 0x02])] {
            let evidence = ReadEvidence::<U256>::from_payload(payload.clone());
            assert_eq!(
                evidence,
                ReadEvidence::Malformed {
                    expected: "uint256 word",
                    data: payload,
                }
            );
            assert_eq!(evidence.into_value(), None);
        }
    }

    /// A batch sub-call's outcome carries the same evidence a sequential read of
    /// the same target does.
    #[test]
    fn a_batch_outcome_names_revert_and_return_separately() {
        let token = address!("1111111111111111111111111111111111111111");
        assert_eq!(
            ReadEvidence::<U256>::from_outcome(token, true, &MulticallOutcome::Returned(word(7))),
            ReadEvidence::Value(U256::from(7u64))
        );
        assert_eq!(
            ReadEvidence::<U256>::from_outcome(
                token,
                true,
                &MulticallOutcome::Reverted(Bytes::from(vec![0xde, 0xad]))
            ),
            ReadEvidence::Reverted(Bytes::from(vec![0xde, 0xad]))
        );
    }

    /// A target with no code at the anchor is `NoCode` - the empty return an EVM
    /// call to it produces is not an answer to anything.
    #[test]
    fn a_code_less_target_is_no_code_evidence() {
        let token = address!("2222222222222222222222222222222222222222");
        assert_eq!(
            ReadEvidence::<U256>::from_outcome(
                token,
                false,
                &MulticallOutcome::Returned(Bytes::new())
            ),
            ReadEvidence::NoCode { address: token }
        );
    }

    /// The string decode is the shared ERC-20 one: a dynamic `string`, a
    /// NUL-padded `bytes32`, and nothing else.
    #[test]
    fn a_text_read_decodes_the_shared_string_shapes() {
        assert_eq!(
            ReadEvidence::<String>::from_payload(dynamic_string("GHO")),
            ReadEvidence::Value(String::from("GHO"))
        );

        let mut bytes32 = [0u8; 32];
        bytes32[..3].copy_from_slice(b"GHO");
        assert_eq!(
            ReadEvidence::<String>::from_payload(Bytes::copy_from_slice(&bytes32)),
            ReadEvidence::Value(String::from("GHO"))
        );

        let malformed = ReadEvidence::<String>::from_payload(Bytes::from(vec![0x01u8, 0x02]));
        assert!(
            matches!(
                &malformed,
                ReadEvidence::Malformed {
                    expected: "ABI string",
                    ..
                }
            ),
            "the malformed evidence names the declared shape: {malformed}"
        );
        assert!(malformed.to_string().contains("ABI string"));
    }

    /// The evidence class survives a value re-statement.
    #[test]
    fn map_restates_the_value_and_keeps_the_class() {
        let evidence = ReadEvidence::<U256>::from_payload(word(3)).map(|value| value.to_string());
        assert_eq!(evidence, ReadEvidence::Value(U256::from(3u64).to_string()));

        let reverted =
            ReadEvidence::<U256>::Reverted(Bytes::new()).map(|value| value + U256::from(1u64));
        assert_eq!(reverted, ReadEvidence::Reverted(Bytes::new()));
    }
}
