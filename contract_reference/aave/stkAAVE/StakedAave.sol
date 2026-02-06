// SPDX-License-Identifier: agpl-3.0
pragma solidity 0.6.12;
pragma experimental ABIEncoderV2;

import {IERC20} from '../interfaces/IERC20.sol';
import {StakedToken} from './StakedToken.sol';

/**
 * @title StakedAave
 * @notice StakedToken with AAVE token as staked token
 * @author Aave
 **/
contract StakedAave is StakedToken {
  string internal constant NAME = 'Staked Aave';
  string internal constant SYMBOL = 'stkAAVE';
  uint8 internal constant DECIMALS = 18;
  
  constructor(
    IERC20 stakedToken,
    IERC20 rewardToken,
    uint256 cooldownSeconds,
    uint256 unstakeWindow,
    address rewardsVault,
    address emissionManager,
    uint128 distributionDuration
  ) public StakedToken(
    stakedToken,
    rewardToken,
    cooldownSeconds,
    unstakeWindow,
    rewardsVault,
    emissionManager,
    distributionDuration,
    NAME,
    SYMBOL,
    DECIMALS) {}
}
