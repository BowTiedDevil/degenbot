//! Offline twin fixture: reuses a live probe's test name in another binary,
//! so a global name filter would suppress an offline unit.

#![cfg(feature = "obs")]

#[test]
fn shared_measurement_name() {}

#[test]
fn offline_twin_only() {}
