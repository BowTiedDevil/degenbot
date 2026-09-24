//! Canonical deployment identity for one executable strategy session.

use alloy::primitives::{address, Address};

/// Ethereum mainnet WETH, the session seed and settlement currency.
pub const ETHEREUM_WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

/// Ethereum mainnet Uniswap V4 `PoolManager`, authoritative for V4 composition,
/// frame descriptors, and simulation overrides in the canonical session.
pub const ETHEREUM_V4_POOL_MANAGER: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");

/// Deployment facts shared by every executable surface in one strategy session.
///
/// Build this once at boot. The command adapter, per-block frame simulation,
/// and V4 descriptor projection consume this same value rather than rebuilding
/// deployment identities at their call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionContext {
    executor: Address,
    pool_manager: Address,
    weth: Address,
}

impl ExecutionContext {
    /// Build an explicit session deployment identity.
    #[must_use]
    pub const fn new(executor: Address, pool_manager: Address, weth: Address) -> Self {
        Self {
            executor,
            pool_manager,
            weth,
        }
    }

    /// Build the canonical Ethereum mainnet deployment for `executor`.
    #[must_use]
    pub const fn ethereum(executor: Address) -> Self {
        Self::new(executor, ETHEREUM_V4_POOL_MANAGER, ETHEREUM_WETH)
    }

    /// The `cmd_executor` contract encoded calls target.
    #[must_use]
    pub const fn executor(self) -> Address {
        self.executor
    }

    /// The authoritative Uniswap V4 `PoolManager` for this session.
    #[must_use]
    pub const fn pool_manager(self) -> Address {
        self.pool_manager
    }

    /// The session's WETH seed and settlement currency.
    #[must_use]
    pub const fn weth(self) -> Address {
        self.weth
    }
}
