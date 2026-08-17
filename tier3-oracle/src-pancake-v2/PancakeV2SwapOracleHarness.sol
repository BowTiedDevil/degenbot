pragma solidity =0.5.16;

/// Minimal ABI surface of the DEPLOYED PancakeSwap V2 `PancakePair` that the
/// harness drives. Deliberately NOT `import {PancakePair}` from a vendored
/// source: the pair's logic must come ONLY from the PINNED on-chain creation
/// bytecode (deployed via a raw EVM `create`), never from a locally-compiled
/// copy — a local recompile can't reproduce the deployment's embedded metadata
/// hash, and the old init-code-hash check (`0x57224589…`) was exactly where a
/// source build drifted. The ABI here (initialize/sync/swap) matches the
/// on-chain contract and is read verbatim by the Rust driver.
interface IMinimalPancakePair {
    function initialize(address _token0, address _token1) external;
    function sync() external;
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
}


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


/// PancakeSwap V2 pair on-chain accuracy oracle harness. Deploys the REAL,
/// PINNED Ethereum-mainnet `PancakePair` — the canonical
/// `pancakeswap/pancake-swap-core` fork (the live mainnet pair
/// `0x2E8135bE71230c6B1B4045696d41C09Db0414226`, Sourcify `exact_match`
/// bytecode committed under `artifacts/PancakeV2Pair/`), which hardcodes a
/// **0.25%** swap fee (`balance0Adjusted = balance0.mul(10000)
/// .sub(amount0In.mul(25))` → retained `(9975, 10000)`) and stores reserves as
/// the 3-tuple `(uint112, uint112, uint32 blockTimestampLast)` (the
/// `PancakeswapStyle` ABI the engine's `DexVariant::PancakeswapV2` reads) —
/// plus two `MockERC20V2` tokens.
///
/// The pair is created from the PINNED on-chain creation bytecode passed as
/// the constructor argument (`pairInitCode`) via a raw EVM `create`, so the
/// deployed contract is byte-for-byte the live deployment (not a local
/// compile) and `factory() == address(this)` (this harness) exactly as a real
/// factory deploy. It is then `initialize(token0, token1)` as the factory
/// does. Exposes a `setup` (mint reserves + `sync` so the slot-8 reserves
/// 3-tuple equals the live `balanceOf` — the K-check consistency a TEST oracle
/// sets by construction, per ADR-020 D4) and a `doSwap` (transfer input in +
/// `pair.swap` with the TEST-computed output).
///
/// The harness carries NO swap math: `doSwap` takes `amountOut` as a PARAMETER
/// (computed by the Rust engine) and routes it to `pair.swap`. If `amountOut`
/// exceeds the on-chain K-invariant boundary `pair.swap` reverts
/// ('Pancake: K') — so "swap succeeds with engine's amountOut" + "swap reverts
/// with amountOut + 1" together prove the engine's value is BYTE-EXACT the
/// on-chain maximal output at the fork's hardcoded 0.25% fee.
contract PancakeV2SwapOracleHarness {
    IMinimalPancakePair public pair;
    MockERC20V2 public token0;
    MockERC20V2 public token1;

    constructor(bytes memory pairInitCode) public {
        MockERC20V2 t0 = new MockERC20V2("T0", "T0", 18);
        MockERC20V2 t1 = new MockERC20V2("T1", "T1", 18);
        token0 = t0;
        token1 = t1;
        address p;
        // Raw create of the PINNED on-chain creation code: the created
        // contract's `factory()` == this harness (as a real factory deploy).
        assembly {
            p := create(0, add(pairInitCode, 0x20), mload(pairInitCode))
        }
        require(p != address(0), "pair create failed");
        IMinimalPancakePair(p).initialize(address(t0), address(t1));
        pair = IMinimalPancakePair(p);
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
    /// on-chain boundary at the fork's hardcoded 0.25% fee. `to` receives the
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
