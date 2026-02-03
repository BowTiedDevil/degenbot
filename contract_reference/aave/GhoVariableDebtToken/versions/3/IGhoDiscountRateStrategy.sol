// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/**
 * @title IGhoDiscountRateStrategy
 * @author Aave
 * @notice Defines the basic interface of the GhoDiscountRateStrategy
 */
interface IGhoDiscountRateStrategy {
  /**
   * @notice Calculates the discount rate depending on the debt and discount token balances
   * @param debtBalance The debt balance of the user
   * @param discountTokenBalance The discount token balance of the user
   * @return The discount rate, as a percentage - the maximum can be 10000 = 100.00%
   */
  function calculateDiscountRate(
    uint256 debtBalance,
    uint256 discountTokenBalance
  ) external view returns (uint256);
}
