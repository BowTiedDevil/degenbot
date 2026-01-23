// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

import 'forge-std/Test.sol';

import {PercentageMath} from '../../../../src/contracts/protocol/libraries/math/PercentageMath.sol';
import {PercentageMathWrapper} from '../../../../src/contracts/mocks/tests/PercentageMathWrapper.sol';

contract PercentageMathTests is Test {
  PercentageMathWrapper internal w;

  function setUp() public {
    w = new PercentageMathWrapper();
  }

  function test_constants() public view {
    assertEq(w.PERCENTAGE_FACTOR(), 1e4, 'percentage factor');
    assertEq(w.HALF_PERCENTAGE_FACTOR(), 0.5e4, 'half wad');
  }

  function test_percentMul_fuzz(uint256 value, uint256 percentage) public {
    if (
      (percentage == 0 ||
        (value > (type(uint256).max - w.HALF_PERCENTAGE_FACTOR()) / percentage) == false) == false
    ) {
      vm.expectRevert();
      w.percentMul(value, percentage);
    } else {
      assertEq(
        w.percentMul(value, percentage),
        ((value * percentage) + w.HALF_PERCENTAGE_FACTOR()) / (w.PERCENTAGE_FACTOR())
      );
    }
  }

  function test_percentDiv_fuzz(uint256 value, uint256 percentage) public {
    if (percentage == 0 || value > (type(uint256).max - (percentage / 2)) / w.PERCENTAGE_FACTOR()) {
      vm.expectRevert();
      w.percentDiv(value, percentage);
    } else {
      assertEq(
        w.percentDiv(value, percentage),
        ((value * w.PERCENTAGE_FACTOR()) + (percentage / 2)) / percentage
      );
    }
  }

  function test_percentMul() public view {
    assertEq(w.percentMul(1e18, 50_00), 0.5e18);
    assertEq(w.percentMul(14.2515e18, 74_42), 10.605966300000000000e18);
    assertEq(w.percentMul(9087312e27, 13_33), 1211338689600000000000000000000000);
  }

  function test_percentDiv() public view {
    assertEq(w.percentDiv(1e18, 50_00), 2e18);
    assertEq(w.percentDiv(14.2515e18, 74_42), 19.150094060736361193e18);
    assertEq(w.percentDiv(9087312e27, 13_33), 68171882970742685671417854463615904);
  }
}