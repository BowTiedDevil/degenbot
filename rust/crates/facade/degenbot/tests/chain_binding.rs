//! A `cargo add degenbot` consumer binds its provider to a chain the same way
//! every other consumer does: the refusal comes from the crate, so a
//! pure-Rust bot cannot bind a misconfigured endpoint and hear nothing.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions fail loudly"
)]

use alloy::network::Ethereum;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::transports::mock::{Asserter, MockTransport};
use degenbot::core::errors::ProviderError;
use degenbot::rpc::provider::AlloyProvider;
use std::sync::Arc;

/// A transport answering `eth_chainId` with `chain_id`, and nothing else.
fn node_reporting(chain_id: u64) -> AlloyProvider {
    let asserter = Asserter::new();
    asserter.push_success(&format!("0x{chain_id:x}"));
    let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
    let inner = ProviderBuilder::new().connect_client(client).erased();
    AlloyProvider::from_provider(Arc::new(inner) as Arc<dyn Provider<Ethereum>>)
}

#[tokio::test]
async fn binding_a_mismatched_endpoint_refuses_for_a_pure_rust_consumer() {
    let provider = node_reporting(8453);

    let Err(error) = provider.bind_to_chain(1).await else {
        panic!("an endpoint bound for chain 1 that serves 8453 never binds");
    };

    assert!(
        matches!(
            error,
            ProviderError::ChainMismatch {
                expected: 1,
                actual: 8453,
                ..
            }
        ),
        "the refusal is the core's typed chain refusal: {error:?}"
    );
}

#[tokio::test]
async fn binding_a_matching_endpoint_succeeds_for_a_pure_rust_consumer() {
    let provider = node_reporting(8453);

    provider
        .bind_to_chain(8453)
        .await
        .expect("an endpoint that serves the bound chain binds");

    assert_eq!(provider.verified_chain_id(), Some(8453));
    assert_eq!(provider.get_chain_id().await.expect("cached read"), 8453);
}
