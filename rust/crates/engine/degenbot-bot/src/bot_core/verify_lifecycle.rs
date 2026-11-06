//! The pose-independent Verify lifecycle — one family-free operation over a
//! pool key and an optional snapshot block.
//!
//! `quarantine → seed verify → drain + pin → post-drain verify → live` is one
//! machine ([`crate::bot_core::run_verify_lifecycle`]) over one [`PoolLookup`],
//! and the *policy* around entering it — the at-most-once claim, the durable
//! verified-pool fact, and the bounded retry of transient failures — used to be
//! the session driver's. It is a property of the capability, not of a pump: a
//! process that never subscribes still verifies pools.
//!
//! So the provider, the `StateView` address, and the retry policy are
//! CONSTRUCTION values (one [`VerifyLifecycleConfig`], installed once), and the
//! interface is one operation: [`VerifyLifecycle::run`]. There is no `_sync` /
//! `_with_retry` verb lattice and no per-family key argument — the key is a
//! value and the retry lives inside the one body, so a rule learned here holds
//! for V3 and V4 callers alike (ADR-022 D1/D3, ADR-093 D4).

use std::sync::Arc;

use alloy::primitives::Address;
use parking_lot::Mutex;
use tracing::Instrument as _;

use degenbot_core::errors::ProviderError;
use degenbot_core::{diag, op_warn};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::{BotState, PoolLookup};

use super::registration_ledger::{PreBuildIdentity, RefusalKind};
use super::verification_retry::{retry_verification_call, RetryPolicy};
use super::verify_claims::PoolVerifications;

/// The three construction values the lifecycle enters with.
///
/// They are configuration, never a per-call argument: a caller cannot re-point
/// a live lifecycle's provider between two attempts, so a retry re-verifies
/// against the same chain view the first attempt used.
#[derive(Clone)]
pub struct VerifyLifecycleConfig {
    /// The bot's ONE RPC provider (ADR-022 D3). `None` fails a tracked pool fast
    /// with the missing-provider precondition; sparse pools never need one.
    pub provider: Option<AlloyProvider>,
    /// The V4 `StateView` contract address the deployment resolved.
    pub state_view: Option<Address>,
    /// The bounded retry policy for transient failures.
    pub retry: RetryPolicy,
}

impl Default for VerifyLifecycleConfig {
    fn default() -> Self {
        Self {
            provider: None,
            state_view: None,
            retry: RetryPolicy::verification_default(),
        }
    }
}

/// The one owner of the verify lifecycle's policy: the claim window, the
/// durable verified-pool fact, and the bounded retry.
pub struct VerifyLifecycle {
    /// The shared state owner the machine runs over.
    core: Arc<StateLock<BotState>>,
    /// The construction values (see [`VerifyLifecycleConfig`]).
    config: Mutex<VerifyLifecycleConfig>,
    /// The at-most-once claim window plus the durable verified-pool fact.
    verifications: PoolVerifications<RefusalKind>,
    /// Test-only scripted attempt outcomes: each attempt pops one, and an empty
    /// script runs the real lifecycle.
    #[cfg(test)]
    script: Mutex<Vec<Result<(), RefusalKind>>>,
}

impl VerifyLifecycle {
    /// A lifecycle over the shared state owner.
    #[must_use]
    pub fn new(core: Arc<StateLock<BotState>>, config: VerifyLifecycleConfig) -> Self {
        Self {
            core,
            config: Mutex::new(config),
            verifications: PoolVerifications::new(),
            #[cfg(test)]
            script: Mutex::new(Vec::new()),
        }
    }

    /// Install the construction values — the one configuration door.
    pub fn configure(&self, config: VerifyLifecycleConfig) {
        *self.config.lock() = config;
    }

    /// The construction values in force (for a caller merging one field).
    #[must_use]
    pub fn config(&self) -> VerifyLifecycleConfig {
        self.config.lock().clone()
    }

    /// Whether this pool's verify lifecycle already COMPLETED.
    #[must_use]
    pub fn is_verified(&self, key: &PreBuildIdentity) -> bool {
        self.verifications.is_verified(key)
    }

    /// How many claim windows are live (the at-most-once witness).
    #[must_use]
    pub fn live_claim_count(&self) -> usize {
        self.verifications.live_claim_count()
    }

    /// How many pools hold a completed-lifecycle fact.
    #[must_use]
    pub fn verified_pool_count(&self) -> usize {
        self.verifications.verified_pool_count()
    }

