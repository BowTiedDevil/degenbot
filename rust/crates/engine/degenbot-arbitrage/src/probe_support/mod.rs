//! Product-owned development support shared by the arbitrage dispatch probes
//! and their behavior pins: the scripted dispatch chain the dispatch suites and
//! the scaling probe drive, and the controlled same-head dispatch-scaling
//! benchmark over it.
//!
//! Every module here is fixture surface: the numbers a probe reports and the
//! facts a pin asserts are computed by this ONE implementation, so a probe and
//! its pins cannot drift apart. The dependency arrow is consumers -> this
//! module (the crate's own integration tests and the engine-lab probes),
//! never the reverse.
//!
//! Not a production API: `#[doc(hidden)]` with no stability promise. It is
//! compiled into the library rather than gated behind `cfg(test)` so the
//! integration-test binaries and the engine-lab tool package share the same
//! support.
// The scripted chain and its assertions fail loudly on a broken fixture.
#![expect(
    clippy::expect_used,
    reason = "the scripted chain and its assertions fail loudly on a broken fixture"
)]

pub mod dispatch_probe;
pub mod dispatch_scaling;
