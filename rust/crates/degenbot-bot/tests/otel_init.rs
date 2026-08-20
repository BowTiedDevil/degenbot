// Integration test: `init_otel_tracing()` contract (epic KDUED5 / DFN6FF).
//
// This file is a dedicated test binary, i.e. its own process: the
// process-global subscriber is guaranteed to be free, so the "second call is
// AlreadySetUp" contract is testable deterministically.
#![cfg(feature = "otel")]

#[test]
fn first_init_ok_second_init_is_already_set_up() {
    // Point the OTLP exporter at a closed port: construction never dials, and
    // any background flush fails fast with connection-refused (no collector is
    // available in the test environment).
    std::env::set_var(
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "http://127.0.0.1:1/v1/traces",
    );

    let first = degenbot_bot::otel::init_otel_tracing();
    assert!(first.is_ok(), "first init should succeed: {first:?}");
    let handle = match &first {
        Ok(handle) => handle,
        Err(_) => return, // unreachable: asserted ok above
    };
    let flushed = handle.flush();
    // Connection refused is an export error, not a flush-contract failure of
    // our code: the call must not panic or hang either way.
    let _ = flushed;

    let second = degenbot_bot::otel::init_otel_tracing();
    assert!(
        matches!(
            second,
            Err(degenbot_bot::otel::OtelInitError::AlreadySetUp(_))
        ),
        "second init must be AlreadySetUp, got: {second:?}"
    );
}
