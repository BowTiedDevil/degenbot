// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";

/// Port of `tests/executor/test_withdraw_reentrancy.py`.
contract WithdrawReentrancyTest is PoolSetup {
    uint256 internal constant WITHDRAW_WETH = 10 ether;
    address internal constant HONEST_DEST = address(0xD5);

    function _attackerDest() internal returns (address) {
        return _deploy("ReenteringDestination", abi.encode(address(executor), WITHDRAW_WETH));
    }

    function testWithdrawReentryReverts() public {
        weth.mint(address(executor), WITHDRAW_WETH);
        address dest = _attackerDest();
        uint256 ownerWethBefore = weth.balanceOf(address(this));

        assertEq(weth.balanceOf(address(executor)), WITHDRAW_WETH, "executor funded with WETH only");

        (bool ok,) = address(executor).call(
            abi.encodeWithSignature("withdraw(uint256,address)", WITHDRAW_WETH, dest)
        );
        assertFalse(ok, "reentrant withdraw must revert the whole transaction");

        assertEq(weth.balanceOf(address(executor)), WITHDRAW_WETH, "executor WETH untouched");
        assertEq(weth.balanceOf(dest), 0, "attacker received nothing");
        assertEq(weth.balanceOf(address(this)), ownerWethBefore, "owner WETH untouched");
    }

    function testWithdrawHonestPathUnaffected() public {
        weth.mint(address(executor), WITHDRAW_WETH);
        uint256 balBefore = HONEST_DEST.balance;

        executor.withdraw(WITHDRAW_WETH, HONEST_DEST);

        assertLe(weth.balanceOf(address(executor)), 1, "only the initialize-wei dust may remain");
        assertEq(HONEST_DEST.balance, balBefore + WITHDRAW_WETH, "honest destination paid in full");
    }
}
