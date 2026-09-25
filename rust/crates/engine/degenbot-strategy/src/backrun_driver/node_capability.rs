//! The node capability a driver module declares, injected at construction
//! instead of assumed by a client builder.
//!
//! Two kinds of consumer in this driver need an endpoint: the node join every
//! downstream artifact reads through, and the bundle sim. Neither is entitled
//! to choose its transport. The boot resolves WHICH endpoint (the chain, then
//! the scope) from the operator layers; the caller injects WHO dials it. That
//! split is what lets a `nodes.ipc` entry — a local node over a unix socket —
//! reach a driver, and it puts the transport decision at the call site where a
//! reader can see it instead of inside a `ClientBuilder` buried in a
//! constructor.
//!
//! The seam is the same one the pump's ingestor uses: a caller hands over a
//! ready dialer ([`NodeCapability`]) and this module asks it for endpoints.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};

/// The provider a capability builds for one endpoint, or the reason it could
/// not. The reason is the transport's own message (an unsupported scheme, a
/// socket that would not open); each caller wraps it in the typed refusal that
/// names its own endpoint.
pub type RequestProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Arc<AlloyProvider>, String>> + Send + 'a>>;

/// The capability this module needs from its node, injected at construction.
///
/// A capability is not a transport name. It is the answer to "this module
/// makes REQUESTS against whatever endpoint the boot resolved, and here is who
/// dials it" — the same shape the pump's `WsIngestor::with_provider` takes,
/// one step earlier: a ready dialer rather than a client this module builds.
///
/// One provider serves every request-scope consumer in this module: the pool
/// reads, the tick bootstrap, the sample verifier, the bundle sim's
/// `eth_callMany` and the gap-boundary probe. A consumer that needs a raw
/// method speaks it through [`AlloyProvider::make_request`], so no consumer
/// can end up on a different transport than the node it was handed.
///
/// `chain_id` is the chain the boot resolved for the endpoint. An
/// implementation that can verify cheaply may; one that dials lazily may
/// ignore it and let `resolve_backrun_boot` bind the provider once, as the
/// production scope below does.
pub trait NodeCapability: Send + Sync {
    /// The request-scope provider for `endpoint`: a transport that can answer
    /// pool reads, `eth_callMany` and submission, over whichever transport
    /// `endpoint` names.
    fn request_provider(&self, endpoint: &str, chain_id: u64) -> RequestProviderFuture<'_>;
}

/// The scope that takes whichever request transport the resolved endpoint
/// names: `http(s)://`, `ws(s)://`, or `ipc://` / a socket path. Scheme
/// detection, the WS message-size caps and the IPC connect retries all live in
/// [`AlloyProvider::new`], so this scope adds no transport logic of its own.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnyRequestTransport;

impl NodeCapability for AnyRequestTransport {
    fn request_provider(&self, endpoint: &str, _chain_id: u64) -> RequestProviderFuture<'_> {
        let endpoint = endpoint.to_string();
        Box::pin(async move {
            AlloyProvider::new(&endpoint, DEFAULT_MAX_RETRIES)
                .await
                .map(Arc::new)
                .map_err(|error| error.to_string())
        })
    }
}
