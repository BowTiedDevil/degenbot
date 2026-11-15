//! Product-owned development support shared by the simulation probes and their
//! behavior pins: the scripted chain stub and the modeled chain it answers
//! from, the recorded corpus readers, the read oracle, the session driver, the
//! delta-parity comparison, the head legs, the real-coverage accounting, the
//! wall ledger and its report, the relevance pairing, and the candidate-scaling
//! model.
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
// The fixtures fail loudly on a broken synthetic account and print the legs a
// probe turns on: a pin that cannot read its fixture must not pass silently.
#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::unwrap_used,
    reason = "a fixture fails loudly on a broken synthetic account and prints an operator-enabled debug leg"
)]

pub mod candidate_scaling;
pub mod chain_stub;
pub mod delta_parity;
pub mod head_legs;
pub mod modeled_chain;
pub mod probe_report;
pub mod read_oracle;
pub mod real_coverage;
pub mod recorded_corpus;
pub mod relevance_pairs;
pub mod scripted_head;
pub mod session_drive;
pub mod wall_ledger;
