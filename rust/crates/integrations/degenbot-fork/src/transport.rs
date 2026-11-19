//! The fork's owned IPC transport: one provider frontend, one dedicated
//! runtime, one close.
//!
//! The alloy pubsub service behind the session's provider owns the only code
//! that can dial the child's socket after teardown: it reconnects with backoff
//! when its backend dies, and a reconnect against a socket whose child is gone
//! is the storm this type exists to make impossible. Two properties provide
//! that:
//!
//! - the service and its backend tasks are spawned on the runtime the session
//!   owns, so disposing that runtime cancels them as one unit, and
//! - every reconnect dial passes [`ReconnectGate`], which the session shuts
//!   before it disposes of anything: an attempt that arrives after the close
//!   parks instead of dialling, and a dial admitted just before it is allowed
//!   to finish before the teardown proceeds.
//!
//! Neither property depends on the order the owner's fields drop in, and
//! neither depends on callers refraining from cloning the provider: a clone
//! holds a frontend whose request channel [`ForkTransport::close`] closes, so
//! its calls fail with `backend_gone` while the gate keeps the service from
//! re-dialling.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use alloy::{
    providers::{DynProvider, Provider, ProviderBuilder},
    pubsub::{ConnectionHandle, PubSubConnect},
    transports::{ipc::IpcConnect, TransportError, TransportErrorKind},
};
use degenbot_core::pubsub_runtime::OwnedPubsubRuntime;

/// Admission state, mutated only under the gate's mutex.
#[derive(Debug, Default)]
struct GateState {
    /// Set by [`ReconnectGate::close`]; every later admission is refused.
    closed: bool,
    /// Dials admitted and not yet finished.
    active: usize,
    /// Reconnect attempts the connector has made.
    attempts: usize,
    /// Attempts refused because the gate was shut.
    refused: usize,
}

/// The one decision an alloy pubsub service must not be left to make on its
/// own: whether it may reconnect.
///
/// Open for the session's whole life; shut by [`ForkTransport::close`] before
/// anything is disposed. Admission and the dial's lifetime are one decision
/// under the gate's mutex: [`Self::admit`] hands back a [`ReconnectPermit`]
/// that must be held across the dial, and [`Self::close`] returns only after
/// every admitted permit is released. An attempt that arrives after the close
/// is refused — never admitted, never dialled.
#[derive(Debug)]
pub(crate) struct ReconnectGate {
    state: Mutex<GateState>,
    /// Notified when the last admitted dial releases; the drain
    /// [`Self::close`] waits on.
    drained: Condvar,
}

impl Default for ReconnectGate {
    fn default() -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            drained: Condvar::new(),
        }
    }
}

/// One admitted reconnect dial.
///
/// A shared borrow of the gate, never a mutex guard, so the future driving the
/// dial stays `Send`; releasing it is what lets a waiting
/// [`ReconnectGate::close`] return.
#[derive(Debug)]
#[must_use = "an admitted dial must be held until the dial finishes"]
struct ReconnectPermit<'a> {
    gate: &'a ReconnectGate,
}

impl Drop for ReconnectPermit<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.lock();
        debug_assert!(
            state.active > 0,
            "a permit is released only while its dial is still counted"
        );
        state.active -= 1;
        if state.active == 0 {
            self.gate.drained.notify_all();
        }
    }
}

impl ReconnectGate {
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Admit one reconnect dial, or refuse it once the gate is shut.
    fn admit(&self) -> Option<ReconnectPermit<'_>> {
        let mut state = self.lock();
        state.attempts += 1;
        if state.closed {
            state.refused += 1;
            None
        } else {
            state.active += 1;
            Some(ReconnectPermit { gate: self })
        }
    }

    /// Shut the gate: refuse every later admission, and do not return while an
    /// admitted dial is still in flight.
    ///
    /// The wait is satisfied by the dial's own driver — the session's dedicated
    /// runtime worker, never the closing thread — so a caller that is itself a
    /// task blocks only for a local IPC connect it cannot be driving. The
    /// owner can therefore dispose of that runtime and then the child knowing
    /// no dial is still in flight.
    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        while state.active > 0 {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Whether every reconnect attempt is currently refused.
    #[cfg(test)]
    pub(crate) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// `(attempts, refused)` as the connector saw them.
    ///
    /// The teardown witness: `attempts == refused` means every attempt the
    /// connector observed was refused, so none of them could reach the socket.
    /// It says nothing about dials admitted before the close. Test-only.
    #[cfg(test)]
    pub(crate) fn stats(&self) -> (usize, usize) {
        let state = self.lock();
        (state.attempts, state.refused)
    }
}

