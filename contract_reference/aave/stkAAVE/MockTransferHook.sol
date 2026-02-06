// SPDX-License-Identifier: agpl-3.0
pragma solidity 0.6.12;

import {ITransferHook} from "../interfaces/ITransferHook.sol";

contract MockTransferHook is ITransferHook {
    event MockHookEvent();

    function onTransfer(address from, address to, uint256 amount) external override {
        emit MockHookEvent();
    }
}