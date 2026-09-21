//! The bid's economics: the price reads, the broadcast relay fan-out, and the
//! bundle target a decided frame is submitted against.
//!
//! Invariant surface: these are reads and pure derivations, never lifecycle
//! moves. A bid's gas cost is priced off the observed head's next base fee,
//! and `strategy.backrun.mevblocker_url` augments the raw broadcast fan-out
//! without ever replacing the bundle target (pinned by
//! `mevblocker_url_does_not_alter_the_target`).

use std::sync::Arc;

use alloy::primitives::B256;
use degenbot_bot::backrun::BackrunConfig;
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};

use crate::bundle::MEVBLOCKER_STREAM_URL;
use crate::submit::{BundleTarget, SubmissionTarget};

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
/// Empty when `strategy.backrun.mevblocker_url` is unset: the submit leaf then
/// broadcasts to the read provider alone. When set, the configured private
/// endpoint leads and the read provider follows, so the private path is tried
/// first and the public provider is the fallback relay. An endpoint that cannot
/// be constructed degrades to the read-provider-only list.
pub(super) async fn build_broadcast_relays(
    cfg: &BackrunConfig,
    provider: &Arc<AlloyProvider>,
) -> Vec<Arc<AlloyProvider>> {
    let Some(url) = cfg.mevblocker_url.as_deref() else {
        return Vec::new();
    };
    match AlloyProvider::new(url, DEFAULT_MAX_RETRIES).await {
        Ok(private) => vec![Arc::new(private), Arc::clone(provider)],
        Err(error) => {
            tracing::warn!(
                %error,
                url,
                "strategy.backrun.mevblocker_url provider build failed - read provider only"
            );
            Vec::new()
        }
    }
}

/// The bid's submission target: this frame's target hash pinned to the next
/// block, `MEVBlocker` searcher WS only.
///
/// Deliberately independent of `strategy.backrun.mevblocker_url`: that key
/// augments the raw broadcast fan-out (see [`build_broadcast_relays`]) and
/// never replaces the bundle target. The bundle (auction) arm keeps its own
/// economics; the private endpoint engages only when the target fans out
/// under [`SubmissionTarget::Public`].
pub(super) fn bid_submission_target(
    cfg: &BackrunConfig,
    target_tx_hash: B256,
    block_number: u64,
) -> SubmissionTarget {
    SubmissionTarget::Bundle(BundleTarget {
        stream_url: if cfg.stream_url.is_empty() {
            String::from(MEVBLOCKER_STREAM_URL)
        } else {
            cfg.stream_url.clone()
        },
        target_tx_hash,
        block_number,
    })
}
