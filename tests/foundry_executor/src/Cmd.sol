// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// Command-stream encoder for the `cmd_executor` byte-stream VM.
///
/// Port of `tests/executor/conftest_shared.py`'s `enc_*` helpers and the
/// `AddressTable` sentinel resolution. Every field width here is a fact about
/// `contracts/vyper/cmd_executor.vy`; the sizes are asserted by the tests.
library Cmd {
    // ── Opcodes ───────────────────────────────────────────────────────────
    uint8 internal constant SET_ADDRESS = 0x00;

    uint8 internal constant ERC20_TRANSFER = 0x10;
    uint8 internal constant ERC20_XFER_BALANCE = 0x11;
    uint8 internal constant WETH_DEPOSIT = 0x12;
    uint8 internal constant WETH_WITHDRAW = 0x13;
    uint8 internal constant WETH_DEPOSIT_ALL = 0x14;
    uint8 internal constant WETH_WITHDRAW_ALL = 0x15;
    uint8 internal constant SEND_ETH = 0x16;
    uint8 internal constant SEND_ETH_ALL = 0x17;

    uint8 internal constant V2_SWAP_COMPACT = 0x20;
    uint8 internal constant V2_SWAP_CALC = 0x21;
    uint8 internal constant V2_SWAP_DIRECT = 0x22;

    uint8 internal constant V3_SWAP_COMPACT = 0x30;
    uint8 internal constant V3_SWAP_DELTA = 0x31;
    uint8 internal constant V3_SWAP_ONE_FOR_ZERO = 0x33;
    uint8 internal constant V3_SWAP_ZERO_FOR_ONE = 0x34;

    uint8 internal constant V4_SWAP_COMPACT = 0x40;
    uint8 internal constant V4_SWAP_DYNAMIC = 0x41;
    uint8 internal constant V4_BATCH = 0x42;

    /// `V4_BATCH_OPEN_WETH`: identical 0x42 layout; the tail settle leaves a
    /// POSITIVE WETH delta open for a trailing `V4_MINT_COMPACT` (the
    /// executor's ERC6909-capture pairing). A negative WETH delta still
    /// settles, and a positive delta left open without a consuming mint
    /// reverts at PM.unlock (`CurrencyNotSettled`).
    uint8 internal constant V4_BATCH_OPEN_WETH = 0x43;

    uint8 internal constant V4_UNLOCK = 0x50;
    uint8 internal constant V4_TAKE = 0x51;
    uint8 internal constant V4_TAKE_COMPACT = 0x52;
    uint8 internal constant V4_TAKE_DELTA = 0x53;
    uint8 internal constant V4_SYNC = 0x54;
    uint8 internal constant V4_SETTLE = 0x55;
    uint8 internal constant V4_SETTLE_DELTA = 0x56;
    uint8 internal constant V4_SETTLE_ALL = 0x57;
    uint8 internal constant V4_MINT_COMPACT = 0x58;
    uint8 internal constant V4_BURN_COMPACT = 0x59;

    uint8 internal constant BEGIN_EXECUTION = 0xFF;

    // ── Reserved address-table indices ────────────────────────────────────
    uint8 internal constant SENTINEL_USER0 = 0xFA;
    uint8 internal constant SENTINEL_USER1 = 0xFB;
    uint8 internal constant SENTINEL_PM = 0xFC;
    uint8 internal constant SENTINEL_SELF = 0xFD;
    uint8 internal constant SENTINEL_WETH = 0xFE;
    uint8 internal constant SENTINEL_NATIVE = 0xFF;

    struct Table {
        address[32] addrs;
        uint256 count;
        address weth;
        address executor;
        address pm;
        address user0;
        address user1;
    }

    struct BatchSwap {
        uint8 c0;
        uint8 c1;
        uint16 fee;
        int16 tickSpacing;
        uint8 hooks;
        bool zeroForOne;
        uint256 amount;
    }

    // ── Field encoding ────────────────────────────────────────────────────

    /// Big-endian `n`-byte truncation of an unsigned value.
    function be(uint256 value, uint256 n) internal pure returns (bytes memory out) {
        bytes32 word = bytes32(value << (256 - 8 * n));
        out = new bytes(n);
        for (uint256 i = 0; i < n; i++) {
            out[i] = word[i];
        }
    }

    /// Big-endian `n`-byte two's-complement truncation of a signed value.
    function beSigned(int256 value, uint256 n) internal pure returns (bytes memory out) {
        uint256 mask = (uint256(1) << (8 * n)) - 1;
        return be(uint256(value) & mask, n);
    }

    function flag(bool value) internal pure returns (bytes memory) {
        return value ? bytes("\x01") : bytes("\x00");
    }

    // ── Address table ─────────────────────────────────────────────────────

    function newTable(address weth, address executor, address pm)
        internal
        pure
        returns (Table memory t)
    {
        t.weth = weth;
        t.executor = executor;
        t.pm = pm;
    }

    /// A table that resolves only the two user-token sentinels (marked `0xFA`
    /// and `0xFB`); every other address takes an appended index.
    function tokenTable(address user0, address user1) internal pure returns (Table memory t) {
        t.user0 = user0;
        t.user1 = user1;
    }

    /// Resolve `addr` to its table index, or to the reserved sentinel that
    /// skips a SET_ADDRESS entry entirely.
    function add(Table memory t, address addr) internal pure returns (uint8) {
        if (addr == address(0)) return SENTINEL_NATIVE;
        if (t.weth != address(0) && addr == t.weth) return SENTINEL_WETH;
        if (t.executor != address(0) && addr == t.executor) return SENTINEL_SELF;
        if (t.pm != address(0) && addr == t.pm) return SENTINEL_PM;
        if (t.user0 != address(0) && addr == t.user0) return SENTINEL_USER0;
        if (t.user1 != address(0) && addr == t.user1) return SENTINEL_USER1;
        for (uint256 i = 0; i < t.count; i++) {
            if (t.addrs[i] == addr) return uint8(i);
        }
        require(t.count < 32, "address table full");
        t.addrs[t.count] = addr;
        t.count += 1;
        return uint8(t.count - 1);
    }

    function setAddress(address addr) internal pure returns (bytes memory) {
        return abi.encodePacked(SET_ADDRESS, addr);
    }

    /// Preprocessing section: one SET_ADDRESS per table entry, then the 0xFF
    /// separator that marks the start of execution.
    function preamble(Table memory t) internal pure returns (bytes memory out) {
        for (uint256 i = 0; i < t.count; i++) {
            out = bytes.concat(out, setAddress(t.addrs[i]));
        }
        return bytes.concat(out, bytes.concat(bytes1(BEGIN_EXECUTION)));
    }

    // ── Config ────────────────────────────────────────────────────────────

    /// `(min_profit << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | mode`
    function makeConfig(uint256 mode, uint16 bribeBips, uint8 bribeRecipientIdx, uint256 minProfit)
        internal
        pure
        returns (uint256)
    {
        require(mode == 1 || mode == 2, "bad mode");
        require(bribeBips <= 10_000, "bad bips");
        require(bribeRecipientIdx <= 31, "bad bribe idx");
        return (minProfit << 32) | (uint256(bribeRecipientIdx) << 24) | (uint256(bribeBips) << 8) | mode;
    }

    // ── V2 ────────────────────────────────────────────────────────────────

    function v2SwapCompact(
        uint8 pool,
        bool zeroForOne,
        uint256 amountOut,
        uint8 recipient,
        uint16 fee,
        bytes memory forward
    ) internal pure returns (bytes memory) {
        return abi.encodePacked(
            V2_SWAP_COMPACT,
            pool,
            flag(zeroForOne),
            be(amountOut, 12),
            recipient,
            be(fee, 2),
            be(forward.length, 1),
            forward
        );
    }

    function v2SwapCalc(uint8 pool, bool zeroForOne, uint8 recipient, uint16 fee)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(V2_SWAP_CALC, pool, flag(zeroForOne), recipient, be(fee, 2));
    }

    function v2SwapDirect(uint8 pool, bool zeroForOne, uint256 amountOut, uint8 recipient)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(V2_SWAP_DIRECT, pool, flag(zeroForOne), be(amountOut, 12), recipient);
    }

    // ── V3 ────────────────────────────────────────────────────────────────

    function v3SwapCompact(
        uint8 pool,
        bool zeroForOne,
        uint256 amountSpecified,
        uint8 recipient,
        bytes memory forward
    ) internal pure returns (bytes memory) {
        uint8 op = zeroForOne ? V3_SWAP_ZERO_FOR_ONE : V3_SWAP_ONE_FOR_ZERO;
        return abi.encodePacked(
            op, pool, be(amountSpecified, 12), recipient, be(forward.length, 1), forward
        );
    }

    /// The raw `V3_SWAP_COMPACT` (0x30) form. One byte longer than the
    /// direction-specific 0x33/0x34 layouts (17+N vs 16+N): the direction is
    /// carried in-band as the `zeroForOne` flag byte instead of in the opcode.
    function v3SwapCompactRaw(
        uint8 pool,
        bool zeroForOne,
        uint256 amountSpecified,
        uint8 recipient,
        bytes memory forward
    ) internal pure returns (bytes memory) {
        return abi.encodePacked(
            V3_SWAP_COMPACT,
            pool,
            flag(zeroForOne),
            be(amountSpecified, 12),
            recipient,
            be(forward.length, 1),
            forward
        );
    }

    function v3SwapDelta(uint8 pool, bool zeroForOne, uint8 recipient)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(V3_SWAP_DELTA, pool, flag(zeroForOne), recipient);
    }

    // ── V4 ────────────────────────────────────────────────────────────────

    function v4SwapCompact(
        uint8 c0,
        uint8 c1,
        uint16 fee,
        int16 tickSpacing,
        uint8 hooks,
        bool zeroForOne,
        uint256 amount
    ) internal pure returns (bytes memory) {
        return abi.encodePacked(
            V4_SWAP_COMPACT,
            c0,
            c1,
            be(fee, 2),
            beSigned(tickSpacing, 2),
            hooks,
            flag(zeroForOne),
            be(amount, 12)
        );
    }

    function v4SwapDynamic(
        uint8 c0,
        uint8 c1,
        uint16 fee,
        int16 tickSpacing,
        uint8 hooks,
        bool zeroForOne
    ) internal pure returns (bytes memory) {
        return abi.encodePacked(
            V4_SWAP_DYNAMIC, c0, c1, be(fee, 2), beSigned(tickSpacing, 2), hooks, flag(zeroForOne)
        );
    }

    function v4Batch(BatchSwap[] memory swaps) internal pure returns (bytes memory) {
        return _v4Batch(V4_BATCH, swaps);
    }

    function v4BatchOpenWeth(BatchSwap[] memory swaps) internal pure returns (bytes memory) {
        return _v4Batch(V4_BATCH_OPEN_WETH, swaps);
    }

    /// Shared 0x42/0x43 body: `[op][count:1][entry:20]*` with every entry
    /// `[c0:1][c1:1][fee:2][ts:2][hooks:1][zfo:1][amount:12]`. An `amount` of
    /// zero is the dynamic form (amount from the PM exttload delta).
    function _v4Batch(uint8 opcode, BatchSwap[] memory swaps) private pure returns (bytes memory out) {
        out = abi.encodePacked(opcode, be(swaps.length, 1));
        for (uint256 i = 0; i < swaps.length; i++) {
            BatchSwap memory s = swaps[i];
            out = bytes.concat(
                out,
                abi.encodePacked(
                    s.c0,
                    s.c1,
                    be(s.fee, 2),
                    beSigned(s.tickSpacing, 2),
                    s.hooks,
                    flag(s.zeroForOne),
                    be(s.amount, 12)
                )
            );
        }
    }

    function v4Unlock(bytes memory forward) internal pure returns (bytes memory) {
        return abi.encodePacked(V4_UNLOCK, be(forward.length, 1), forward);
    }

    function v4Take(uint8 currency, uint8 recipient, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(V4_TAKE, currency, recipient, be(amount, 32));
    }

    function v4TakeDelta(uint8 currency, uint8 recipient) internal pure returns (bytes memory) {
        return abi.encodePacked(V4_TAKE_DELTA, currency, recipient);
    }

    function v4TakeCompact(uint8 currency, uint8 recipient, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(V4_TAKE_COMPACT, currency, recipient, be(amount, 12));
    }

    function v4MintCompact(uint8 currency, uint8 recipient, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(V4_MINT_COMPACT, currency, recipient, be(amount, 12));
    }

    function v4BurnCompact(uint8 currency, uint256 amount) internal pure returns (bytes memory) {
        return abi.encodePacked(V4_BURN_COMPACT, currency, be(amount, 12));
    }

    function v4Sync(uint8 currency) internal pure returns (bytes memory) {
        return abi.encodePacked(V4_SYNC, currency);
    }

    function v4Settle() internal pure returns (bytes memory) {
        return abi.encodePacked(V4_SETTLE);
    }

    function v4SettleDelta(uint8 currency) internal pure returns (bytes memory) {
        return abi.encodePacked(V4_SETTLE_DELTA, currency);
    }

    function v4SettleAll() internal pure returns (bytes memory) {
        return abi.encodePacked(V4_SETTLE_ALL);
    }

    // ── ERC20 / WETH / ETH ────────────────────────────────────────────────

    function erc20Transfer(uint8 token, uint8 recipient, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(ERC20_TRANSFER, token, recipient, be(amount, 12));
    }

    function erc20XferBalance(uint8 token, uint8 recipient) internal pure returns (bytes memory) {
        return abi.encodePacked(ERC20_XFER_BALANCE, token, recipient);
    }

    function wethDeposit(uint256 amount) internal pure returns (bytes memory) {
        return abi.encodePacked(WETH_DEPOSIT, be(amount, 32));
    }

    function wethDepositAll() internal pure returns (bytes memory) {
        return abi.encodePacked(WETH_DEPOSIT_ALL);
    }

    function wethWithdraw(uint256 amount) internal pure returns (bytes memory) {
        return abi.encodePacked(WETH_WITHDRAW, be(amount, 32));
    }

    function wethWithdrawAll() internal pure returns (bytes memory) {
        return abi.encodePacked(WETH_WITHDRAW_ALL);
    }

    function sendEth(uint8 recipient, uint256 amount) internal pure returns (bytes memory) {
        return abi.encodePacked(SEND_ETH, recipient, be(amount, 12));
    }

    function sendEthAll(uint8 recipient) internal pure returns (bytes memory) {
        return abi.encodePacked(SEND_ETH_ALL, recipient);
    }
}
