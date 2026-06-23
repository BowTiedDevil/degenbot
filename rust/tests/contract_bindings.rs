use alloy::sol_types::SolCall;
use degenbot_rs::contract_bindings;
use degenbot_rs::types::executor::{
    composeFourLegCall, executeNativeArbCall, matchInternalCall, transferToSettlementCall,
    triggerCoWFlashLoanRouterCall,
};

#[test]
fn generated_executor_bindings_are_wired_into_degenbot_rs() {
    // Bind against the `IExecutor` interface (the monolith `Executor` was
    // archived at P8; the diamond implements this same interface). Couples the
    // generated 12-field ADR-029 selectors to the hand-maintained sol! mirror.
    use contract_bindings::i_executor::IExecutor;

    assert_eq!(
        IExecutor::executeNativeArbCall::SELECTOR,
        executeNativeArbCall::SELECTOR,
    );
    assert_eq!(
        IExecutor::matchInternalCall::SELECTOR,
        matchInternalCall::SELECTOR,
    );
    assert_eq!(
        IExecutor::composeFourLegCall::SELECTOR,
        composeFourLegCall::SELECTOR,
    );
    assert_eq!(
        IExecutor::triggerCoWFlashLoanRouterCall::SELECTOR,
        triggerCoWFlashLoanRouterCall::SELECTOR,
    );
    assert_eq!(
        IExecutor::transferToSettlementCall::SELECTOR,
        transferToSettlementCall::SELECTOR,
    );
}

#[test]
fn generated_selected_binding_set_exposes_hot_path_modules() {
    // 20 generated binding modules after the archived monolith executors
    // (Executor / AtomicExecutor / LiquidationExecutor) were dropped in favour
    // of the `IExecutor` interface. Injected by the generator from the actual
    // `pub mod` count (see `foundry_bindings.inject_selected_binding_count`).
    assert_eq!(20, contract_bindings::SELECTED_CONTRACT_BINDING_COUNT);
    assert_eq!(
        [0x98, 0xb6, 0xd7, 0xda],
        contract_bindings::multi_hop_caller::MultiHopCaller::swapWithAutoSlippageCall::SELECTOR,
    );
    assert_eq!(
        [0x80, 0x8c, 0x50, 0xc4],
        <contract_bindings::router_registry::RouterRegistry::UnknownRouterKind as alloy::sol_types::SolError>::SELECTOR,
    );
}
