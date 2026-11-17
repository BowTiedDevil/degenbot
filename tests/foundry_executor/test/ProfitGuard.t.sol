// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";

/// Port of `tests/executor/test_profit_guard_m1_m2.py`.
contract ProfitGuardTest is PoolSetup {
    uint256 internal constant PREFUND_WETH = 10 ether;

    function testBribeTooHighBipsReverts() public {
        // The bribe-bips guard sits after the profit checks, so the stream
        // must be non-degenerate: a bare separator reverts in the dispatch
        // loop before any config handling. WETH_DEPOSIT_ALL with no native
        // ETH is a clean no-op that leaves profit at zero.
        weth.mint(address(executor), PREFUND_WETH);
        bytes memory stream = bytes.concat(Cmd.preamble(_emptyTable()), Cmd.wethDepositAll());
        uint256 config = (uint256(10_001) << 8) | 1;
        Outcome memory o = _executeRaw(executor, stream, config);
        _assertRevertData(o, _executorRevert(ExecutorErrors.BIPS_TOO_HIGH, 10_001), "bribe above 100%");
    }

    function testZeroMinProfitIsLegalForNoPrefundExecution() public {
        weth.mint(address(executor), PREFUND_WETH);
        uint256 config = Cmd.makeConfig(1, 0, 0, 0);
        bytes memory commands = bytes.concat(Cmd.preamble(_emptyTable()), Cmd.wethDepositAll());
        _execute(executor, commands, config);
        assertEq(
            weth.balanceOf(address(executor)),
            PREFUND_WETH,
            "no native ETH to wrap: the minted WETH balance is untouched"
        );
    }
}
