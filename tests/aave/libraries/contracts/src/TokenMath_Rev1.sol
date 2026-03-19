// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

import {WadRayMath} from "./WadRayMath.sol";

/**
 * @title TokenMath (Rev 1 Simulated)
 * @notice Simulated TokenMath for Pool revisions 1-3 using half-up rounding.
 * @dev This version maps all TokenMath functions to the base WadRayMath functions
 * which use half-up rounding. This is functionally equivalent to how Rev 1-3
 * pools calculate token amounts before TokenMath was introduced.
 */
library TokenMath {
  using WadRayMath for uint256;

  /**
   * @notice Calculates the scaled amount of aTokens to mint.
   * Uses rayDiv (half-up) since rayDivFloor doesn't exist in Rev 1.
   * @param amount The amount of underlying asset supplied.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The scaled amount of aTokens to mint.
   */
  function getATokenMintScaledAmount(
    uint256 amount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return amount.rayDiv(liquidityIndex);
  }

  /**
   * @notice Calculates the scaled amount of aTokens to burn.
   * Uses rayDiv (half-up) since rayDivCeil doesn't exist in Rev 1.
   * @param amount The amount of underlying asset to withdraw.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The scaled amount of aTokens to burn.
   */
  function getATokenBurnScaledAmount(
    uint256 amount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return amount.rayDiv(liquidityIndex);
  }

  /**
   * @notice Calculates the scaled amount of aTokens to transfer.
   * Uses rayDiv (half-up) since rayDivCeil doesn't exist in Rev 1.
   * @param amount The amount of aTokens to transfer.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The scaled amount of aTokens for transfer.
   */
  function getATokenTransferScaledAmount(
    uint256 amount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return amount.rayDiv(liquidityIndex);
  }

  /**
   * @notice Calculates the actual aToken balance from scaled balance.
   * Uses rayMul (half-up) since rayMulFloor doesn't exist in Rev 1.
   * @param scaledAmount The scaled aToken balance.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The actual aToken balance.
   */
  function getATokenBalance(
    uint256 scaledAmount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return scaledAmount.rayMul(liquidityIndex);
  }

  /**
   * @notice Calculates the scaled amount of vTokens to mint.
   * Uses rayDiv (half-up) since rayDivCeil doesn't exist in Rev 1.
   * @param amount The amount of underlying asset borrowed.
   * @param variableBorrowIndex The current vToken variableBorrowIndex.
   * @return The scaled amount of vTokens to mint.
   */
  function getVTokenMintScaledAmount(
    uint256 amount,
    uint256 variableBorrowIndex
  ) internal pure returns (uint256) {
    return amount.rayDiv(variableBorrowIndex);
  }

  /**
   * @notice Calculates the scaled amount of vTokens to burn.
   * Uses rayDiv (half-up) since rayDivFloor doesn't exist in Rev 1.
   * @param amount The amount of underlying asset corresponding to vTokens to burn.
   * @param variableBorrowIndex The current vToken variableBorrowIndex.
   * @return The scaled amount of vTokens to burn.
   */
  function getVTokenBurnScaledAmount(
    uint256 amount,
    uint256 variableBorrowIndex
  ) internal pure returns (uint256) {
    return amount.rayDiv(variableBorrowIndex);
  }

  /**
   * @notice Calculates the actual vToken balance (debt) from scaled balance.
   * Uses rayMul (half-up) since rayMulCeil doesn't exist in Rev 1.
   * @param scaledAmount The scaled vToken balance.
   * @param variableBorrowIndex The current vToken variableBorrowIndex.
   * @return The actual vToken balance (debt).
   */
  function getVTokenBalance(
    uint256 scaledAmount,
    uint256 variableBorrowIndex
  ) internal pure returns (uint256) {
    return scaledAmount.rayMul(variableBorrowIndex);
  }
}
