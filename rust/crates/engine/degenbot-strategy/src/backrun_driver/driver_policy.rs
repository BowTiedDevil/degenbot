//! The bid's wallet economics: the price reads the head-advance refresh rides.
//!
//! Invariant surface: these are reads and pure derivations, never lifecycle
//! moves. The submit ordering itself (the relay fan-out, the bundle target,
//! the dispatch) lives in the frame module.
//!
//! A failed read prices NOTHING: a transport refusal or an absent block
//! propagates through the driver's error channel, so no read failure can move
//! the wallet cost. The one value the policy invents is the explicit legacy
//! next base fee, and only a block the chain proves pre-EIP-1559 prices at it.

use std::future::Future;
use std::pin::Pin;

use crate::backrun::BackrunConfig;
use crate::frame_pipeline::priority_fee_wei;
use degenbot_rpc::provider::AlloyProvider;

use super::driver_boot::BackrunBootError;

/// The next base fee the pricing policy applies to a block the chain proves
/// pre-EIP-1559: a successfully-read block carrying no base fee, since a
/// post-London header always carries one. This is the price of that proven
/// state, never a fallback for a read that failed.
pub(super) const LEGACY_NEXT_BASE_FEE_WEI: u128 = 30_000_000_000;

/// What one base-fee read proved about the block it read. The three states
/// stay distinct: a fresh fee, a proven legacy block, and a number the node
/// has no block at. A read that proves NOTHING is not here - it is the
/// transport refusal the reader returns as its error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BaseFeeAnswer {
    /// The block carries this EIP-1559 base fee (wei per gas).
    Fee(u128),
    /// The block carries no base fee, which proves it pre-EIP-1559: the
    /// explicit [`LEGACY_NEXT_BASE_FEE_WEI`] policy prices it.
    MissingBaseFee,
    /// The node has no block at the read number.
    MissingBlock,
}

/// The boxed future one narrow read resolves on, boxed so the trait stays
/// object-safe and a fake answers without an async runtime of its own.
pub(super) type BlockFeeFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The narrow block/fee reads the wallet pricing rides: the head number and
/// a block's base fee, nothing else. A production reader speaks them through
/// [`AlloyProvider`]; a test's scripted reader answers the same two reads
/// with any proof state, so every failure class is producible without a live
/// HTTP service. A read that fails at the transport answers with the
/// transport's own message; a read that succeeds answers with what it proved.
pub(super) trait BlockFeeReader: Send + Sync {
    /// The head block number, or the transport's own refusal message.
    fn head_number(&self) -> BlockFeeFuture<'_, Result<u64, String>>;

    /// What the block at `block` proves about its base fee, or the
    /// transport's own refusal message.
    fn base_fee_wei(&self, block: u64) -> BlockFeeFuture<'_, Result<BaseFeeAnswer, String>>;
}

impl BlockFeeReader for AlloyProvider {
    fn head_number(&self) -> BlockFeeFuture<'_, Result<u64, String>> {
        Box::pin(read_head_number(self))
    }

    fn base_fee_wei(&self, block: u64) -> BlockFeeFuture<'_, Result<BaseFeeAnswer, String>> {
        Box::pin(read_base_fee(self, block))
    }
}

/// The [`AlloyProvider`] reader's head-number read.
async fn read_head_number(provider: &AlloyProvider) -> Result<u64, String> {
    provider
        .get_block_number()
        .await
        .map_err(|error| error.to_string())
}

/// The [`AlloyProvider`] reader's base-fee read: what the block at `block`
/// proves about its base fee.
async fn read_base_fee(provider: &AlloyProvider, block: u64) -> Result<BaseFeeAnswer, String> {
    match provider.get_block(block).await {
        Err(error) => Err(error.to_string()),
        Ok(None) => Ok(BaseFeeAnswer::MissingBlock),
        Ok(Some(read)) => Ok(match read.header.base_fee_per_gas {
            Some(base_fee) => BaseFeeAnswer::Fee(u128::from(base_fee)),
            None => BaseFeeAnswer::MissingBaseFee,
        }),
    }
}

/// The transport's own refusal carried through the driver's error channel,
/// naming the read that failed.
fn transport_refusal(read: &str, reason: &str) -> BackrunBootError {
    BackrunBootError::WalletGasPriceTransport(format!("{read}: {reason}"))
}

/// The wallet's gas burn for one composed bundle at `head`:
/// estimate x (next base fee x 1.2 + priority). Read on head advances so
/// the compose gate always prices at a fresh base fee.
///
/// # Errors
///
/// [`BackrunBootError::WalletGasPriceTransport`] when the read fails at the
/// transport before the node answers anything, and
/// [`BackrunBootError::WalletGasPriceMissingBlock`] when the node has no
/// block at `head`. Both price nothing: a read failure can never move the
/// wallet cost through a default.
pub(super) async fn wallet_gas_cost_at<R: BlockFeeReader + ?Sized>(
    reader: &R,
    head: u64,
    cfg: &BackrunConfig,
) -> Result<u128, BackrunBootError> {
    let read = reader.base_fee_wei(head).await;
    let read_name = format!("block {head} base fee");
    let answer = read.map_err(|reason| transport_refusal(&read_name, &reason))?;
    let base_fee_next = match answer {
        BaseFeeAnswer::Fee(base_fee) => base_fee * 12 / 10,
        BaseFeeAnswer::MissingBaseFee => LEGACY_NEXT_BASE_FEE_WEI,
        BaseFeeAnswer::MissingBlock => {
            return Err(BackrunBootError::WalletGasPriceMissingBlock(head));
        }
    };
    let wallet_gas_cost = u128::from(cfg.bundle_gas_est)
        .saturating_mul(base_fee_next.saturating_add(priority_fee_wei(cfg)));
    Ok(wallet_gas_cost)
}

/// The wallet cost priced at the head the node reports, for the boot's
/// initial gate value.
///
/// # Errors
///
/// [`BackrunBootError::WalletGasPriceTransport`] when the head read fails at
/// the transport - nothing is priced at block zero - plus every refusal
/// [`wallet_gas_cost_at`] carries.
pub(super) async fn initial_wallet_gas_cost<R: BlockFeeReader + ?Sized>(
    reader: &R,
    cfg: &BackrunConfig,
) -> Result<u128, BackrunBootError> {
    let head_read = reader.head_number().await;
    let head = head_read.map_err(|reason| transport_refusal("head number", &reason))?;
    wallet_gas_cost_at(reader, head, cfg).await
}