    /// Run one pool's verify lifecycle: at most once per live claim window,
    /// never again once completed, under the configured bounded retry.
    ///
    /// The claim is keyed by the same identity the registration ledger memos on
    /// ([`PreBuildIdentity`]), so an admission ledger and this claim table share
    /// one fact instead of three private string formats. An unregistered pool is
    /// a no-op `Ok` that records nothing — a later registration of that pool
    /// still needs the verify it is owed.
    ///
    /// The retry runs INSIDE the claim window: concurrent callers share one
    /// attempt sequence and its final outcome, and a failed sequence releases
    /// the window so a later caller retries.
    ///
    /// # Errors
    ///
    /// The lifecycle's [`RefusalKind`]: a fatal one immediately (a mismatch is
    /// the tripwire, never retried), a transient one once the policy is
    /// exhausted.
    pub async fn run(
        &self,
        key: PoolLookup,
        snapshot_block: Option<u64>,
    ) -> Result<(), RefusalKind> {
        let claim_key = claim_key(key);
        if self.verifications.is_verified(&claim_key) {
            return Ok(());
        }
        // An unregistered pool is a no-op: recording a fact here would skip the
        // verify the pool still owes once it IS registered.
        let registered = {
            let state = self.core.read_at(LockSite::Registration);
            key.coverage(&state).is_some()
        };
        if !registered {
            return Ok(());
        }
        let config = self.config.lock().clone();
        self.verifications
            .run_exclusive(&claim_key, || async move {
                let span = tracing::info_span!(
                    "degenbot.pool.verify_lifecycle",
                    pool.version = key.family().tag(),
                    pool.address = %key,
                );
                let result = retry_verification_call(&config.retry, |_attempt| {
                    #[cfg(test)]
                    let scripted = self.script.lock().pop();
                    let provider = config.provider.clone();
                    let core = Arc::clone(&self.core);
                    async move {
                        #[cfg(test)]
                        if let Some(scripted) = scripted {
                            return scripted;
                        }
                        crate::bot_core::run_registration_lifecycle(
                            &core,
                            provider.as_ref(),
                            key,
                            snapshot_block,
                        )
                        .await
                    }
                })
                .instrument(span)
                .await;
                if result.is_ok() {
                    diag!(domain = pump, version = %key.family().tag(), pool = %key, "registration verify-lifecycle complete");
                } else {
                    op_warn!(domain = pump, version = %key.family().tag(), pool = %key, "registration verify-lifecycle FAILED");
                }
                result
            })
            .await
    }

    /// Script the next attempt outcomes: each attempt pops one entry, and an
    /// empty script runs the real lifecycle. The seam a suite needs to observe
    /// the retry bound and the fatal-vs-transient split without a live node.
    #[cfg(test)]
    pub(crate) fn script_attempts_for_test(&self, attempts: Vec<Result<(), RefusalKind>>) {
        // Attempts answer in the order the caller scripted them (the pop reads
        // the tail).
        *self.script.lock() = attempts.into_iter().rev().collect();
    }

    /// How many scripted attempts are still unconsumed.
    #[cfg(test)]
    pub(crate) fn remaining_script_for_test(&self) -> usize {
        self.script.lock().len()
    }
}

/// Build the bot's ONE verify provider from a node endpoint (ADR-022 D3).
///
/// The one home of "make a provider for verification": the session boot and
/// every verify-home configuration call it, so a provider cannot be built two
/// different ways. Construction failure is the CALLER's to refuse at once
/// (`Result`, never a swallowed `Option`): a dead endpoint fails the
/// configuration door loudly instead of leaving the verify home silently
/// unconfigured — where the tracked pool would only later trip the
/// missing-provider precondition.
///
/// # Errors
///
/// The transport's own [`ProviderError`] when the endpoint cannot be built.
pub async fn build_verify_provider(rpc_url: &str) -> Result<AlloyProvider, ProviderError> {
    AlloyProvider::new(rpc_url, degenbot_rpc::provider::DEFAULT_MAX_RETRIES).await
}

