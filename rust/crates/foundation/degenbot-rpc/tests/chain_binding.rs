//! Binding an endpoint to a chain is what makes "one Bot per chain" a
//! property of the transport rather than of each caller (ADR-006 D5). The
//! guard costs exactly one `eth_chainId` round-trip per BINDING; the verified
//! id then rides on the provider, so a hot-path read never re-verifies.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions fail loudly"
)]

use alloy::network::Ethereum;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::transports::mock::{Asserter, MockTransport};
use degenbot_core::errors::ProviderError;
use degenbot_rpc::provider::AlloyProvider;
use std::sync::Arc;

/// A transport whose queue holds EXACTLY the answers the test means to spend:
/// a request the test did not budget for errors instead of quietly succeeding.
fn scripted(asserter: &Asserter) -> AlloyProvider {
    let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
    let inner = ProviderBuilder::new().connect_client(client).erased();
    AlloyProvider::from_provider(Arc::new(inner) as Arc<dyn Provider<Ethereum>>)
}

/// An endpoint that answers `eth_chainId` with `chain_id` and nothing else.
fn reporting(chain_id: u64) -> Asserter {
    let asserter = Asserter::new();
    asserter.push_success(&format!("0x{chain_id:x}"));
    asserter
}

#[tokio::test]
async fn binding_to_the_wrong_chain_refuses_and_names_both_chain_ids() {
    let asserter = reporting(8453);
    let provider = scripted(&asserter);

    let error = provider
        .bind_to_chain(1)
        .await
        .expect_err("an endpoint bound for chain 1 that serves 8453 never binds");

    let ProviderError::ChainMismatch {
        expected, actual, ..
    } = error
    else {
        panic!("a wrong-chain endpoint is a chain refusal: {error:?}");
    };
    assert_eq!(
        expected, 1,
        "the refusal names the chain the caller declared"
    );
    assert_eq!(
        actual, 8453,
        "the refusal names the chain the endpoint serves"
    );

    let message = error.to_string();
    assert!(
        message.contains('1') && message.contains("8453"),
        "the message a log line carries must name both: {message}"
    );
    assert_eq!(
        provider.verified_chain_id(),
        None,
        "a refused binding caches nothing"
    );
}

#[tokio::test]
async fn binding_to_the_matching_chain_succeeds_and_never_re_verifies() {
    let asserter = reporting(1);
    let provider = scripted(&asserter);

    provider
        .bind_to_chain(1)
        .await
        .expect("an endpoint that serves the bound chain binds");

    assert_eq!(provider.verified_chain_id(), Some(1));
    assert!(
        asserter.read_q().is_empty(),
        "the binding spent its one read"
    );

    // The response queue is empty, so a second `eth_chainId` would error
    // rather than answer. Three successful reads ARE the no-repeat proof.
    for _ in 0..3 {
        assert_eq!(provider.get_chain_id().await.expect("cached read"), 1);
    }

    provider
        .bind_to_chain(1)
        .await
        .expect("re-binding an already-verified provider is a no-op");
    assert!(
        asserter.read_q().is_empty(),
        "no round-trip is spent after the binding verified"
    );
}

#[tokio::test]
async fn a_clone_shares_the_verified_chain_of_its_source() {
    let asserter = reporting(8453);
    let provider = scripted(&asserter);
    provider
        .bind_to_chain(8453)
        .await
        .expect("matching chain binds");

    let clone = provider.clone();
    assert_eq!(clone.verified_chain_id(), Some(8453));
    assert_eq!(clone.get_chain_id().await.expect("cached read"), 8453);
    assert!(asserter.read_q().is_empty());
}

/// A fake IPC node: a Unix socket that answers `eth_chainId` with `chain_id`
/// and counts the chain-id reads it served. The IPC transport is a different
/// transport from HTTP, so a guard that only ran over HTTP would look covered
/// while leaving the local-node path unchecked.
#[cfg(unix)]
struct FakeIpcNode {
    path: std::path::PathBuf,
    chain_id_reads: Arc<std::sync::atomic::AtomicUsize>,
    _dir: tempfile::TempDir,
}

#[cfg(unix)]
impl FakeIpcNode {
    fn serving(chain_id: u64) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("node.ipc");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind the fake node");
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        std::thread::spawn(move || {
            use std::io::Write;

            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buf = Vec::new();
                let mut byte = [0_u8; 1];
                // The IPC transport writes one request object and waits for a
                // frame back, so reading to the closing brace frames it.
                while let Ok(1) = std::io::Read::read(&mut stream, &mut byte) {
                    buf.push(byte[0]);
                    if byte[0] == b'}' {
                        break;
                    }
                }
                let request: serde_json::Value =
                    serde_json::from_slice(&buf).unwrap_or(serde_json::Value::Null);
                let id = request.get("id").cloned().unwrap_or(1.into());
                let is_chain_id =
                    request.get("method") == Some(&serde_json::Value::from("eth_chainId"));
                if is_chain_id {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                let result = if is_chain_id {
                    format!("0x{chain_id:x}")
                } else {
                    "0x0".to_string()
                };
                let response = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                let _ = stream
                    .write_all(format!("{response}\n").as_bytes())
                    .and_then(|()| stream.flush());
            }
        });
        Self {
            path,
            chain_id_reads: reads,
            _dir: dir,
        }
    }

    fn uri(&self) -> String {
        format!("ipc://{}", self.path.display())
    }

    fn served_chain_id_reads(&self) -> usize {
        self.chain_id_reads
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_guard_runs_over_the_ipc_transport_too() {
    let node = FakeIpcNode::serving(8453);

    let Err(refused) = AlloyProvider::for_chain(&node.uri(), 1, 1).await else {
        panic!("an IPC endpoint bound for chain 1 that serves 8453 never binds");
    };

    let message = refused.to_string();
    assert!(
        message.contains('1') && message.contains("8453"),
        "the IPC refusal names both chains too: {message}"
    );

    let bound = AlloyProvider::for_chain(&node.uri(), 8453, 1)
        .await
        .expect("an IPC endpoint that serves the bound chain binds");
    assert_eq!(bound.verified_chain_id(), Some(8453));
    assert_eq!(bound.get_chain_id().await.expect("cached read"), 8453);
    assert_eq!(
        node.served_chain_id_reads(),
        2,
        "one read per binding (the refused one and the bound one), none for the hot-path read"
    );
}
