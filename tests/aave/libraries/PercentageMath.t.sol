// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

import 'forge-std/Test.sol';

import {PercentageMath} from '../../../../src/contracts/protocol/libraries/math/PercentageMath.sol';

/// forge-config: default.allow_internal_expect_revert = true
contract PercentageMathTests is Test {
  function test_constants() public pure {
    assertEq(PercentageMath.PERCENTAGE_FACTOR, 1e4, 'percentage factor');
    assertEq(PercentageMath.HALF_PERCENTAGE_FACTOR, 0.5e4, 'half wad');
  }

  function test_percentMul_fuzz(uint256 value, uint256 percentage) public {
    if (
      (percentage == 0 ||
        (value > (type(uint256).max - PercentageMath.HALF_PERCENTAGE_FACTOR) / percentage) ==
        false) == false
    ) {
      vm.expectRevert();
      PercentageMath.percentMul(value, percentage);
    } else {
      assertEq(
        PercentageMath.percentMul(value, percentage),
        ((value * percentage) + PercentageMath.HALF_PERCENTAGE_FACTOR) /
          (PercentageMath.PERCENTAGE_FACTOR)
      );
    }
  }

  function test_percentDiv_fuzz(uint256 value, uint256 percentage) public {
    if (
      percentage == 0 ||
      value > (type(uint256).max - (percentage / 2)) / PercentageMath.PERCENTAGE_FACTOR
    ) {
      vm.expectRevert();
      PercentageMath.percentDiv(value, percentage);
    } else {
      assertEq(
        PercentageMath.percentDiv(value, percentage),
        ((value * PercentageMath.PERCENTAGE_FACTOR) + (percentage / 2)) / percentage
      );
    }
  }

  function test_percentMul_revertOnOverflow() public {
    uint256 max = type(uint256).max;
    // percentage != 0 and value > (max - HALF_PERCENTAGE_FACTOR) / percentage
    vm.expectRevert();
    PercentageMath.percentMul(max, 2);
  }

  function test_percentDiv_revertOnDivByZero() public {
    vm.expectRevert();
    PercentageMath.percentDiv(1e18, 0);
  }

  function test_percentDiv_revertOnOverflow() public {
    uint256 max = type(uint256).max;
    // value > (max - percentage/2) / PERCENTAGE_FACTOR
    vm.expectRevert();
    PercentageMath.percentDiv(max, 1);
  }

  function test_percentMul() external pure {
    assertEq(PercentageMath.percentMul(1e18, 50_00), 0.5e18);
    assertEq(PercentageMath.percentMul(14.2515e18, 74_42), 10.605966300000000000e18);
    assertEq(PercentageMath.percentMul(9087312e27, 13_33), 1211338689600000000000000000000000);
  }

  function test_percentDiv() external pure {
    assertEq(PercentageMath.percentDiv(1e18, 50_00), 2e18);
    assertEq(PercentageMath.percentDiv(14.2515e18, 74_42), 19.150094060736361193e18);
    assertEq(PercentageMath.percentDiv(9087312e27, 13_33), 68171882970742685671417854463615904);
  }

  function testPercentMulCeil_Exact() external pure {
    uint256 result = PercentageMath.percentMulCeil(100 ether, PercentageMath.PERCENTAGE_FACTOR); // 100%
    assertEq(result, 100 ether);
  }

  function testPercentMulCeil_WithRoundingUp() external pure {
    uint256 result = PercentageMath.percentMulCeil(1, 1); // (1 * 1) / 10_000 = 0.0001 => ceil to 1
    assertEq(result, 1);
  }

  function testPercentMulCeil_ZeroValueOrPercent() external pure {
    assertEq(PercentageMath.percentMulCeil(0, 100), 0);
    assertEq(PercentageMath.percentMulCeil(100, 0), 0);
  }

  function testPercentMulCeil_RevertOnOverflow() public {
    uint256 max = type(uint256).max;
    vm.expectRevert();
    PercentageMath.percentMulCeil(max, 2);
  }

  function testPercentMulFloor_Exact() external pure {
    uint256 result = PercentageMath.percentMulFloor(100 ether, PercentageMath.PERCENTAGE_FACTOR); // 100%
    assertEq(result, 100 ether);
  }

  function testPercentMulFloor_WithTruncation() external pure {
    uint256 result = PercentageMath.percentMulFloor(1, 1); // (1 * 1) / 10_000 = 0.0001 => floor to 0
    assertEq(result, 0);
  }

  function testPercentMulFloor_ZeroInputs() external pure {
    assertEq(PercentageMath.percentMulFloor(0, 1234), 0);
    assertEq(PercentageMath.percentMulFloor(1234, 0), 0);
  }

  function testPercentMulFloor_RevertOnOverflow() external {
    uint256 max = type(uint256).max;
    vm.expectRevert();
    PercentageMath.percentMulFloor(max, 2);
  }

  function testPercentDivCeil_Exact() external pure {
    uint256 result = PercentageMath.percentDivCeil(100 ether, PercentageMath.PERCENTAGE_FACTOR); // 100%
    assertEq(result, 100 ether);
  }

  function testPercentDivCeil_WithCeilNeeded() external pure {
    uint256 result = PercentageMath.percentDivCeil(5, 3); // (5 * 10_000) / 3 = 16666.6... => ceil to 16667
    assertEq(result, 16667);
  }

  function testPercentDivCeil_RevertOnDivByZero() public {
    vm.expectRevert();
    PercentageMath.percentDivCeil(1234, 0);
  }

  function testPercentDivCeil_RevertOnOverflow() public {
    uint256 max = type(uint256).max;
    vm.expectRevert();
    PercentageMath.percentDivCeil(max, 1); // max * PERCENTAGE_FACTOR will overflow
  }
}