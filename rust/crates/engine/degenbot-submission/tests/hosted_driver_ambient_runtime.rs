#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

//! A hosted driver must see the multi-thread runtime ambience a dedicated
//! `#[tokio::main]` entrypoint provides (the retired standalone bin's shape).
//!
//! The live two-driver run failed every `BlockSimHandle::build` with "no ambient
//! multi-threaded tokio runtime" because `StrategyHost::start_driving` booted
//! the !Send driver future on a dedicated current-thread runtime; the standalone
//! sidecar polls the same future directly on a multi-thread runtime. The
//! engine's cold-miss adapter carries the same callsite contract today:
//! `BlockingAsyncDb::new` returns `None` unless the current handle is
//! multi-threaded, and its reads then block on that handle through
//! `block_in_place`.
//!
//! This test boots a driver through the real host edge and asserts both halves
//! of the contract inside the driver's own future: construction returns `Some`,
//! and a read completes (the `block_in_place` path) without panicking — plus the
//! negative half, a current-thread runtime capturing no handle. The provider is
//! a mock transport, so no node is needed.

use std::sync::Arc;

use alloy::network::Ethereum;
use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::transports::mock::{Asserter, MockTransport};

use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_bot::strategy_host::{FacetStatus, StrategyHost};
use degenbot_eventhub::Hub;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_substrate::driver_spawn::{DriverExit, DriverSpawnFactory};
use degenbot_substrate::nonce::{NonceAuthority, StrategyId};
use degenbot_substrate::RouteRegistry;
use evm2::evm::Database;

/// The block every adapter read is fenced to.
const READ_BLOCK: u64 = 1;

/// A provider answering [`AlloyProvider::get_account_at`]'s
/// balance/nonce/code batch with an empty account — the offline read the
/// ambient-runtime contract performs.
fn offline_provider() -> AlloyProvider {
    let asserter = Asserter::new();
    asserter.push_success(&"0x0"); // eth_getBalance
    asserter.push_success(&"0x0"); // eth_getTransactionCount
    asserter.push_success(&"0x"); // eth_getCode
    let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    AlloyProvider::from_provider(Arc::new(dyn_provider) as Arc<dyn Provider<Ethereum>>)
}

/// The negative half: a current-thread runtime captures no handle, so the
/// blocking adapter refuses to build (the failure mode the live run hit).
#[tokio::test]
async fn current_thread_runtime_captures_no_blocking_db() {
    let provider = offline_provider();
    let db = AlloyAsyncDb::new(&provider, READ_BLOCK);
    assert!(
        BlockingAsyncDb::new(db).is_none(),
        "a current-thread runtime must not yield a blocking cold-miss adapter"
    );
}

/// The positive half, driven through the real host edge.
#[tokio::test]
async fn hosted_driver_builds_the_blocking_db() {
    let mut host = StrategyHost::new(
        Arc::new(Hub::new()),
        Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
        Arc::new(NonceAuthority::new(7)),
    );
    let id = StrategyId::new("backrun");
    host.register(id.clone(), FacetStatus::Configured)
        .expect("register");

    let (contract_tx, contract_rx) = tokio::sync::oneshot::channel::<bool>();
    let spawn: DriverSpawnFactory = Box::new(move |_namespace| {
        Box::pin(async move {
            // The exact `BlockSimHandle::build_inner` construction contract.
            let provider = offline_provider();
            let db = AlloyAsyncDb::new(&provider, READ_BLOCK);
            // The layer-read contract: the adapter's reads block on the
            // captured handle, which must be reachable via `block_in_place`
            // from the driver's runtime context.
            let read_ok = BlockingAsyncDb::new(db)
                .is_some_and(|mut db| matches!(db.get_account(&Address::ZERO), Ok(None)));
            contract_tx.send(read_ok).expect("driver contract receiver");
            DriverExit::Stopped
        })
    });
    host.attach_spawn(&id, spawn).expect("attach spawn");
    host.enable(&id).expect("enable");

    let tasks = host.start_driving().expect("start driving");
    let task = tasks.into_iter().next().expect("one driver task");
    assert_eq!(task.wait().await, DriverExit::Stopped);
    assert!(
        contract_rx
            .await
            .expect("driver reported the callsite verdict"),
        "hosted driver must expose a multi-thread runtime to the blocking cold-miss adapter"
    );
}
