//! The bid's wallet economics: the price reads the head-advance refresh rides.
//!
//! Invariant surface: these are reads and pure derivations, never lifecycle
//! moves. The submit ordering itself (the relay fan-out, the bundle target,
//! the dispatch) lives in the frame module.

use crate::backrun::BackrunConfig;
use crate::frame_pipeline::priority_fee_wei;
use degenbot_rpc::provider::AlloyProvider;

/// The wallet's gas burn for one composed bundle at `head`:
/// estimate x (next base fee x 1.2 + priority). Read on head advances so
/// the compose gate always prices at a fresh base fee.
pub(super) async fn wallet_gas_cost_at(
    provider: &AlloyProvider,
    head: u64,
    cfg: &BackrunConfig,
) -> u128 {
    let base_fee_next = provider
        .get_block(head)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.header.base_fee_per_gas)
        .map_or(30_000_000_000u128, |x| u128::from(x) * 12 / 10);
    u128::from(cfg.bundle_gas_est)
        .saturating_mul(base_fee_next.saturating_add(priority_fee_wei(cfg)))
}

pub(super) async fn initial_wallet_gas_cost(provider: &AlloyProvider, cfg: &BackrunConfig) -> u128 {
    let head = provider.get_block_number().await.unwrap_or(0);
    wallet_gas_cost_at(provider, head, cfg).await
}
