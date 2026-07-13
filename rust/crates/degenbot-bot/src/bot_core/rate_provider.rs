//! Rate-provider seam for Balancer V2 yield-bearing pools.
//!
//! **Relocated.** The `BalancerRateProvider` trait + `RateProviderError` + the
//! pure-Rust `StaticRateProvider` impl now live in `degenbot-pools` (the trait
//! is held as an `Option<Arc<dyn BalancerRateProvider>>` on
//! `BalancerStablePoolState`) and the value-only tests moved with them.
//! Re-exported here at the historical path so the bot's consumers resolve
//! unchanged. The RPC *implementations* + the `PyBalancerRateProvider` `PyO3`
//! adapter still live in `degenbot-bot` / `degenbot-python`.
//!
//! Transient re-export — the shim file is removed and consumers are repointed
//! at `degenbot_pools::` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::{BalancerRateProvider, RateProviderError, StaticRateProvider};
