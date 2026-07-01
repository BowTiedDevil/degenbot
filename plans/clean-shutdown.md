# Clean shutdown signal: Python → Rust core

## Goal
Stop the backrun bot promptly on Ctrl-C. Today a `KeyboardInterrupt` tears down
the asyncio loop, but the Rust `BlockPump` task (spawned on the shared tokio
runtime) keeps blocking on `combined.next()` — up to `BACKFILL_TIMEOUT_SECS`
(60s) of inactivity before it re-checks its `shutdown` flag, and indefinitely
if the WS subscription never delivers a final frame. The process hangs in
teardown until the WS subscription closes. Add a clean shutdown path: Python
tells the Rust core to stop the pump before tearing down.

## Context
- `PumpState` (`rust/crates/degenbot-python/src/bot/pump.rs`) already owns
  `shutdown: Arc<AtomicBool>` + `pump_handle: Mutex<Option<JoinHandle<()>>>`.
  `PumpState::subscribe`'s guard even references a `stop()` ("Call stop()
  first.") that does not exist yet.
- The block pump loop checks `self.shutdown` at the top of each iteration but
  only after `timeout(wait_timeout, combined.next()).await` returns (60s
  window when idle) — so the flag alone is too slow.
- Precedent: legacy `PyV2ArbEngine::stop()` "sets the shutdown flag and aborts
  the pump task" (`rust/CONTEXT.md`). Mirror it: set the flag *and* abort the
  task so the await unblocks immediately.

## Work

---
# Add PumpState::stop() (Rust)

Implement `PumpState::stop(&self) -> PyResult<()>`:
1. `self.shutdown.store(true, Ordering::Relaxed)` — signals the cooperative
   path so any in-flight WS visit notices + returns.
2. `self.pump_handle.lock().take()` → `handle.abort()` then `block_on(handle
   .await)` (ignore `JoinError`) — unblocks the `combined.next()` await
   immediately, dropping the WS subscription futures (which closes the
   transport).
3. Clear `subscribe_state` so a stray second `subscribe()` is allowed.
4. Log `[shutdown] BlockPump stopped` so the operator sees it.

Why abort (not just the flag): the pump loop blocks up to 60s on the idle
settle window with no incoming event; aborting the task is the only way to
honour a prompt Ctrl-C. Aborting mid-`on_drain`/`on_send` is safe: those take
internal `parking_lot` locks (non-poisoning); the engine `Mutex` is released
on cancellation and `shutdown` is already set so no new drain ticks matter.

---
# Expose stop() on PyBot + PyUniswapArbEngine (Rust)

- `PyBot::stop` (`bot/mod.rs`): delegate to `self.pump_state()?.stop()`.
- `PyUniswapArbEngine::stop` (`bot/engine/register.rs`, the pump-lifecycle
  slice that already hosts `subscribe`/`resume`): delegate to `self.pump
  .stop()`.
- Idempotent: a second call returns `Ok(())` (the handle is `take()`n on the
  first).

---
# Update degenbot_rs.pyi (Python stubs)

Add `def stop(self) -> None: ...` to both `PyBot` and `UniswapArbEngine` in
`src/degenbot/degenbot_rs.pyi` so type checkers see it.

---
# Wire BackrunSession.shutdown() (Python)

`examples/eth_backrun_v2_v3_v4_rust.py`:
- `BackrunSession.shutdown(self)` — best-effort: call
  `self.engine_registry.engine.stop()` inside `try/except Exception`, then
  cancel `_result_consumer_task`. Keep it defensive: `engine_registry` /
  `engine` may be `None` if shutdown is hit before `start()` finished.
- `__aexit__`: call `await self.shutdown()` (or best-effort sync) before the
  existing task-cancel cleanup so the pump is stopped even on normal exit /
  exceptions, not just Ctrl-C.
- `main()`: guard `async with BackrunSession(cfg) as session: await
  session.run()` with `except KeyboardInterrupt: ... session.shutdown()` and a
  log line, then return (no traceback). This is the user-facing Ctrl-C path:
  the SIGINT aborts `run()`'s `await`s; we then signal stop and exit.

---
# Rust tests

`rust/crates/degenbot-python/src/bot/pump.rs` `#[cfg(test)]`:
- `stop_sets_shutdown_flag`: stop() flips the flag to true.
- `stop_aborts_pump_handle`: after stop, `pump_handle` is `None` (consumed).
- `stop_is_idempotent`: calling twice is `Ok(())` with no panic.

Use the existing `BlockPump::for_test` builder with a faked shutdown flag so
no real WS connection is opened.

---
# Python test

`tests/` (find the engine registry / backrun session test seam): a unit test
that constructs an injected `BackrunSession` (or a fake engine with a `stop`
spy) and asserts `shutdown()` calls `engine.stop()` once and swallows a
raised exception (the "best-effort" contract).

## Acceptance
- Ctrl-C on the running backrun bot exits within ~1s (pump aborted, WS
  closed), printing a single `[shutdown]` line and no traceback.
- `stop()` is idempotent and callable before `resume()` (no pump handle →
  no-op Ok).
- `just test-rust` + `just test-python` green.