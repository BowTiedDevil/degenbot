// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {ICmdExecutor} from "../src/generated/ICmdExecutor.sol";

/// Test-local unbounded token — the boundary driver for the checked-mul guard
/// in the executor's V2 amount-out math. Real (and fake) tokens have finite
/// supply, so the overflow is unreachable with them; this contract exists only
/// inside this test file and mints arbitrary amounts so a donation can be made
/// large enough to cross the uint256 boundary. Production fakes are untouched.
contract UnboundedTestToken {
    string public constant NAME = "Unbounded Test Token";
    string public constant SYMBOL = "UTT";
    uint8 public constant DECIMALS = 18;

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    function mint(address to, uint256 value) external {
        balanceOf[to] += value;
        totalSupply += value;
        emit Transfer(address(0), to, value);
    }

    function transfer(address to, uint256 value) external returns (bool) {
        balanceOf[msg.sender] -= value;
        balanceOf[to] += value;
        emit Transfer(msg.sender, to, value);
        return true;
    }

    function transferFrom(address from, address to, uint256 value) external returns (bool) {
        allowance[from][msg.sender] -= value;
        balanceOf[from] -= value;
        balanceOf[to] += value;
        emit Transfer(from, to, value);
        return true;
    }

    function approve(address spender, uint256 value) external returns (bool) {
        allowance[msg.sender][spender] = value;
        emit Approval(msg.sender, spender, value);
        return true;
    }
}

/// Port of `tests/executor/test_v2_swap_calc_overflow.py`.
///
/// The source module is a permanent skip: it argues the donation-driven wrap
/// grief is unreachable with finite-supply tokens and settles for a structural
/// note. This port replaces the skip with executable boundary evidence:
///
/// - the boundary test drives V2_SWAP_CALC with a donation large enough to
///   overflow the amount-in-carrying multiplication
///   (`amount_in * fee_multiplier` stays below 2^256 in the unchecked first
///   product, but `* reserve_out` overflows in the checked second product),
///   and asserts a clean checked-mul revert instead of a wrapped (small, wrong)
///   amount_out — with a no-output-delivered guard;
/// - the normal-path control proves the guard does not regress a regular
///   excess-balance swap.
contract V2SwapCalcOverflowTest is PoolSetup {
    // AMOUNT_USDC (2000e6) is the fixture constant.
    uint256 internal constant LIQ_INPUT = 1 ether;

    /// A donation of this size against a 2000 USDC reserve_out overflows the
    /// checked product: 1e64 * 9970 ~= 9.97e67 < 2^256 (the unchecked mul does
    /// not wrap), then * 2e9 ~= 1.99e77 > 2^256 (the checked mul reverts).
    uint256 internal constant GIANT_DONATION = 1e64;

    struct OverflowRig {
        UnboundedTestToken input;
        IFakeUniswapV2Pair pair;
        ICmdExecutor ex;
        bool zfo;
    }

    /// Deploy an unbounded-input-token / USDC pair plus the bespoke executor
    /// (the pair standing in as the PM placeholder — V2_SWAP_CALC never
    /// touches the PoolManager), fund liquidity, and sync reserves.
    function _deployRig() internal returns (OverflowRig memory rig) {
        rig.input = new UnboundedTestToken();
        (address t0, address t1) = _sorted(address(rig.input), address(usdc));
        rig.pair = _deployV2Pair(t0, t1, 0, 30);
        rig.ex = ICmdExecutor(
            _deploy(
                "CmdExecutor",
                abi.encode(address(weth), address(rig.pair), address(weth), address(weth))
            )
        );

        usdc.mint(address(rig.pair), AMOUNT_USDC);
        rig.input.mint(address(rig.pair), LIQ_INPUT);
        rig.pair.sync();
        rig.zfo = rig.pair.token0() == address(rig.input);
    }

    function _calcSwap(OverflowRig memory rig) internal view returns (bytes memory) {
        Cmd.Table memory at = _emptyTable();
        uint8 pairIdx = Cmd.add(at, address(rig.pair));
        uint8 exIdx = Cmd.add(at, address(rig.ex));
        return bytes.concat(Cmd.preamble(at), Cmd.v2SwapCalc(pairIdx, rig.zfo, exIdx, 30));
    }

    /// Boundary: a giant donation must revert cleanly at the checked mul —
    /// never wrap to a small (wrong) amount_out that the pair would happily
    /// pay out.
    function testCheckedMulRevertsOnGiantDonation() public {
        OverflowRig memory rig = _deployRig();

        // The donation lands after sync(), so it is pure excess balance: the
        // executor reads amount_in = balanceOf(pair) - reserve_in ~= 1e64.
        rig.input.mint(address(rig.pair), GIANT_DONATION);

        Outcome memory o = _executeRaw(rig.ex, _calcSwap(rig));
        assertFalse(o.ok, "giant donation must hit the checked-mul boundary, not wrap");
        // The executor's checked mul carries no revert identity — pin the
        // zero-length payload the Vyper checked arithmetic raises.
        assertEq(o.revertData.length, 0, "clean checked-mul revert (no wrapped output path)");
        // Nothing was delivered: a wrapped small amount_out would have paid USDC.
        assertEq(usdc.balanceOf(address(rig.ex)), 0, "no wrapped output delivered");
    }

    /// Normal-path control: with an ordinary excess-balance donation the same
    /// V2_SWAP_CALC path computes a sane output and delivers it — the guard
    /// does not regress the regular swap.
    function testNormalV2SwapCalcUnaffected() public {
        OverflowRig memory rig = _deployRig();

        uint256 reserveIn = rig.input.balanceOf(address(rig.pair));
        uint256 reserveOut = usdc.balanceOf(address(rig.pair));
        uint256 expectedOut = _v2GetAmountOut(LIQ_INPUT, reserveIn, reserveOut, 30);

        rig.input.mint(address(rig.pair), LIQ_INPUT);

        Outcome memory o = _executeRaw(rig.ex, _calcSwap(rig));
        assertTrue(o.ok, "normal excess-balance swap must succeed");
        assertGe(
            usdc.balanceOf(address(rig.ex)),
            expectedOut,
            "USDC output covers expected amount on the normal path"
        );
    }
}
