// SPDX-License-Identifier: agpl-3.0
pragma solidity 0.6.12;

interface ITransferHook {
    function onTransfer(address from, address to, uint256 amount) external;
}