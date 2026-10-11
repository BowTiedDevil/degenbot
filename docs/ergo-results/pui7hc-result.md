# PUI7HC result — pytest exception-spray containment (Python side)

## What shipped

- `src/degenbot/logging.py`: `_STDOUT_HANDLER` is now `_LiveStdoutStreamHandler` — resolves `sys.stdout` at emit time (stdlib `_StderrHandler` pattern), closed-stream fallback to the process original, single-writer/queue design untouched. `RUST_BRIDGE_LOGGER_NAMES` gains the third-party `alloy` umbrella so alloy WARN/ERROR records take the queued, capture-safe path instead of raw `lastResort` stderr.
- `tests/test_rust_logging_bridge.py`: two guard tests pin late-binding and the closed-stream fallback; the drift-guard tuple mirrors the alloy entry.
- `tests/ambient_config.toml`: facet endpoint is a hermetic dead loopback (`ws://127.0.0.1:9`) — the default suite no longer dials the internet from the ambient fleet boot.
- (via the foreign tick-stream commit 0c09b1aa3, attributed in woxt5z-result.md) `tests/conftest.py`: `pytest_sessionfinish` stops the queue listener then the Rust log drainer before capture teardown; `.devcontainer/devcontainer.json`: exposes `DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ENDPOINTS`.

## Verification (PM ladder)

- ruff check src/ examples/ — pass; ruff format --check src/ — pass; ty check src/ — pass.
- just verify-build-fresh — fresh.
- just test-python — 3137 passed, 50 skipped, exit 0; zero `--- Logging error ---` and zero `Reconnection attempt` lines, two consecutive runs (baseline before: 4 + 2).

## Follow-ups (delegated separately)

- HostedSources lifecycle ownership + head-task shutdown (the durable Rust fix for the leaked pubsub retry storm).
- Suite noise tripwire gate.
- Optional: deferred feed/head start (construction dial-free).
