// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {TestBase} from "./TestBase.sol";

/// Reusable tracked-account conservation verification: the composed form of
/// `test_conservation.py`'s `_verify_conservation`, built from the `TestBase`
/// snapshot/diff primitives (`_assertTokenConserved`, `_assertWethEthConserved`,
/// `_assertExecutorProfit`).
///
/// Semantics match the source helper exactly:
/// - `tokens[0]` is WETH. WETH and NATIVE are owned by the combined WETH+ETH
///   check (the mode-1 profit domain) and are never summed alone.
/// - Every other tracked token must sum to zero across the tracked account
///   set — a nonzero sum means tokens entered or left the set (a leak).
/// - When `checkExecutorProfit` is set, the executor's combined WETH+ETH
///   delta must equal `expectedWethEth` exactly; the profit check runs
///   first, matching the source ordering.
abstract contract ConservationOps is TestBase {
    function _verifyConservation(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        address executorAccount,
        bool checkExecutorProfit,
        int256 expectedWethEth,
        string memory label
    )
        internal
        pure
    {
        if (checkExecutorProfit) {
            _assertExecutorProfit(
                beforeSnap,
                afterSnap,
                0,
                _accountIdx(beforeSnap, executorAccount),
                expectedWethEth,
                label
            );
        }
        for (uint256 i = 1; i < beforeSnap.tokens.length; i++) {
            if (beforeSnap.tokens[i] == NATIVE) continue; // owned by the combined check
            _assertTokenConserved(beforeSnap, afterSnap, i, label);
        }
        _assertWethEthConserved(beforeSnap, afterSnap, 0, label);
    }

    function _accountIdx(BalanceSnapshot memory s, address account)
        private
        pure
        returns (uint256)
    {
        for (uint256 j = 0; j < s.accounts.length; j++) {
            if (s.accounts[j] == account) return j;
        }
        revert(string.concat("account not tracked in snapshot: ", vmToString(uint160(account))));
    }
}