/// The claim key for a pool key: the identity the ledger memos on.
fn claim_key(key: PoolLookup) -> PreBuildIdentity {
    match key {
        PoolLookup::Address(address) => PreBuildIdentity::Address(address),
        PoolLookup::V4 { pool_id, .. } => PreBuildIdentity::V4(pool_id),
    }
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::registration_ledger::{RegistrationPhase, VerifyOrigin};
    use alloy::primitives::{U128, U256};
    use degenbot_pools::{
        PoolTickCoverage, RegisterV3PoolParams, RegisterV4PoolParams, TickInfo, V4PoolKey,
    };
    use hashbrown::HashMap;

    const SNAPSHOT: u64 = 42;

    fn lifecycle() -> VerifyLifecycle {
        VerifyLifecycle::new(
            Arc::new(StateLock::new(BotState::new())),
            VerifyLifecycleConfig {
                provider: None,
                state_view: None,
                retry: policy(3),
            },
        )
    }

    fn policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            base_delay: 0.0,
            max_delay: 0.0,
            jitter: 0.0,
        }
    }

    /// A transient verify failure: a per-call transport refusal.
    fn transient() -> RefusalKind {
        RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            false,
            VerifyOrigin::Transport,
            "transport blip".to_owned(),
        )
    }

    /// A fatal verify failure: the on-chain divergence tripwire.
    fn mismatch() -> RefusalKind {
        RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            true,
            VerifyOrigin::Mismatch,
            "snapshot mismatch".to_owned(),
        )
    }

    /// Register a V3 pool at `address` with `coverage`.
    fn register_v3(
        core: &StateLock<BotState>,
        address: Address,
        coverage: PoolTickCoverage,
    ) -> u64 {
        let mut tick_data = HashMap::new();
        if coverage == PoolTickCoverage::Tracked {
            tick_data.insert(
                60,
                TickInfo {
                    liquidity_gross: U128::from(100),
                    liquidity_net: 100i128,
                    block: 0,
                },
            );
        }
        core.write_at(LockSite::Registration)
            .register_v3_pool(&RegisterV3PoolParams {
                address,
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                tick_data_block: None,
                coverage,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 registration")
    }

    /// Register a Sparse V4 pool under `(pool_manager, pool_id)`.
    fn register_v4(core: &StateLock<BotState>, pool_manager: Address, pool_id: [u8; 32]) -> u64 {
        core.write_at(LockSite::Registration)
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                state_view: Address::ZERO,
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
                known_bitmap_words: Vec::new(),
            })
            .expect("test setup: V4 registration")
    }

    /// A sparse pool completes with no provider and no verify RPC: it is Live
    /// the moment it is registered, and the lifecycle records that as the fact.
    #[tokio::test]
    async fn a_sparse_pool_completes_without_a_provider() {
        let lifecycle = lifecycle();
        let address = Address::from([0x11u8; 20]);
        register_v3(&lifecycle.core, address, PoolTickCoverage::Sparse);

        lifecycle
            .run(PoolLookup::from_address(address), Some(SNAPSHOT))
            .await
            .expect("a sparse pool verifies without a provider");
        assert!(lifecycle.is_verified(&PreBuildIdentity::Address(address)));
        assert_eq!(lifecycle.verified_pool_count(), 1);
        assert_eq!(
            lifecycle.live_claim_count(),
            0,
            "the claim window released on settlement"
        );
    }

    /// A completed lifecycle is a pool fact: a second run of the same identity
    /// spends no work, even if the pool's coverage changed to one that would
    /// verify (and fail) on its own.
    #[tokio::test]
    async fn a_completed_lifecycle_is_a_pool_fact_so_a_second_run_spends_nothing() {
        let lifecycle = lifecycle();
        let address = Address::from([0x12u8; 20]);
        register_v3(&lifecycle.core, address, PoolTickCoverage::Sparse);
        lifecycle
            .run(PoolLookup::from_address(address), Some(SNAPSHOT))
            .await
            .expect("first lifecycle");

        // Swap the verified Sparse pool for a Tracked one at the same address:
        // a re-run would reach the verify step and fail for lack of a provider.
        lifecycle
            .core
            .write_at(LockSite::Registration)
            .unregister_pool(PoolLookup::from_address(address));
        register_v3(&lifecycle.core, address, PoolTickCoverage::Tracked);

        lifecycle
            .run(PoolLookup::from_address(address), Some(SNAPSHOT))
            .await
            .expect("a verified pool is not re-verified");
        assert_eq!(lifecycle.verified_pool_count(), 1);
    }

    /// An unregistered pool is a no-op `Ok` that records NO fact: the verify it
    /// is owed must still happen when it is finally registered.
    #[tokio::test]
    async fn an_unregistered_pool_records_no_fact() {
        let lifecycle = lifecycle();
        let address = Address::from([0x13u8; 20]);
        lifecycle
            .run(PoolLookup::from_address(address), Some(SNAPSHOT))
            .await
            .expect("an unregistered pool is a no-op");
        assert_eq!(lifecycle.verified_pool_count(), 0);
    }

    /// A transient failure is retried until the lifecycle answers; the policy's
    /// attempt bound is what decides how many times.
    #[tokio::test]
    async fn a_transient_failure_is_retried_up_to_the_policy() {
        let lifecycle = lifecycle();
        let address = Address::from([0x14u8; 20]);
        register_v3(&lifecycle.core, address, PoolTickCoverage::Sparse);
        // Two transient failures, then the real (successful) lifecycle.
        lifecycle.script_attempts_for_test(vec![Err(transient()), Err(transient())]);

        lifecycle
            .run(PoolLookup::from_address(address), Some(SNAPSHOT))
            .await
            .expect("the third attempt runs the real lifecycle");
        assert_eq!(lifecycle.remaining_script_for_test(), 0);
        assert!(lifecycle.is_verified(&PreBuildIdentity::Address(address)));
    }

    /// The same transient failure under a one-attempt policy is returned at
    /// once: the retry is bounded by the policy, not by the classification.
    #[tokio::test]
    async fn an_exhausted_policy_returns_the_last_transient_failure() {
        let lifecycle = VerifyLifecycle::new(
            Arc::new(StateLock::new(BotState::new())),
            VerifyLifecycleConfig {
                provider: None,
                state_view: None,
                retry: policy(1),
            },
        );
        let address = Address::from([0x15u8; 20]);
        register_v3(&lifecycle.core, address, PoolTickCoverage::Sparse);
        lifecycle.script_attempts_for_test(vec![Err(transient()), Err(transient())]);

        let err = lifecycle
            .run(PoolLookup::from_address(address), Some(SNAPSHOT))
            .await
            .expect_err("the policy admitted one attempt");
        assert_eq!(err, transient());
        assert_eq!(
            lifecycle.remaining_script_for_test(),
            1,
            "a one-attempt policy never re-tries"
        );
        assert_eq!(
            lifecycle.verified_pool_count(),
            0,
            "a failure records no fact"
        );
    }

    /// A fatal mismatch is the tripwire: it is never retried, however wide the
    /// policy, and it records no fact (the pool stays quarantined and a later
    /// caller retries the whole lifecycle).
    #[tokio::test]
    async fn a_fatal_failure_is_never_retried() {
        let lifecycle = lifecycle();
        let address = Address::from([0x16u8; 20]);
        register_v3(&lifecycle.core, address, PoolTickCoverage::Sparse);
        lifecycle.script_attempts_for_test(vec![Err(mismatch()), Err(transient())]);

        let err = lifecycle
            .run(PoolLookup::from_address(address), Some(SNAPSHOT))
            .await
            .expect_err("a mismatch fails the lifecycle");
        assert_eq!(err, mismatch());
        assert_eq!(
            lifecycle.remaining_script_for_test(),
            1,
            "a fatal failure is not retried"
        );
        assert_eq!(lifecycle.verified_pool_count(), 0);
    }

    /// The verified fact is key-scoped, never address-scoped: a V3 pool at an
    /// address does not verify a V4 pool under the same manager address.
    #[tokio::test]
    async fn the_verified_fact_is_key_scoped_not_address_scoped() {
        let lifecycle = lifecycle();
        let shared = Address::from([0x17u8; 20]);
        let sibling = [0x5au8; 32];
        register_v3(&lifecycle.core, shared, PoolTickCoverage::Sparse);
        register_v4(&lifecycle.core, shared, sibling);

        lifecycle
            .run(PoolLookup::from_address(shared), Some(SNAPSHOT))
            .await
            .expect("V3 lifecycle");
        assert!(lifecycle.is_verified(&PreBuildIdentity::Address(shared)));
        assert!(
            !lifecycle.is_verified(&PreBuildIdentity::V4(sibling)),
            "a V3 fact is not a V4 fact at the same address"
        );

        lifecycle
            .run(
                PoolLookup::V4 {
                    pool_manager: shared,
                    pool_id: sibling,
                },
                Some(SNAPSHOT),
            )
            .await
            .expect("V4 lifecycle");
        assert_eq!(lifecycle.verified_pool_count(), 2);
    }
}
