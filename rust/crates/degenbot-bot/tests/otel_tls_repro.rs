// TLS destructor-order repro for soak-2026-08-22 v6.
//
// Mechanism (symbolized from /tmp/soak2/bot5.log faulthandler + addr2line):
// 1. Thread exits; libc __call_tls_dtors runs Rust TLS dtors.
// 2. tracing_opentelemetry GUARD_STACK (IdValueStack<ContextGuard>) drops.
// 3. ContextGuard::drop (opentelemetry context.rs:489) touches opentelemetry's
//    OWN thread-local - already destroyed -> AccessError panic.
// 4. Panic hook logs a tracing event -> fmt layer TLS also destroyed ->
//    panic-while-processing-panic -> abort.
//
// Any thread that dies with a leftover OTel ContextGuard on GUARD_STACK aborts.
// This test proves the mechanism in isolation (child-process abort), so the
// fix can be verified without a full soak.

use std::process::Command;

/// Spawn a child that enters an OTel-context span, leaks the guard, and exits.
/// If the destructor-ordering hazard is present, the child aborts.
#[test]
#[ignore = "runs a subprocess that aborts by design; invoke directly when bisecting"]
fn leaked_otel_guard_on_thread_exit_aborts() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let bin = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "test",
            "--test",
            "otel_tls_repro",
            "child_leaks_guard",
            "--",
            "--exact",
            "--nocapture",
        ])
        .current_dir(manifest)
        .env("RUST_BACKTRACE", "1")
        .status()
        .expect("spawn child");
    // The child is EXPECTED to abort (non-zero exit). If it exits cleanly,
    // either the hazard is fixed or the mechanism did not reproduce.
    assert!(
        !bin.success(),
        "child exited cleanly: leftover-guard TLS abort did NOT reproduce"
    );
}

/// The child: enter a span under the OTel layer, deliberately leak the guard,
/// then exit the thread. Expected: AccessError in TLS dtor -> abort.
#[test]
fn child_leaks_guard() {
    use degenbot_bot::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    let (provider, tracer) = otel::provider_with_exporter(InMemorySpanExporter::default());
    let layer = otel::layer(tracer);
    use tracing_subscriber::layer::SubscriberExt;
    // Bare registry + OTel layer only - no fmt layer, so the repro isolates
    // GUARD_STACK's dtor from the fmt-layer TLS (both died in v6; one variable
    // at a time). Local default, not global: the child is a dedicated process.
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::set_global_default(subscriber)
        .expect("child process owns its global subscriber");

    let handle = std::thread::spawn(|| {
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        // Match v6 worker shape: the OTel CONTEXT TLS initializes BEFORE
        // GUARD_STACK when the thread's first OTel touch is an attach
        // (e.g. set_parent/Context::current paths), reversing the dtor order
        // so GUARD_STACK's dtor drops guards into an already-dead context.
        let outer = tracing::info_span!("repro.outer");
        let _detached = outer.set_parent(opentelemetry::context::Context::new());
        let _o = outer.enter();
        let inner = tracing::info_span!("repro.inner");
        let _i = inner.enter();
        tracing::info!("repro event inside inner");
        // Simulate the imbalance: thread exits with BOTH guards alive.
        std::mem::forget(_o);
        std::mem::forget(_i);
        // Thread exits here -> TLS dtors run -> expected AccessError -> abort.
    });
    let _ = handle.join(); // join Err expected if the thread aborted mid-run
                           // Give the dying thread a beat; then this test process continues.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = provider.force_flush();
}