/// [`IpcConnect`] behind the session's gate.
///
/// The pubsub service keeps this connector for the session's whole life and
/// calls [`PubSubConnect::try_reconnect`] when its backend dies; that call is
/// the single place a reconnect can reach the socket, so it is where the gate
/// is enforced.
#[derive(Debug)]
struct GatedIpc {
    connect: IpcConnect<String>,
    gate: Arc<ReconnectGate>,
}

impl PubSubConnect for GatedIpc {
    fn is_local(&self) -> bool {
        self.connect.is_local()
    }

    async fn connect(&self) -> Result<ConnectionHandle, TransportError> {
        self.connect.connect().await
    }

    async fn try_reconnect(&self) -> Result<ConnectionHandle, TransportError> {
        let Some(permit) = self.gate.admit() else {
            // The session is closing and its child owns the socket: an attempt
            // from here could only dial a path that is about to disappear.
            // Park; the session's runtime cancels this task.
            return std::future::pending().await;
        };
        let dialled = self.connect.connect().await;
        drop(permit);
        dialled
    }
}

/// The fork's IPC transport session.
///
/// Owns everything that can talk to the child's socket — the provider
/// frontend, the dedicated runtime its pubsub service runs on, and the gate
/// that decides whether that service may dial again — so [`Self::close`] is the
/// single operation that ends the transport.
pub(crate) struct ForkTransport {
    /// The provider frontend [`crate::AnvilFork::provider`] lends out. `None`
    /// once closed.
    provider: Option<DynProvider>,
    /// The dedicated single-worker runtime hosting the provider's pubsub
    /// service and IPC backend tasks. `None` once closed. Disposing it through
    /// [`OwnedPubsubRuntime`] is what makes the teardown bounded off and inside
    /// an async context.
    runtime: Option<OwnedPubsubRuntime>,
    /// Shut first at close; every later reconnect attempt parks.
    gate: Arc<ReconnectGate>,
}

impl ForkTransport {
    /// Connect a provider to `ipc_path`, spawning its pubsub service on
    /// `runtime` — the worker that hosts it for the session's whole life.
    ///
    /// The connect runs inside `runtime`: a nested `block_on` here would panic,
    /// because the caller is itself async and a task spawned onto another
    /// runtime keeps the service off the caller's runtime.
    ///
    /// # Errors
    ///
    /// The IPC connect failed, or the connect task aborted without delivering a
    /// result.
    pub(crate) async fn open(
        runtime: OwnedPubsubRuntime,
        ipc_path: String,
    ) -> Result<Self, TransportError> {
        let gate = Arc::new(ReconnectGate::default());
        let connector = GatedIpc {
            connect: IpcConnect::new(ipc_path),
            gate: Arc::clone(&gate),
        };
        let (connected_tx, connected_rx) =
            tokio::sync::oneshot::channel::<Result<DynProvider, TransportError>>();
        runtime.spawn(async move {
            let connected = ProviderBuilder::default()
                .connect_pubsub_with(connector)
                .await
                .map(Provider::erased);
            let _ = connected_tx.send(connected);
        });
        let provider = match connected_rx.await {
            Ok(Ok(provider)) => provider,
            Ok(Err(source)) => return Err(source),
            Err(_) => return Err(TransportErrorKind::custom_str("IPC connect task aborted")),
        };
        Ok(Self {
            provider: Some(provider),
            runtime: Some(runtime),
            gate,
        })
    }

