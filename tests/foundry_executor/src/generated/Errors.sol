// SPDX-License-Identifier: MIT

// Generated from the pinned-vyper CmdExecutor compile by build_artifacts.py — do not edit.
//
// The executor lane's revert-identity vocabulary, generated rather than
// maintained by hand a second time:
//   - custom-error selectors come from artifacts/CmdExecutor.json's ABI entries;
//   - the compiler revert-label vocabulary comes from the tracked release
//     cmd_executor.error_map.json, cross-checked against this lane's fresh
//     compile on every artifact rebuild.
//
// Compiler revert-label vocabulary (cmd_executor.error_map.json):
//   safediv -> empty-or-invalid
//   safemod -> empty-or-invalid
//   safemul -> empty-or-invalid
//   user assert -> empty-or-invalid
//   user revert with custom error -> custom
//
// The compiler-emitted check labels revert with empty revert data or the
// INVALID opcode — they carry no selector. Only the executor's `raise`
// statements produce selectable custom errors, listed below.
library ExecutorErrors {
    // Standard revert envelopes.
    bytes4 internal constant ERROR_STRING = 0x08c379a0; // Error(string)
    bytes4 internal constant PANIC = 0x4e487b71; // Panic(uint256)
    uint256 internal constant PANIC_ARITHMETIC = 0x11;

    bytes4 internal constant UNAUTHORIZED = 0x8e4a23d6; // Unauthorized(address)
    bytes4 internal constant INVALID_CALLBACK = 0xb028a63a; // InvalidCallback(address)
    bytes4 internal constant INSUFFICIENT_BALANCE = 0xcf479181; // InsufficientBalance(uint256,uint256)
    bytes4 internal constant INSUFFICIENT_PROFIT = 0x4e88422a; // InsufficientProfit(uint256,uint256)
    bytes4 internal constant INSUFFICIENT_MINT_DELTA = 0xfd81bdee; // InsufficientMintDelta(uint256,uint256)
    bytes4 internal constant INVALID_COMMAND = 0x83276224; // InvalidCommand(uint256)
    bytes4 internal constant INVALID_PROFIT_MODE = 0x3e0c87dc; // InvalidProfitMode(uint256)
    bytes4 internal constant BIPS_TOO_HIGH = 0x60ef0bb0; // BipsTooHigh(uint256)
    bytes4 internal constant INVALID_MSG_VALUE = 0xa61be9f0; // InvalidMsgValue(uint256)
    bytes4 internal constant NOT_PLAIN_ETH_TRANSFER = 0xe5b6bf32; // NotPlainEthTransfer()

    /// Name a revert selector from the generated vocabulary, or "unknown".
    function nameOf(bytes4 selector) internal pure returns (string memory) {
        if (selector == ERROR_STRING) return "(Error)";
        if (selector == PANIC) return "(Panic)";
        if (selector == UNAUTHORIZED) return "Unauthorized";
        if (selector == INVALID_CALLBACK) return "InvalidCallback";
        if (selector == INSUFFICIENT_BALANCE) return "InsufficientBalance";
        if (selector == INSUFFICIENT_PROFIT) return "InsufficientProfit";
        if (selector == INSUFFICIENT_MINT_DELTA) return "InsufficientMintDelta";
        if (selector == INVALID_COMMAND) return "InvalidCommand";
        if (selector == INVALID_PROFIT_MODE) return "InvalidProfitMode";
        if (selector == BIPS_TOO_HIGH) return "BipsTooHigh";
        if (selector == INVALID_MSG_VALUE) return "InvalidMsgValue";
        if (selector == NOT_PLAIN_ETH_TRANSFER) return "NotPlainEthTransfer";
        return "unknown";
    }
}
