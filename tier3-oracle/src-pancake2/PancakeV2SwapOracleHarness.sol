pragma solidity =0.5.16;

import {PancakePair} from "pancake2-core/contracts/PancakePair.sol";


/// Minimal ERC-20 for the PancakeSwap V2 oracle — only the entry points
/// `PancakePair` reads: `balanceOf` (the `_update` reserves + K-check) +
/// `transfer` (the `_safeTransfer` route). `balanceOf` is at storage slot 0 so
/// the pair's `token.call(transfer, …)` + K-check see a real accounting entry,
/// not a manually-seeded slot. Byte-identical to the Uniswap V2 oracle's mock
/// (`src-v2/V2SwapOracleHarness.sol`) — PancakePair consumes the same ERC-20
/// surface and the two mocks compile under the same solc 0.5.16.
contract MockERC20V2 {
    string public name;
    string public symbol;
    uint8 public decimals;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    constructor(string memory _name, string memory _symbol, uint8 _decimals) public {
        name = _name;
        symbol = _symbol;
        decimals = _decimals;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] = balanceOf[to] + amount;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        balanceOf[msg.sender] = balanceOf[msg.sender] - amount;
        balanceOf[to] = balanceOf[to] + amount;
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        allowance[from][msg.sender] = allowance[from][msg.sender] - amount;
        balanceOf[from] = balanceOf[from] - amount;
        balanceOf[to] = balanceOf[to] + amount;
        return true;
    }
}


/// PancakeSwap V2 pair on-chain accuracy oracle harness. Deploys the REAL
/// `PancakePair` — the canonical `pancakeswap/pancake-swap-core` contract
/// (vendored under `lib/pancake2-src/`, solc 0.5.16), a Uniswap V2 fork that
/// hardcodes a **0.2%** swap fee (`balance0Adjusted = balance0.mul(1000)
/// .sub(amount0In.mul(2))` → retained `(998, 1000)`, vs Uniswap's 0.3%
/// `(997, 1000)`) and stores reserves as the 3-tuple `(uint112, uint112,
/// uint32 blockTimestampLast)` (the `PancakeswapStyle` ABI the engine's
/// `DexVariant::PancakeswapV2` reads) — plus two `MockERC20V2` tokens. Exposes
/// a `setup` (mint reserves + `sync` so the slot-8 reserves 3-tuple equals the
/// live `balanceOf` — the K-check consistency a TEST oracle sets by
/// construction, per ADR-020 D4) and a `doSwap` (transfer input in + `pair.swap`
/// with the TEST-computed output).
///
/// The harness carries NO swap math: `doSwap` takes `amountOut` as a PARAMETER
/// (computed by the Rust engine) and routes it to `pair.swap`. If `amountOut`
/// exceeds the on-chain K-invariant boundary `pair.swap` reverts ('Pancake: K')
/// — so "swap succeeds with engine's amountOut" + "swap reverts with amountOut
/// + 1" together prove the engine's value is BYTE-EXACT the on-chain maximal
/// output at the fork's hardcoded 0.2% fee.
contract PancakeV2SwapOracleHarness {
    PancakePair public pair;
    MockERC20V2 public token0;
    MockERC20V2 public token1;

    constructor() public {
        MockERC20V2 t0 = new MockERC20V2("T0", "T0", 18);
        MockERC20V2 t1 = new MockERC20V2("T1", "T1", 18);
        token0 = t0;
        token1 = t1;
        PancakePair p = new PancakePair(); // factory := msg.sender (this harness)
        p.initialize(address(t0), address(t1));
        pair = p;
    }

    /// Mint `r0`/`r1` of token0/token1 to the pair then `sync` so the pair's
    /// slot-8 reserves (3-tuple) equal the live `balanceOf` (the K-check
    /// `balance0Adjusted * balance1Adjusted >= reserve0 * reserve1` then holds
    /// with the seeded reserves on both sides). Whole-slot-set seeding via
    /// `sync` — no manual slot-8 + balanceOf bookkeeping split.
    function setup(uint112 r0, uint112 r1) external {
        token0.mint(address(pair), uint256(r0));
        token1.mint(address(pair), uint256(r1));
        pair.sync();
    }

    /// Transfer `amountIn` of the input token to the pair, then call
    /// `pair.swap` with the test-computed `amountOut` routed to the correct
    /// side. Reverts (K-invariant, 'Pancake: K') if `amountOut` exceeds the
    /// on-chain boundary at the fork's hardcoded 0.2% fee. `to` receives the
    /// output tokens. Direct typed calls so a `pair.swap` revert propagates its
    /// real reason to the test caller.
    function doSwap(uint256 amountIn, bool zeroForOne, uint256 amountOut, address to) external {
        MockERC20V2 tIn = zeroForOne ? token0 : token1;
        tIn.mint(address(this), amountIn);
        tIn.transfer(address(pair), amountIn);
        (uint256 a0Out, uint256 a1Out) = zeroForOne
            ? (uint256(0), amountOut)
            : (amountOut, uint256(0));
        pair.swap(a0Out, a1Out, to, "");
    }
}