    /// The session's provider frontend, or `None` once it is closed.
    pub(crate) fn provider(&self) -> Option<&DynProvider> {
        self.provider.as_ref()
    }

    /// End the session. Idempotent, and safe inside and outside an async
    /// context.
    ///
    /// 1. Shut the reconnect gate: an attempt the pubsub service makes from
    ///    here on parks instead of dialling the socket, and a dial already
    ///    admitted is awaited by the shutdown before it proceeds.
    /// 2. Drop the provider frontend, which closes the pubsub request channel
    ///    and wakes the service.
    /// 3. Drop the dedicated runtime, which cancels the service and its backend
    ///    tasks.
    pub(crate) fn close(&mut self) {
        self.gate.close();
        self.provider.take();
        drop(self.runtime.take());
    }

    /// The reconnect gate, for witnesses that outlive the session. Test-only.
    #[cfg(test)]
    pub(crate) fn gate(&self) -> Arc<ReconnectGate> {
        Arc::clone(&self.gate)
    }
}

impl Drop for ForkTransport {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use std::future::Future as _;

    /// An IPC path no node ever bound.
    fn absent_ipc_path() -> String {
        std::env::temp_dir()
            .join("degenbot-fork-absent-transport.ipc")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn gate_admits_until_closed_then_refuses() {
        let gate = ReconnectGate::default();

        let permit = gate.admit().expect("an open gate admits");
        assert!(!gate.is_closed());
        assert_eq!(gate.stats(), (1, 0));
        drop(permit);

        gate.close();
        gate.close();

        assert!(gate.is_closed(), "close is idempotent");
        assert!(gate.admit().is_none(), "a closed gate refuses");
        assert!(gate.admit().is_none(), "a closed gate keeps refusing");
        assert_eq!(gate.stats(), (3, 2));
    }

    /// A dial admitted before the close finishes before the close returns, so
    /// the owner can dispose of the runtime and the child without a dial
    /// sneaking through behind it. Deterministic: the closer thread cannot
    /// pass the gate while this thread holds the only permit, and it must pass
    /// once that permit is released.
    #[test]
    fn close_waits_for_an_admitted_dial() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let gate = ReconnectGate::default();
        let permit = gate.admit().expect("an open gate admits");
        let (closed_tx, closed_rx) = mpsc::channel::<()>();

        std::thread::scope(|scope| {
            let _closer = scope.spawn(|| {
                gate.close();
                let _ = closed_tx.send(());
            });

            // The closer has set the gate shut once this observes it; with one
            // admitted permit still live it must be parked in the drain wait.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !gate.is_closed() {
                assert!(Instant::now() < deadline, "the closer never shut the gate");
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                closed_rx.try_recv().is_err(),
                "close returned while an admitted dial was still in flight"
            );

            drop(permit);

            closed_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("close must complete once the admitted dial released");
        });
    }

    /// Once the gate is shut, an attempt never reaches the filesystem: it parks
    /// on the connector's reconnect path, where the session's runtime cancels
    /// it. That is the whole "no reconnect into a dead socket" guarantee, and
    /// it holds without observing a single log line.
    #[tokio::test]
    async fn a_closed_gate_parks_every_reconnect_attempt() {
        let gate = Arc::new(ReconnectGate::default());
        let gated = GatedIpc {
            connect: IpcConnect::new(absent_ipc_path()),
            gate: Arc::clone(&gate),
        };

        // Open: the attempt reaches the filesystem and reports its failure.
        assert!(
            gated.try_reconnect().await.is_err(),
            "an absent socket must report a connect failure"
        );
        assert_eq!(gate.stats(), (1, 0));

        gate.close();

        let mut attempt = Box::pin(gated.try_reconnect());
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            attempt.as_mut().poll(&mut cx).is_pending(),
            "a refused attempt must park, never dial"
        );
        assert_eq!(gate.stats(), (2, 1));
    }
}
