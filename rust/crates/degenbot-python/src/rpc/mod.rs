//! `degenbot-rpc` `PyO3` wrappers (provider/contract/subscription, sync + async)
//! over the pure RPC core. Mirrors `crates/degenbot-rpc/`. (ergo UG6FKN task
//! WXHGOH.)

pub mod contract;
pub mod provider;
pub mod subscription;

#[cfg(feature = "async")]
pub mod async_contract;
#[cfg(feature = "async")]
pub mod async_provider;
