// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";

/// Port of `tests/executor/test_donation_profit_inflation.py`.
contract DonationProfitInflationTest is PoolSetup {
    uint256 internal constant DONATION_WETH = 10 ether;
    uint256 internal constant DONATION = 5 ether;
    address internal constant ATTACKER = address(0xA7);

    function testPlainEthDonationReverts() public {
        weth.mint(address(executor), DONATION_WETH);
        uint256 preBalance = weth.balanceOf(address(executor)) + address(executor).balance;

        vm.deal(ATTACKER, DONATION + 0.01 ether);
        vm.prank(ATTACKER);
        (bool ok, bytes memory ret) = address(executor).call{value: DONATION}("");
        // The __default__ gate rejects any non-PM, non-WETH sender: the
        // donation is an Unauthorized(caller) revert, not a silent accept.
        _assertRevertData(
            Outcome(ok, ret, 0, 0, 0),
            _executorRevert(ExecutorErrors.UNAUTHORIZED, ATTACKER),
            "plain ETH donation"
        );

        assertEq(
            weth.balanceOf(address(executor)) + address(executor).balance,
            preBalance,
            "executor balance untainted by the rejected donation"
        );
    }
}
