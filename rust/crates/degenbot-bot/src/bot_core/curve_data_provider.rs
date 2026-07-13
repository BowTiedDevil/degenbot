//! Data-provider seam for Curve `StableSwap` / Crypto pools.
//!
//! **Relocated.** The `CurveDataProvider` trait + `CurveDataProviderError` now
//! live in `degenbot-pools` (the trait is held as an `Option<Arc<dyn
//! CurveDataProvider>>` on `CurvePoolState`) and the value-only `StubProvider`
//! test moved with them. Re-exported here at the historical path so the bot's
//! consumers resolve unchanged. The RPC *implementations* + the
//! `PyCurveDataProvider` `PyO3` adapter still live in `degenbot-bot` /
//! `degenbot-python`.
//!
//! Transient re-export — the shim file is removed and consumers are repointed
//! at `degenbot_pools::` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::{CurveDataProvider, CurveDataProviderError};
