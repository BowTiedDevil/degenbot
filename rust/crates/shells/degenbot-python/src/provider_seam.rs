//! The shell's provider seam edge (ADR-068 D5: the provider seam is
//! injection, not a mock server).
//!
//! The core run entries already take an ALREADY-BUILT [`AlloyProvider`] — the
//! `CassetteReplayTransport` replay drives them through the real provider
//! seam, with no socket in existence. The shells keep operator ergonomics
//! through a thin `rpc_url` wrapper that builds the live transport, and this
//! module is that wrapper's one home in the binding layer:
//!
//! - [`live_provider`] is the ONLY live-transport construction site under
//!   `degenbot-python/src`, so every shell seam — the updater chunk loops,
//!   their verify companions, the contract wrappers — reaches the transport
//!   through one construction path, and a test can drive the same seam
//!   entries with a recorded cassette instead of a node.
//! - [`drive`] is the shell's synchronous drive: the seam bodies hand their
//!   futures to the core bridge
//!   ([`degenbot_core::runtime::bridged_block_on`]), which owns the one
//!   in-runtime hand-off — an ambient multi-threaded runtime parks the worker
//!   and drives its own handle (the verify seams, entered by a Python driver
//!   through `call_on_ambient_runtime`), while an off-runtime thread drives
//!   the process-wide shared runtime (the updater entries, which arrive on a
//!   Python worker thread with no runtime context at all).
//! - [`UPDATER_MAX_RETRIES`] is the updater family's ONE retry budget (the
//!   literal that used to sit at every entry).
//! - [`require_ambient_runtime`] is the ONE spelling of the verify seams'
//!   ambient-runtime policy.
//!
//! # Why the verify seams refuse a missing ambient runtime
//!
//! The updater chunk loops block on the shared runtime themselves, so a
//! nested drive would panic — the hazard [`drive`] hands to the core bridge.
//! The verify seams are refused a missing ambient runtime rather than
//! silently driven, because their callers are Python drivers that satisfy the
//! policy explicitly (`tests/test_ambient_runtime.py` pins both faces). One
//! bridge, two consistent policies.

use std::future::Future;

use degenbot_core::errors::ProviderResult;
use degenbot_core::runtime::bridged_block_on;
use degenbot_rpc::provider::AlloyProvider;
use tokio::runtime::Handle;

/// The updater family's transport retry budget: the pool chunk loop, the
/// Aave chunk loop, and their verify/maintenance companions share ONE number.
///
/// The hotpath default is
/// [`degenbot_rpc::provider::DEFAULT_MAX_RETRIES`]; the updaters deliberately
/// override it with this larger budget, since their archive backfills run
/// long fetch windows that a transient endpoint hiccup would otherwise fail.
pub const UPDATER_MAX_RETRIES: u32 = 5;

/// Build the live provider `rpc_url` names — the shell's ONE transport
/// construction call.
///
/// Callers either [`drive`] the returned future (a synchronous seam body) or
/// `.await` it directly inside a task that already runs on the shared
/// runtime.
///
/// # Errors
///
/// Propagates the transport build failure ([`ProviderError`]) unchanged; each
/// seam maps it onto its own typed error (`RunError::Provider` for the
/// updater seams, `PyErr` for the contract wrappers).
///
/// [`ProviderError`]: degenbot_core::errors::ProviderError
pub async fn live_provider(rpc_url: &str, max_retries: u32) -> ProviderResult<AlloyProvider> {
    AlloyProvider::new(rpc_url, max_retries).await
}

/// Drive `fut` to completion from a synchronous seam body — the shell's name
/// for [`bridged_block_on`], the ONE home of the in-runtime panic hazard.
#[must_use]
pub fn drive<F: Future>(fut: F) -> F::Output {
    bridged_block_on(fut)
}

/// The verify seams' runtime policy: the calling thread MUST already stand in
/// an ambient tokio context.
///
/// These seams used to build and drop a full multi-thread runtime per call —
/// its default worker count is the raw core count, so the live process
/// accumulated dead `tokio-rt-worker` threads. They now refuse a missing
/// ambient runtime with the typed error instead of quietly driving the shared
/// runtime, so a Python driver that forgot `call_on_ambient_runtime` fails
/// loudly at the policy line rather than papering over it.
///
/// # Errors
///
/// An [`std::io::Error`] carrying the policy message. Every seam maps it
/// through its own `RunError::Runtime`, which is the `ValueError` face in
/// Python.
pub fn require_ambient_runtime() -> Result<(), std::io::Error> {
    if Handle::try_current().is_ok() {
        return Ok(());
    }
    Err(std::io::Error::other(
        "no ambient tokio runtime: refusing to build a per-call multi-thread runtime; run under the shared degenbot-core ambient runtime",
    ))
}
