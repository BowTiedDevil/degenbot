// SPDX-License-Identifier: MIT
// Generated from artifacts/ExttloadComparator.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IExttloadComparator {
    function computeSlot(address target, address currency) external view returns (bytes32 r0);
    function nativeDeltaSlot() external view returns (bytes32 r0);
    function compare() external payable returns (bool r0);
    function readSlotsInsideCallback() external payable;
    function unlockCallback(bytes calldata data) external returns (bytes memory r0);
    function readArbitrarySlot(bytes32 slot) external view returns (bytes32 r0, bytes32 r1);
    function real_exttload_value() external view returns (bytes32 r0);
    function fake_exttload_value() external view returns (bytes32 r0);
    function s_real_lock() external view returns (bytes32 r0);
    function s_real_count() external view returns (bytes32 r0);
    function s_real_delta() external view returns (bytes32 r0);
    function s_fake_lock() external view returns (bytes32 r0);
    function s_fake_count() external view returns (bytes32 r0);
    function s_fake_delta() external view returns (bytes32 r0);
}
