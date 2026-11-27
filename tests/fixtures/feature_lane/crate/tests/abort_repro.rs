//! Abort-by-design fixture: the child aborts, so the binary must not batch-run.

#![cfg(feature = "obs")]

#[test]
#[ignore = "runs a subprocess that aborts by design"]
fn aborts_by_design() {}

#[test]
fn child_leaks_guard() {}
