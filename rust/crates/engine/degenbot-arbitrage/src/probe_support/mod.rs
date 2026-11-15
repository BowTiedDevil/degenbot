//! Product-owned development support shared by the arbitrage dispatch probes
//! and their behavior pins: the scripted dispatch chain the dispatch suites and
//! the scaling probe drive, and the controlled same-head dispatch-scaling
//! benchmark over it.
//!
//! Every module here is fixture surface: the numbers a probe reports and the
//! facts a pin asserts are computed by this ONE implementation, so a probe and
//! its pins cannot drift apart. The dependency arrow is consumers -> this
//! module (the crate's own examples and integration tests), never the reverse.
//!
//! Not a production API: `#[doc(hidden)]` with no stability promise. It is
//! compiled into the library rather than gated behind `cfg(test)` so the
//! example binaries share the same support.
// The scripted chain and its assertions fail loudly on a broken fixture.
#![expect(
    clippy::expect_used,
    reason = "the scripted chain and its assertions fail loudly on a broken fixture"
)]

pub mod dispatch_probe;
pub mod dispatch_scaling;
