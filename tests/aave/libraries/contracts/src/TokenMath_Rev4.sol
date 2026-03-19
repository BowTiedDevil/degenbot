// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

import {WadRayMath} from "./WadRayMathExtended.sol";

/**
 * @title TokenMath (Rev 4)
 * @author BGD Labs
 * @notice Provides utility functions for calculating scaled amounts and balances for aTokens and vTokens,
 *         applying specific rounding rules (floor/ceil) as per Aave v3.5's rounding improvements.
 *         The rounding behavior of the operations is in line with the ERC-4626 token standard.
 *         In practice, this means rounding in favor of the protocol.
 * @dev Extracted from AToken/rev_4.sol - this is the TokenMath library for Pool revisions 4-8.
 */
library TokenMath {
  using WadRayMath for uint256;

  /**
   * @notice Calculates the scaled amount of aTokens to mint when supplying underlying assets.
   *         The amount is rounded down to ensure the minted aTokens are less than or equal to the supplied amount.
   * @param amount The amount of underlying asset supplied.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The scaled amount of aTokens to mint.
   */
  function getATokenMintScaledAmount(
    uint256 amount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return amount.rayDivFloor(liquidityIndex);
  }

  /**
   * @notice Calculates the scaled amount of aTokens to burn when withdrawing underlying assets.
   *         The scaled amount is rounded up to ensure the user's aToken balance is sufficiently reduced.
   * @param amount The amount of underlying asset to withdraw.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The scaled amount of aTokens to burn.
   */
  function getATokenBurnScaledAmount(
    uint256 amount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return amount.rayDivCeil(liquidityIndex);
  }

  /**
   * @notice Calculates the scaled amount of aTokens to transfer.
   *         The scaled amount is rounded up to ensure the recipient receives at least the requested amount.
   * @param amount The amount of aTokens to transfer.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The scaled amount of aTokens for transfer.
   */
  function getATokenTransferScaledAmount(
    uint256 amount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return amount.rayDivCeil(liquidityIndex);
  }

  /**
   * @notice Calculates the actual aToken balance from a scaled balance and the current liquidityIndex.
   *         The balance is rounded down to prevent overaccounting.
   * @param scaledAmount The scaled aToken balance.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The actual aToken balance.
   */
  function getATokenBalance(
    uint256 scaledAmount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return scaledAmount.rayMulFloor(liquidityIndex);
  }

  /**
   * @notice Calculates the scaled amount of vTokens to mint when borrowing.
   *         The amount is rounded up to ensure the protocol never underaccounts the user's debt.
   * @param amount The amount of underlying asset borrowed.
   * @param variableBorrowIndex The current vToken variableBorrowIndex.
   * @return The scaled amount of vTokens to mint.
   */
  function getVTokenMintScaledAmount(
    uint256 amount,
    uint256 variableBorrowIndex
  ) internal pure returns (uint256) {
    return amount.rayDivCeil(variableBorrowIndex);
  }

  /**
   * @notice Calculates the scaled amount of vTokens to burn.
   *         The scaled amount is rounded down to prevent over-burning of vTokens.
   * @param amount The amount of underlying asset corresponding to the vTokens to burn.
   * @param variableBorrowIndex The current vToken variableBorrowIndex.
   * @return The scaled amount of vTokens to burn.
   */
  function getVTokenBurnScaledAmount(
    uint256 amount,
    uint256 variableBorrowIndex
  ) internal pure returns (uint256) {
    return amount.rayDivFloor(variableBorrowIndex);
  }

  /**
   * @notice Calculates the actual vToken balance (debt) from a scaled balance and the current variableBorrowIndex.
   *         The balance is rounded up to prevent underaccounting the user's debt.
   * @param scaledAmount The scaled vToken balance.
   * @param variableBorrowIndex The current vToken variableBorrowIndex.
   * @return The actual vToken balance (debt).
   */
  function getVTokenBalance(
    uint256 scaledAmount,
    uint256 variableBorrowIndex
  ) internal pure returns (uint256) {
    return scaledAmount.rayMulCeil(variableBorrowIndex);
  }
}
