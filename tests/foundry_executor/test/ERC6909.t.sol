// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_erc6909.py`.
///
/// ERC6909 mint/burn on the fake PoolManager: internal balances held as
/// accounting entries without physical ERC-20 transfers. Replacing
/// take+sync+transfer+settle with mint turns four operations (delta back to
/// +amount) into one (delta to 0).
///
///   - View surface: default balance/allowance/operator are zero, and the
///     owner can grant an allowance and set an operator.
///   - V4_MINT_COMPACT converts a positive delta into an ERC6909 balance.
///   - V4_BURN_COMPACT converts an ERC6909 balance into a payable delta.
///   - The source's `test_mint_vs_take_gas` (disposition 11) becomes one
///     self-contained named comparison: both variants run from the same
///     pristine state (state-snapshot isolated), no module-global table, no
///     execution-order dependence.
///
/// Every execution keeps the source's `config = 2`.
contract ERC6909Test is PoolSetup {
    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;

    event MintVsTakeGas(
        string label, uint256 takeGas, uint256 mintGas, uint256 saved
    );

    /// The source `AddressTable()` order: pm, weth, usdc, executor, zero.
    /// The bare table resolves zero to the native sentinel; every other
    /// address takes an appended index.
    struct ETable {
        Cmd.Table at;
        uint8 pmIdx;
        uint8 wethIdx;
        uint8 usdcIdx;
        uint8 executorIdx;
        uint8 zeroIdx;
    }

    function _erc6909Table() internal view returns (ETable memory t) {
        t.at = _emptyTable();
        t.pmIdx = Cmd.add(t.at, address(v4Pm));
        t.wethIdx = Cmd.add(t.at, address(weth));
        t.usdcIdx = Cmd.add(t.at, address(usdc));
        t.executorIdx = Cmd.add(t.at, address(executor));
        t.zeroIdx = Cmd.add(t.at, address(0));
    }

    function _usdcId() internal view returns (uint256) {
        return uint256(uint160(address(usdc)));
    }

    /// WETH/USDC (fee 3000, ts 60), programmed for a canned swap. `sellWeth`
    /// picks the swap direction — the same pool key serves both phases of the
    /// burn test (phase 1 sells WETH, phase 2 sells USDC).
    function _setupWethUsdcPool(
        uint256 amountIn,
        uint256 amountOut,
        bool sellWeth,
        address outputToken
    ) internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 60);
        zfo =
            sellWeth ? key.currency0 == address(weth) : key.currency0 == address(usdc);
        _setupV4Swap(v4Pm, key, amountIn, amountOut, zfo, outputToken);
    }

    function _v4Swap(ETable memory t, PoolKey memory key, bool zfo, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return Cmd.v4SwapCompact(
            Cmd.add(t.at, key.currency0),
            Cmd.add(t.at, key.currency1),
            uint16(key.fee),
            int16(key.tick_spacing),
            t.zeroIdx,
            zfo,
            amount
        );
    }

    // ── View functions ────────────────────────────────────────────────────

    function testDefaultBalancesAreZero() public view {
        assertEq(v4Pm.balanceOf(address(this), _usdcId()), 0, "balance");
        assertEq(v4Pm.allowance(address(this), address(this), _usdcId()), 0, "allowance");
        assertFalse(v4Pm.isOperator(address(this), address(this)), "operator");
    }

    function testApproveAndSetOperator() public {
        // A plain second EOA stand-in (the source's accounts[1]).
        address other = address(uint160(0xA11CE));
        v4Pm.approve(other, _usdcId(), 1000);
        assertEq(v4Pm.allowance(address(this), other, _usdcId()), 1000, "allowance");
        v4Pm.setOperator(other, true);
        assertTrue(v4Pm.isOperator(address(this), other), "operator");
    }

    // ── V4_MINT_COMPACT ───────────────────────────────────────────────────

    /// V4 WETH→USDC swap, then mint USDC as ERC6909 instead of take: the
    /// positive USDC delta becomes an ERC6909 balance entry, no physical
    /// USDC transfer — the tokens stay inside the PoolManager.
    function testMintCreatesErc6909Balance() public {
        weth.mint(address(executor), AMOUNT_WETH);
        ETable memory t = _erc6909Table();
        (PoolKey memory key, bool zfo) =
            _setupWethUsdcPool(AMOUNT_WETH, AMOUNT_USDC, true, address(usdc));

        bytes memory inner = _v4Swap(t, key, zfo, AMOUNT_WETH);
        inner = bytes.concat(
            inner, Cmd.v4MintCompact(t.usdcIdx, t.executorIdx, AMOUNT_USDC)
        );
        // settle the WETH input debt
        inner = bytes.concat(inner, Cmd.v4Sync(t.wethIdx));
        inner = bytes.concat(inner, Cmd.erc20Transfer(t.wethIdx, t.pmIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4Settle());

        _execute(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner)), 2);

        assertEq(
            v4Pm.balanceOf(address(executor), _usdcId()), AMOUNT_USDC, "ERC6909 USDC minted"
        );
        assertEq(usdc.balanceOf(address(executor)), 0, "no physical USDC left the PM");
    }

    // ── V4_BURN_COMPACT ───────────────────────────────────────────────────

    /// Two-phase flow: (1) swap WETH→USDC, mint USDC as ERC6909, settle the
    /// WETH debt; (2) swap USDC→WETH, burn the ERC6909 USDC (settles the
    /// USDC debt with no physical transfer), take the WETH output.
    function testBurnSettlesFromErc6909() public {
        weth.mint(address(executor), AMOUNT_WETH);
        ETable memory t = _erc6909Table();

        // Phase 1: WETH→USDC, mint USDC as ERC6909.
        (PoolKey memory key, bool zfo) =
            _setupWethUsdcPool(AMOUNT_WETH, AMOUNT_USDC, true, address(usdc));
        bytes memory inner1 = _v4Swap(t, key, zfo, AMOUNT_WETH);
        inner1 = bytes.concat(
            inner1, Cmd.v4MintCompact(t.usdcIdx, t.executorIdx, AMOUNT_USDC)
        );
        inner1 = bytes.concat(inner1, Cmd.v4Sync(t.wethIdx));
        inner1 = bytes.concat(inner1, Cmd.erc20Transfer(t.wethIdx, t.pmIdx, AMOUNT_WETH));
        inner1 = bytes.concat(inner1, Cmd.v4Settle());
        _execute(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner1)), 2);
        assertEq(
            v4Pm.balanceOf(address(executor), _usdcId()), AMOUNT_USDC, "phase 1 minted"
        );

        // Phase 2: USDC→WETH, burn ERC6909 USDC to pay, take the WETH output.
        (PoolKey memory keyB, bool zfoB) =
            _setupWethUsdcPool(AMOUNT_USDC, AMOUNT_WETH_PROFIT, false, address(weth));
        bytes memory inner2 = _v4Swap(t, keyB, zfoB, AMOUNT_USDC);
        inner2 = bytes.concat(inner2, Cmd.v4BurnCompact(t.usdcIdx, AMOUNT_USDC));
        inner2 = bytes.concat(
            inner2, Cmd.v4TakeCompact(t.wethIdx, t.executorIdx, AMOUNT_WETH_PROFIT)
        );
        _execute(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner2)), 2);

        assertEq(v4Pm.balanceOf(address(executor), _usdcId()), 0, "ERC6909 burned");
        // The executor held no physical WETH before the take.
        assertEq(
            weth.balanceOf(address(executor)), AMOUNT_WETH_PROFIT, "exact WETH taken"
        );
    }

    // ── Gas comparison: mint vs take (disposition 11) ─────────────────────

    /// TAKE arm: physical USDC transfer out of the PM. Runs from whatever
    /// pristine state the caller established and returns the execution gas, so
    /// the comparison and the per-variant gas-evidence test share one body.
    function _runTakeArm() internal returns (uint256 gasUsed) {
        ETable memory t = _erc6909Table();
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory key, bool zfo) =
            _setupWethUsdcPool(AMOUNT_WETH, AMOUNT_USDC, true, address(usdc));
        bytes memory innerTake = _v4Swap(t, key, zfo, AMOUNT_WETH);
        innerTake = bytes.concat(
            innerTake, Cmd.v4Take(t.usdcIdx, t.executorIdx, AMOUNT_USDC)
        );
        innerTake = bytes.concat(innerTake, Cmd.v4Sync(t.wethIdx));
        innerTake =
            bytes.concat(innerTake, Cmd.erc20Transfer(t.wethIdx, t.pmIdx, AMOUNT_WETH));
        innerTake = bytes.concat(innerTake, Cmd.v4Settle());
        Outcome memory take =
            _executeRaw(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(innerTake)), 2);
        assertTrue(take.ok, "take variant should succeed");
        assertEq(usdc.balanceOf(address(executor)), AMOUNT_USDC, "take moved USDC");
        return take.gasUsed;
    }

    /// MINT arm: the identical stream with `V4_MINT_COMPACT` where the TAKE
    /// arm transfers physically. Same pristine-state precondition.
    function _runMintArm() internal returns (uint256 gasUsed) {
        ETable memory t = _erc6909Table();
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory key, bool zfo) =
            _setupWethUsdcPool(AMOUNT_WETH, AMOUNT_USDC, true, address(usdc));
        bytes memory innerMint = _v4Swap(t, key, zfo, AMOUNT_WETH);
        innerMint = bytes.concat(
            innerMint, Cmd.v4MintCompact(t.usdcIdx, t.executorIdx, AMOUNT_USDC)
        );
        innerMint = bytes.concat(innerMint, Cmd.v4Sync(t.wethIdx));
        innerMint =
            bytes.concat(innerMint, Cmd.erc20Transfer(t.wethIdx, t.pmIdx, AMOUNT_WETH));
        innerMint = bytes.concat(innerMint, Cmd.v4Settle());
        Outcome memory mint =
            _executeRaw(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(innerMint)), 2);
        assertTrue(mint.ok, "mint variant should succeed");
        assertEq(
            v4Pm.balanceOf(address(executor), _usdcId()),
            AMOUNT_USDC,
            "mint captured the delta as ERC6909"
        );
        return mint.gasUsed;
    }

    /// Self-contained named comparison replacing the source's
    /// `test_mint_vs_take_gas`: the TAKE arm (physical transfer out of the PM)
    /// and the MINT arm (ERC6909 entry, no transfer) each run from the same
    /// pristine state — the TAKE run's state is rolled back before the MINT
    /// run — so the comparison carries no execution-order dependence and no
    /// module-global summary.
    function testMintVsTakeGas() public {
        uint256 snapshot = vm.snapshotState();
        uint256 takeGas = _runTakeArm();
        vm.revertToState(snapshot);
        uint256 mintGas = _runMintArm();

        // Mint avoids the ERC-20 transfer out of the PM.
        assertTrue(mintGas < takeGas, "mint should be cheaper than take");
        emit MintVsTakeGas("erc6909_mint_vs_take", takeGas, mintGas, takeGas - mintGas);
    }

    /// Per-variant gas label for the TAKE arm: one test per variant so the
    /// Forge snapshot (`testGasEvidenceErc6909Take`) names the arm, with the
    /// same setup the sibling MINT test runs.
    function testGasEvidenceErc6909Take() public {
        emit GasEvidence("erc6909/take", _runTakeArm());
    }

    /// Per-variant gas label for the MINT arm.
    function testGasEvidenceErc6909Mint() public {
        emit GasEvidence("erc6909/mint", _runMintArm());
    }
}
