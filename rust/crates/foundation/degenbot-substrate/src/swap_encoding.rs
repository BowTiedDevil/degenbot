//! Pre-encoded swap calldata: the submission door after a path is chosen.
//!
//! The dispatch is family-wide rather than per-family, because the two ways an
//! encode can fail need different operator responses: an unregistered pool id
//! is a caller bug, while a registered family with no swap-call encoder is a
//! capability gap. A per-family entry point could not tell them apart without
//! the caller consulting the family tag itself.
//!
//! Only the reserve-pair family carries an encoder today; adding a family adds
//! an arm here, never a second entry point.

use alloy::primitives::{Address, U256};

use degenbot_uniswap::v2_encoding::encode_v2_swap;

use crate::pool_lookup::entry_family;

use super::{BotState, EncodedCall, PoolEntry};

/// Why [`BotState::encode_swap`] refused a call.
#[derive(Debug, thiserror::Error)]
pub enum EncodeSwapError {
    /// The pool id is not registered.
    #[error("pool {pool_id} is not registered")]
    NotRegistered {
        /// The requested pool id.
        pool_id: u64,
    },
    /// The pool's family has no swap-call encoder.
    #[error("pool {pool_id} family {family:?} has no swap encoder")]
    UnsupportedFamily {
        /// The requested pool id.
        pool_id: u64,
        /// The registered family tag.
        family: &'static str,
    },
    /// The V2 encoder rejected the call.
    #[error("pool {pool_id} swap call failed to encode")]
    Encode {
        /// The requested pool id.
        pool_id: u64,
        /// The ABI-level failure.
        #[source]
        cause: degenbot_core::errors::AbiDecodeError,
    },
}

impl BotState {
    /// Encode a V2 swap call for the given pool.
    ///
    /// Produces pre-encoded calldata for `swap(uint256,uint256,address,bytes)`
    /// that is ready for on-chain submission.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeSwapError::NotRegistered`] for an unknown pool id,
    /// [`EncodeSwapError::UnsupportedFamily`] for a registered pool whose
    /// family has no swap encoder, and [`EncodeSwapError::Encode`] when the V2
    /// encoder rejects the call.
    pub fn encode_swap(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: U256,
        recipient: Address,
    ) -> Result<EncodedCall, EncodeSwapError> {
        let entry = self
            .registry
            .pools
            .get(&pool_id)
            .ok_or(EncodeSwapError::NotRegistered { pool_id })?;
        match entry {
            PoolEntry::V2(p) => encode_v2_swap(p.0.address, zero_for_one, amount_out, recipient)
                .map_err(|cause| EncodeSwapError::Encode { pool_id, cause }),
            PoolEntry::V3(..)
            | PoolEntry::V4(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => Err(EncodeSwapError::UnsupportedFamily {
                pool_id,
                family: entry_family(entry),
            }),
        }
    }
}
