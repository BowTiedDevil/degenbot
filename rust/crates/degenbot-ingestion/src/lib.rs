//! `degenbot-ingestion` — the WS transport half of the bot.
//!
//! This crate is the pyo3-free owner of everything that reaches Rust from the
//! chain's event firehose:
//!
//! - WS subscriptions (`newHeads` + unfiltered `logs`), merged into one
//!   [`IngestEvent`] stream (fair interleave; one WS connection).
//! - Rust-side topic/address filtering ([`RELEVANT_TOPICS`]).
//! - Gap backfill FETCH ([`build_backfill_filter`] + chunked `eth_getLogs`
//!   via [`WsIngestor`]). The APPLY half (decoding, state machine, solves)
//!   stays in the runtime (`degenbot-bot::bot_core`), which consumes the
//!   crate's [`IngestEvent`] stream.
//! - The header-staleness / logs-silence watchdog windows ([`Watchdog`])
//!   + the transport timeouts (60s idle/degraded, log-catchup settle).
//!
//! The boundary contract (ADR-005 driver-shell + the MROOY7 extraction):
//! **ingestion emits, the runtime decides.** This crate knows nothing about
//! `BotState`, `StageMachine`, Python, or pyo3 — a standalone Rust consumer
//! subscribes to the emitted event stream (or drives a stage machine itself)
//! with no Python in the build graph, and the `PyO3` layer is just another
//! sink that subscribes at the runtime's Published edge.
//!
//! The primary type is [`WsIngestor`]: connect once, subscribe + handshake,
//! keep the handle for gap-backfill fetching alongside the live stream.

pub mod events;
pub mod filter;
pub mod ingestor;
pub mod topics;
pub mod watchdog;

pub use events::{IngestEvent, PoolEvent};
pub use filter::{backfill_filter, build_backfill_filter};
pub use ingestor::{
    SubscribeBoundary, WsIngestor, BACKFILL_TIMEOUT_SECS, DEFAULT_BACKFILL_CHUNK_SIZE,
    LOG_CATCHUP_SETTLE_SECS,
};
pub use topics::{is_relevant_log, is_relevant_topic, RELEVANT_TOPICS};
pub use watchdog::Watchdog;
