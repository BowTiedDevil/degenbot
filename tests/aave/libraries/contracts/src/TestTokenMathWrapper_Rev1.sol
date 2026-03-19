// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

import {WadRayMath} from "./WadRayMath.sol";
import {TokenMath} from "./TokenMath_Rev1.sol";

/**
 * @title TestTokenMathWrapper (Rev 1)
 * @notice Wrapper contract to expose TokenMath library functions as external calls.
 * @dev Used for property-based testing comparing Python vs Solidity implementations.
 * @dev Rev 1 uses half-up rounding (rayMul, rayDiv) since floor/ceil variants don't exist.
 */
contract TestTokenMathWrapper_Rev1 {
  using WadRayMath for uint256;
  using TokenMath for uint256;

  // ============================================================================
  // Constants
  // ============================================================================

  function WAD() external pure returns (uint256) {
    return WadRayMath.WAD;
  }

  function RAY() external pure returns (uint256) {
    return WadRayMath.RAY;
  }

  // ============================================================================
  // Collateral (aToken) Functions
  // ============================================================================

  /**
   * @notice Get scaled amount to mint for collateral deposits.
   * @param amount Underlying amount to deposit.
   * @param index Current liquidity index.
   * @return Scaled amount to mint.
   */
  function getCollateralMintScaledAmount(
    uint256 amount,
    uint256 index
  ) external pure returns (uint256) {
    return amount.getATokenMintScaledAmount(index);
  }

  /**
   * @notice Get scaled amount to burn for collateral withdrawals.
   * @param amount Underlying amount to withdraw.
   * @param index Current liquidity index.
   * @return Scaled amount to burn.
   */
  function getCollateralBurnScaledAmount(
    uint256 amount,
    uint256 index
  ) external pure returns (uint256) {
    return amount.getATokenBurnScaledAmount(index);
  }

  /**
   * @notice Get scaled amount for collateral transfers.
   * @param amount Underlying amount to transfer.
   * @param index Current liquidity index.
   * @return Scaled amount for transfer.
   */
  function getCollateralTransferScaledAmount(
    uint256 amount,
    uint256 index
  ) external pure returns (uint256) {
    return amount.getATokenTransferScaledAmount(index);
  }

  /**
   * @notice Get underlying balance from scaled collateral balance.
   * @param scaledAmount Scaled aToken balance.
   * @param index Current liquidity index.
   * @return Underlying balance.
   */
  function getCollateralBalance(
    uint256 scaledAmount,
    uint256 index
  ) external pure returns (uint256) {
    return scaledAmount.getATokenBalance(index);
  }

  // ============================================================================
  // Debt (vToken) Functions
  // ============================================================================

  /**
   * @notice Get scaled amount to mint for debt (borrowing).
   * @param amount Underlying amount to borrow.
   * @param index Current variable borrow index.
   * @return Scaled amount to mint.
   */
  function getDebtMintScaledAmount(
    uint256 amount,
    uint256 index
  ) external pure returns (uint256) {
    return amount.getVTokenMintScaledAmount(index);
  }

  /**
   * @notice Get scaled amount to burn for debt repayment.
   * @param amount Underlying amount to repay.
   * @param index Current variable borrow index.
   * @return Scaled amount to burn.
   */
  function getDebtBurnScaledAmount(
    uint256 amount,
    uint256 index
  ) external pure returns (uint256) {
    return amount.getVTokenBurnScaledAmount(index);
  }

  /**
   * @notice Get underlying debt balance from scaled debt balance.
   * @param scaledAmount Scaled vToken balance.
   * @param index Current variable borrow index.
   * @return Underlying debt balance.
   */
  function getDebtBalance(
    uint256 scaledAmount,
    uint256 index
  ) external pure returns (uint256) {
    return scaledAmount.getVTokenBalance(index);
  }

  // ============================================================================
  // Raw Math Functions (for direct testing)
  // ============================================================================

  function rayMul(uint256 a, uint256 b) external pure returns (uint256) {
    return a.rayMul(b);
  }

  function rayDiv(uint256 a, uint256 b) external pure returns (uint256) {
    return a.rayDiv(b);
  }

  function wadMul(uint256 a, uint256 b) external pure returns (uint256) {
    return a.wadMul(b);
  }

  function wadDiv(uint256 a, uint256 b) external pure returns (uint256) {
    return a.wadDiv(b);
  }
}
