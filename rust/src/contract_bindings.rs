//! Generated parent-contract bindings consumed by degenbot Rust code.
//!
//! The files under `rust/crates/contract_bindings` are generated from the
//! parent `contracts/src` Foundry workspace. This facade is the stable import
//! point for the Rust extension and host-side parity tests. Do not add this
//! dependency to Stylus WASM crates; Stylus uses native-test parity against this
//! host-side ABI surface instead.

pub use degenbot_contract_bindings::*;

// `SELECTED_CONTRACT_BINDING_COUNT` (the number of generated binding modules) is
// re-exported from the generated crate above. It is injected by
// `degenbot.devtools.foundry_bindings::inject_selected_binding_count` after
// `forge bind`, so it tracks the actual binding set automatically rather than
// being hand-maintained here.

/// Selected parent contract names included in the generated binding crate.
/// Mirrors `DEFAULT_SELECTED_CONTRACTS` in `degenbot.devtools.foundry_bindings`.
/// The standalone monolith executors (Executor / AtomicExecutor /
/// LiquidationExecutor) were archived at P8; the `IExecutor` interface,
/// implemented by the diamond facets, is bound instead.
pub const SELECTED_CONTRACT_BINDINGS: &[&str] = &[
    "IExecutor",
    "IFlashLoanRouter",
    "IFlashLoanReceiver",
    "IERC3156FlashBorrower",
    "IMorphoFlashLoanCallback",
    "IUniswapV3FlashCallback",
    "IReactorCallback",
    "IUniswapV4Hook",
    "MevPaymasterV9",
    "BaseMevPaymaster",
    "MevSafe",
    "MevBotDelegate",
    "StrategyLedger",
    "PermissionToken",
    "PathFinder",
    "IPathFinder",
    "MultiHopCaller",
    "RouterRegistry",
    "LpTransferLib",
    "TokenStandardIds",
    "TransientStorage",
];
