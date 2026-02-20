// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

import 'forge-std/Test.sol';
import {WadRayMathWrapper} from '../../../../src/contracts/mocks/tests/WadRayMathWrapper.sol';

contract WadRayMathTests is Test {
  WadRayMathWrapper internal w;

  function setUp() public {
    w = new WadRayMathWrapper();
  }

  function test_constants() public view {
    assertEq(w.WAD(), 1e18, 'wad');
    assertEq(w.HALF_WAD(), 1e18 / 2, 'half wad');
    assertEq(w.RAY(), 1e27, 'ray');
    assertEq(w.HALF_RAY(), 1e27 / 2, 'half_ray');
  }

  function test_wadMul_edge() public view {
    assertEq(w.wadMul(0, 1e18), 0);
    assertEq(w.wadMul(1e18, 0), 0);
    assertEq(w.wadMul(0, 0), 0);
  }

  function test_wadMul_fuzzing(uint256 a, uint256 b) public {
    if ((b == 0 || (a > (type(uint256).max - w.HALF_WAD()) / b) == false) == false) {
      vm.expectRevert();
      w.wadMul(a, b);
      return;
    }

    assertEq(w.wadMul(a, b), ((a * b) + w.HALF_WAD()) / w.WAD());
  }

  function test_wadDiv_fuzzing(uint256 a, uint256 b) public {
    if ((b == 0) || (((a > ((type(uint256).max - b / 2) / w.WAD())) == false) == false)) {
      vm.expectRevert();
      w.wadDiv(a, b);
      return;
    }

    assertEq(w.wadDiv(a, b), ((a * w.WAD()) + (b / 2)) / b);
  }

  function test_wadMul() public view {
    assertEq(w.wadMul(2.5e18, 0.5e18), 1.25e18);
    assertEq(w.wadMul(412.2e18, 1e18), 412.2e18);
    assertEq(w.wadMul(6e18, 2e18), 12e18);
  }

  function test_rayMul() public view {
    assertEq(w.rayMul(2.5e27, 0.5e27), 1.25e27);
    assertEq(w.rayMul(412.2e27, 1e27), 412.2e27);
    assertEq(w.rayMul(6e27, 2e27), 12e27);
  }

  function test_wadDiv() public view {
    assertEq(w.wadDiv(2.5e18, 0.5e18), 5e18);
    assertEq(w.wadDiv(412.2e18, 1e18), 412.2e18);
    assertEq(w.wadDiv(8.745e18, 0.67e18), 13.052238805970149254e18);
    assertEq(w.wadDiv(6e18, 2e18), 3e18);
  }

  function test_rayDiv() public view {
    assertEq(w.rayDiv(2.5e27, 0.5e27), 5e27);
    assertEq(w.rayDiv(412.2e27, 1e27), 412.2e27);
    assertEq(w.rayDiv(8.745e27, 0.67e27), 13.052238805970149253731343284e27);
    assertEq(w.rayDiv(6e27, 2e27), 3e27);
  }

  function test_wadToRay() public view {
    assertEq(w.wadToRay(1e18), 1e27);
    assertEq(w.wadToRay(412.2e18), 412.2e27);
    assertEq(w.wadToRay(0), 0);
  }

  function test_rayToWad() public view {
    assertEq(w.rayToWad(1e27), 1e18);
    assertEq(w.rayToWad(412.2e27), 412.2e18);
    assertEq(w.rayToWad(0), 0);
  }

  function test_wadToRay_fuzz(uint256 a) public {
    uint256 b;
    bool safetyCheck;
    unchecked {
      b = a * w.WAD_RAY_RATIO();
      safetyCheck = b / w.WAD_RAY_RATIO() == a;
    }
    if (!safetyCheck) {
      vm.expectRevert();
      w.wadToRay(a);
    } else {
      assertEq(w.wadToRay(a), a * w.WAD_RAY_RATIO());
      assertEq(w.wadToRay(a), b);
    }
  }

  function test_rayToWad_fuzz(uint256 a) public view {
    uint256 b;
    uint256 remainder;
    bool roundHalf;
    unchecked {
      b = a / w.WAD_RAY_RATIO();
      remainder = a % w.WAD_RAY_RATIO();
      roundHalf = remainder < w.WAD_RAY_RATIO() / 2;
    }
    if (!roundHalf) {
      assertEq(w.rayToWad(a), (a / w.WAD_RAY_RATIO()) + 1);
      assertEq(w.rayToWad(a), b + 1);
    } else {
      assertEq(w.rayToWad(a), a / w.WAD_RAY_RATIO());
      assertEq(w.rayToWad(a), b);
    }
  }
}