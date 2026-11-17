// SPDX-License-Identifier: MIT
// Generated from artifacts/FakeErc20.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IFakeErc20 {
    event Transfer(address indexed sender, address indexed receiver, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    function transfer(address _to, uint256 _value) external returns (bool r0);
    function transferFrom(address _from, address _to, uint256 _value) external returns (bool r0);
    function approve(address _spender, uint256 _value) external returns (bool r0);
    function mint(address _to, uint256 _value) external;
    function burn(uint256 _value) external;
    function burnFrom(address _to, uint256 _value) external;
    function name() external view returns (string memory r0);
    function symbol() external view returns (string memory r0);
    function decimals() external view returns (uint8 r0);
    function totalSupply() external view returns (uint256 r0);
    function balanceOf(address arg0) external view returns (uint256 r0);
    function allowance(address arg0, address arg1) external view returns (uint256 r0);
}
