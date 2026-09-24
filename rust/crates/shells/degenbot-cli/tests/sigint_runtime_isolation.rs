//! The SIGINT policy must not boot the shared bot runtime.
//!
//! A one-shot console command owns only its argv -> render path; the
//! cgroup-sized driver runtimes (`io_runtime_workers`, solve bins) are a
//! bot-process resource. The listener parks on its own single-thread
//! surface instead. Runs as its own integration-test process so the
//! process-global `OnceLock` runtime state cannot be pre-initialized by
//! sibling tests.

#[test]
fn sigint_install_does_not_boot_the_shared_bot_runtime() {
    let cancel = degenbot_cli_core::CancelHandle::new();
    let guard = degenbot_cli::signal::install(cancel);
    let snapshot = degenbot_core::worker_census::snapshot();
    drop(guard);
    assert!(
        !snapshot.iter().any(|e| e.resource == "io_runtime_workers"),
        "SIGINT listener booted the shared bot runtime; the console must stay off the driver's runtimes"
    );
}
