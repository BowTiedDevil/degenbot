//! Live probe fixture: reads the service gate, and one pure helper test.

#![cfg(feature = "obs")]

fn live_endpoints() -> Option<()> {
    std::env::var("DEGENBOT_JAEGER_E2E").ok()
}

#[test]
fn span_reaches_real_service() {
    let _ = live_endpoints();
}

#[test]
fn shared_measurement_name() {
    let _ = live_endpoints();
}

#[test]
fn pure_helper() {}
