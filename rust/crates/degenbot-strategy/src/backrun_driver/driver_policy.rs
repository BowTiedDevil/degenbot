//! The bid's economics: the price reads, the broadcast relay fan-out, and the
//! bundle target a decided frame is submitted against.
//!
//! Invariant surface: these are reads and pure derivations, never lifecycle
//! moves. The submission slot the composition binds decides the target and
//! the raw fan-out: the `MEVBlocker` slot anchors a bundle on its searcher
//! WebSocket and leads the raw fan-out with the private endpoint, the peer
//! slot fans the signed bytes over the public relays.

use std::sync::Arc;

use crate::backrun::{BackrunConfig, SubmissionSlot};
use alloy::primitives::B256;
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};

use degenbot_submission::submit::{BundleTarget, SubmissionTarget};

/// The gas floor the envelope gate evaluates at (wei) - the composed strategy's
/// standing economics (the env override did not exist upstream either).
pub(super) const GAS_FLOOR_WEI: u64 = 50_000_000_000_000;

/// The operator's priority fee converted from the facet's gwei to wei.
pub(super) fn priority_fee_wei(cfg: &BackrunConfig) -> u128 {
    u128::from(cfg.priority_fee_gwei).saturating_mul(1_000_000_000u128)
}

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

/// The raw-broadcast relay list for the private-broadcast arm, private-first.
///
/// Empty when the `MEVBlocker` slot's private endpoint is unset: the submit leaf then
/// broadcasts to the read provider alone. When set, the configured private
/// endpoint leads and the read provider follows, so the private path is tried
/// first and the public provider is the fallback relay. An endpoint that cannot
/// be constructed degrades to the read-provider-only list.
pub(super) async fn build_broadcast_relays(
    cfg: &BackrunConfig,
    provider: &Arc<AlloyProvider>,
) -> Vec<Arc<AlloyProvider>> {
    let urls = cfg.submission.raw_relay_urls();
    if urls.is_empty() {
        return Vec::new();
    }
    let mut relays = Vec::new();
    for url in &urls {
        match AlloyProvider::new(url, DEFAULT_MAX_RETRIES).await {
            Ok(relay) => relays.push(Arc::new(relay)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    url,
                    "raw broadcast relay build failed - skipped"
                );
            }
        }
    }
    // The read provider is the public fallback relay on every raw fan-out.
    relays.push(Arc::clone(provider));
    relays
}

/// The bid's submission target, bound by the composition's slot: the
/// `MEVBlocker` arm anchors a bundle on its searcher WebSocket, the peer arm
/// fans the signed bytes out over the public relays.
pub(super) fn bid_submission_target(
    cfg: &BackrunConfig,
    target_tx_hash: B256,
    block_number: u64,
) -> SubmissionTarget {
    match &cfg.submission {
        SubmissionSlot::Mevblocker { bundle_url, .. } => SubmissionTarget::Bundle(BundleTarget {
            stream_url: bundle_url.clone(),
            target_tx_hash,
            block_number,
        }),
        SubmissionSlot::PublicFanOut { .. } => SubmissionTarget::Public,
    }
}
