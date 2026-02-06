pragma solidity ^0.6.12;

interface IAToken {
  function getScaledUserBalanceAndSupply(address user) external view returns (uint256, uint256);
}
