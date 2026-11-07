//! The account-query cluster on the `Bot` facade: the read-through over the
//! session's account-value caches.
//!
//! One choreography serves every account query — Bot account queries, the
//! Python `Erc20Builder` entries, and pure-Rust callers: resolve the typed
//! block selector to the concrete block the query is keyed by, answer from
//! [`BotState::accounts`](crate::BotState::accounts) when the stream already
//! holds that block, otherwise run the stateless read
//! (`degenbot_rpc::erc20`, or the native `eth_getBalance` for the conceptual
//! native token) and cache-fill the stream. The caches are session live
//! state (depth-8 per-block FIFO per stream); this module owns the
//! read-through, and nothing here invents a value a read did not produce.
//!
//! The block selector is resolved FIRST and the resolved number is both the
//! cache key and the read's block: a pinned number pins everything, and the
//! canonical tags resolve with correct semantics (`earliest` → 0, `latest` →
//! the head, `pending` → head + 1) against the construction I/O's RPC clock.

use alloy::primitives::{Address, U256};

use degenbot_rpc::erc20::AccountReadError;
use degenbot_substrate::session_registry::TokenIdentity;
use degenbot_substrate::state_lock::LockSite;

use super::bot::Bot;
use super::construction_io::{BlockSelector, ConstructionIo};

/// Why one account query produced no value.
///
/// The consumer's policy stays the consumer's: what a failure MEANS (Aave's
/// zero-intolerance, a verification read's absence-as-zero) is decided above
/// this type, which only names the failure.
#[derive(Debug, thiserror::Error)]
pub enum AccountQueryError {
    /// The session has no construction I/O attached, and the query needs one
    /// (a fetch, or a tag that resolves against the RPC clock).
    #[error("no construction io is attached to this session")]
    MissingConstructionIo,
    /// The stateless account read failed (typed, verbatim).
    #[error(transparent)]
    Read(#[from] AccountReadError),
    /// The native asset has no equivalent of this query: it is not an ERC-20
    /// contract, so `allowance` / `totalSupply` do not exist for it.
    #[error("the native asset has no {query} query")]
    NoNativeQuery {
        /// The query that has no native equivalent.
        query: &'static str,
    },
}

impl Bot {
    /// The account-value read-through: `token`'s balance of `owner`.
    ///
    /// Cache hit at the resolved block answers without any I/O; a miss runs
    /// the stateless read and fills the stream. The conceptual native token
    /// routes to `eth_getBalance` (its balance IS the account's native
    /// balance); every other identity is an ERC-20 `balanceOf`.
    ///
    /// # Errors
    ///
    /// [`AccountQueryError`] — see the variant docs.
    pub fn token_balance(
        &self,
        token: &TokenIdentity,
        owner: Address,
        selector: Option<BlockSelector>,
    ) -> Result<U256, AccountQueryError> {
        let block = self.resolve_account_block(selector)?;
        if let Some(cached) = self
            .state_arc()
            .read_at(LockSite::Orchestrator)
            .accounts()
            .balance(&token.key(), owner, block)
        {
            return Ok(cached);
        }
        let io = self.account_io()?;
        let value = degenbot_core::runtime::get_runtime().block_on(async {
            match token.address() {
                None => io
                    .get_balance(owner, Some(block))
                    .await
                    .map_err(|error| AccountQueryError::Read(AccountReadError::Call(error))),
                Some(address) => {
                    degenbot_rpc::erc20::balance_of(io.as_ref(), address, owner, Some(block))
                        .await
                        .map_err(AccountQueryError::Read)
                }
            }
        })?;
        self.state_arc()
            .write_at(LockSite::Orchestrator)
            .accounts()
            .store_balance(&token.key(), owner, block, value);
        Ok(value)
    }

    /// The account-value read-through: `token`'s allowance from `owner` to
    /// `spender`. The conceptual native asset has no allowances.
    ///
    /// # Errors
    ///
    /// [`AccountQueryError`] — see the variant docs.
    pub fn token_allowance(
        &self,
        token: &TokenIdentity,
        owner: Address,
        spender: Address,
        selector: Option<BlockSelector>,
    ) -> Result<U256, AccountQueryError> {
        let block = self.resolve_account_block(selector)?;
        if let Some(cached) = self
            .state_arc()
            .read_at(LockSite::Orchestrator)
            .accounts()
            .allowance(&token.key(), owner, spender, block)
        {
            return Ok(cached);
        }
        let io = self.account_io()?;
        let address = token
            .address()
            .ok_or(AccountQueryError::NoNativeQuery { query: "allowance" })?;
        let value = degenbot_core::runtime::get_runtime().block_on(
            degenbot_rpc::erc20::allowance(io.as_ref(), address, owner, spender, Some(block)),
        )?;
        self.state_arc()
            .write_at(LockSite::Orchestrator)
            .accounts()
            .store_allowance(&token.key(), owner, spender, block, value);
        Ok(value)
    }

    /// The account-value read-through: `token`'s total supply. The
    /// conceptual native asset has no total supply.
    ///
    /// # Errors
    ///
    /// [`AccountQueryError`] — see the variant docs.
    pub fn token_total_supply(
        &self,
        token: &TokenIdentity,
        selector: Option<BlockSelector>,
    ) -> Result<U256, AccountQueryError> {
        let block = self.resolve_account_block(selector)?;
        if let Some(cached) = self
            .state_arc()
            .read_at(LockSite::Orchestrator)
            .accounts()
            .total_supply(&token.key(), block)
        {
            return Ok(cached);
        }
        let io = self.account_io()?;
        let address = token.address().ok_or(AccountQueryError::NoNativeQuery {
            query: "totalSupply",
        })?;
        let value = degenbot_core::runtime::get_runtime().block_on(
            degenbot_rpc::erc20::total_supply(io.as_ref(), address, Some(block)),
        )?;
        self.state_arc()
            .write_at(LockSite::Orchestrator)
            .accounts()
            .store_total_supply(&token.key(), block, value);
        Ok(value)
    }

    /// Resolve one typed block selector to the concrete block an account
    /// query is keyed by: a pinned number is that number, `earliest` is 0,
    /// and `latest` (also the absent selector) / `pending` resolve against
    /// the RPC clock. The resolved number is the cache key and the read's
    /// block — one answer, one key.
    fn resolve_account_block(
        &self,
        selector: Option<BlockSelector>,
    ) -> Result<u64, AccountQueryError> {
        match selector.map_or(
            alloy::eips::BlockNumberOrTag::Latest,
            BlockSelector::as_number_or_tag,
        ) {
            alloy::eips::BlockNumberOrTag::Number(block) => Ok(block),
            alloy::eips::BlockNumberOrTag::Earliest => Ok(0),
            other => {
                let io = self.account_io()?;
                degenbot_core::runtime::get_runtime()
                    .block_on(io.resolve_block_selector(BlockSelector::tag(other)))
                    .map_err(|error| AccountQueryError::Read(AccountReadError::Call(error)))
            }
        }
    }

    /// The construction I/O an account query needs, or the typed no-io
    /// refusal (the same shape the build cluster answers with).
    fn account_io(&self) -> Result<std::sync::Arc<ConstructionIo>, AccountQueryError> {
        self.construction_io_arc()
            .ok_or(AccountQueryError::MissingConstructionIo)
    }
}
