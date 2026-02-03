// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IVariableDebtToken} from '@aave/core-v3/contracts/interfaces/IVariableDebtToken.sol';

/**
 * @title IGhoVariableDebtToken
 * @author Aave
 * @notice Defines the basic interface of the VariableDebtToken
 */
interface IGhoVariableDebtToken is IVariableDebtToken {
  /**
   * @dev Emitted when the address of the GHO AToken is set
   * @param aToken The address of the GhoAToken contract
   */
  event ATokenSet(address indexed aToken);

  /**
   * @dev Emitted when the GhoDiscountRateStrategy is updated
   * @param oldDiscountRateStrategy The address of the old GhoDiscountRateStrategy
   * @param newDiscountRateStrategy The address of the new GhoDiscountRateStrategy
   */
  event DiscountRateStrategyUpdated(
    address indexed oldDiscountRateStrategy,
    address indexed newDiscountRateStrategy
  );

  /**
   * @dev Emitted when the Discount Token is updated
   * @param oldDiscountToken The address of the old discount token
   * @param newDiscountToken The address of the new discount token
   */
  event DiscountTokenUpdated(address indexed oldDiscountToken, address indexed newDiscountToken);

  /**
   * @dev Emitted when a user's discount is updated
   * @param user The address of the user
   * @param oldDiscountPercent The old discount percent of the user
   * @param newDiscountPercent The new discount percent of the user
   */
  event DiscountPercentUpdated(
    address indexed user,
    uint256 oldDiscountPercent,
    uint256 indexed newDiscountPercent
  );

  /**
   * @notice Sets a reference to the GHO AToken
   * @param ghoAToken The address of the GhoAToken contract
   */
  function setAToken(address ghoAToken) external;

  /**
   * @notice Returns the address of the GHO AToken
   * @return The address of the GhoAToken contract
   */
  function getAToken() external view returns (address);

  /**
   * @notice Updates the Discount Rate Strategy
   * @param newDiscountRateStrategy The address of DiscountRateStrategy contract
   */
  function updateDiscountRateStrategy(address newDiscountRateStrategy) external;

  /**
   * @notice Returns the address of the Discount Rate Strategy
   * @return The address of DiscountRateStrategy contract
   */
  function getDiscountRateStrategy() external view returns (address);

  /**
   * @notice Updates the Discount Token
   * @param newDiscountToken The address of the DiscountToken contract
   */
  function updateDiscountToken(address newDiscountToken) external;

  /**
   * @notice Returns the address of the Discount Token
   * @return address The address of DiscountToken
   */
  function getDiscountToken() external view returns (address);

  /**
   * @notice Updates the discount percents of the users when a discount token transfer occurs
   * @dev To be executed before the token transfer happens
   * @param sender The address of sender
   * @param recipient The address of recipient
   * @param senderDiscountTokenBalance The sender discount token balance
   * @param recipientDiscountTokenBalance The recipient discount token balance
   * @param amount The amount of discount token being transferred
   */
  function updateDiscountDistribution(
    address sender,
    address recipient,
    uint256 senderDiscountTokenBalance,
    uint256 recipientDiscountTokenBalance,
    uint256 amount
  ) external;

  /**
   * @notice Returns the discount percent being applied to the debt interest of the user
   * @param user The address of the user
   * @return The discount percent (expressed in bps)
   */
  function getDiscountPercent(address user) external view returns (uint256);

  /*
   * @dev Returns the amount of interests accumulated by the user
   * @param user The address of the user
   * @return The amount of interests accumulated by the user
   */
  function getBalanceFromInterest(address user) external view returns (uint256);

  /**
   * @dev Decrease the amount of interests accumulated by the user
   * @param user The address of the user
   * @param amount The value to be decrease
   */
  function decreaseBalanceFromInterest(address user, uint256 amount) external;

  /**
   * @notice Rebalances the discount percent of a user
   * @param user The address of the user
   */
  function rebalanceUserDiscountPercent(address user) external;
}
