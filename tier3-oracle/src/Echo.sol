// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/// Echo harness for the Tier-3 forge→revm smoke (ergo task 767HYN).
///
/// Proves the full toolchain loop with zero pool math: a pure function
/// `double(uint256)` that returns `2 * x`. The Rust smoke test deploys this
/// contract's runtime bytecode into an offline revm `CacheDB<EmptyDB>`,
/// calls `double(21)`, and asserts the revm result is `42`. Success means
/// `forge build` → bytecode load → revm transact → arg marshalling → assert
/// all work, which is every prerequisite the real oracle tiers need.
contract Echo {
    /// Returns `2 * x`. Selector: `double(uint256)` = `keccak256("double(uint256)")`.
    function double(uint256 x) external pure returns (uint256) {
        return 2 * x;
    }
}
