#![expect(
    clippy::expect_used,
    reason = "adapter fixtures are valid by construction"
)]

use alloy::primitives::{address, Address, Bytes, U256};
use degenbot_execution::{solve_result::HopDescriptor, SolveResult};
use degenbot_executor::composers::{
    config_for_options, encode_cmd_stream, encode_execute_call, EncodeContext, EncodeOptions,
    EncodeRequest, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};
use degenbot_executor::grammar_ledger::{Bribe, FundingSource, ProfitCapture};
use degenbot_strategy::cmd_executor_adapter::{
    CmdExecutorAdapter, CmdExecutorDecline, CmdExecutorOutcome, CmdExecutorRejection,
};

const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
const PM: Address = address!("000000000004444c5dc75cB358380D2e3De08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");

fn options(bribe_bips: u16) -> EncodeOptions {
    EncodeOptions {
        erc6909_profit: false,
        use_v4_batch: false,
        funding: FundingSource::InPathFlash,
        capture: ProfitCapture::Custody,
        bribe: Bribe::Some {
            bips: bribe_bips,
            recipient_idx: 0,
        },
    }
}

fn solve_result(
    path: &PathInfo,
    optimal_input: u128,
    outputs: Vec<u128>,
    consumed: Vec<u128>,
) -> SolveResult {
    SolveResult {
        path_id: 7,
        hop_count: path.hops.len(),
        optimal_input: U256::from(optimal_input),
        hop_outputs: outputs.into_iter().map(U256::from).collect(),
        consumed_inputs: consumed.into_iter().map(U256::from).collect(),
        net_profit: U256::ZERO,
        hop_descriptors: path.hops.iter().map(HopDescriptor::from_hop_info).collect(),
    }
}

fn v2_path() -> PathInfo {
    PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: address!("00000000000000000000000000000000000000a2"),
            token0_address: WETH,
            token1_address: USDC,
            fee: 30,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: address!("00000000000000000000000000000000000000a3"),
            token0_address: USDC,
            token1_address: WETH,
            fee: 30,
            zfo: true,
        }),
    ])
}

fn reference_calldata(
    ctx: EncodeContext,
    path: &PathInfo,
    result: &SolveResult,
    opts: EncodeOptions,
) -> Bytes {
    let request = EncodeRequest::new(
        path.clone(),
        u128::try_from(result.optimal_input).expect("fixture fits u128"),
        result
            .hop_outputs
            .iter()
            .map(|value| u128::try_from(*value).expect("fixture fits u128"))
            .collect(),
        result
            .consumed_inputs
            .iter()
            .map(|value| u128::try_from(*value).expect("fixture fits u128"))
            .collect(),
        opts,
    );
    let commands = encode_cmd_stream(&ctx, &request).expect("fixture encodes");
    let config = config_for_options(opts, U256::ZERO).expect("fixture config packs");
    Bytes::from(
        encode_execute_call(ctx.executor, &commands, config)
            .expect("fixture execute call encodes")
            .data,
    )
}

fn v3_path() -> PathInfo {
    PathInfo::new(vec![
        HopInfo::V3(V3HopInfo {
            pool_address: address!("00000000000000000000000000000000000000b2"),
            token0_address: WETH,
            token1_address: USDC,
            fee: 3_000,
            zfo: true,
        }),
        HopInfo::V3(V3HopInfo {
            pool_address: address!("00000000000000000000000000000000000000b3"),
            token0_address: USDC,
            token1_address: WETH,
            fee: 3_000,
            zfo: true,
        }),
    ])
}

#[test]
fn composes_v3_calldata_through_session_adapter() {
    let path = v3_path();
    let result = solve_result(
        &path,
        1_000_000_000_000_000_000,
        vec![1_000_000_000_000_000_000; 2],
        vec![999_999_999_999_999_999; 2],
    );
    let opts = options(500);
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let expected = reference_calldata(ctx, &path, &result, opts);

    let outcome = CmdExecutorAdapter::new(ctx).compose(&path, &result, opts);

    assert_eq!(outcome, CmdExecutorOutcome::Encoded(expected));
}

fn v4_path(pool_manager: Address) -> PathInfo {
    PathInfo::new(vec![
        HopInfo::V4(V4HopInfo {
            pool_manager_address: pool_manager,
            pool_id_hex: "0x01".into(),
            currency0_address: WETH,
            currency1_address: USDC,
            fee: 500,
            tick_spacing: 10,
            hook_address: Address::ZERO,
            zfo: true,
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: pool_manager,
            pool_id_hex: "0x02".into(),
            currency0_address: USDC,
            currency1_address: WETH,
            fee: 500,
            tick_spacing: 10,
            hook_address: Address::ZERO,
            zfo: true,
        }),
    ])
}

#[test]
fn composes_v4_calldata_using_the_session_pool_manager() {
    let path = v4_path(PM);
    let result = solve_result(
        &path,
        1_000_000_000_000_000_000,
        vec![1_000_000_000_000_000_000, 1_200_000_000_000_000_000],
        vec![999_999_999_999_999_999; 2],
    );
    let opts = options(250);
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let expected = reference_calldata(ctx, &path, &result, opts);

    let outcome = CmdExecutorAdapter::new(ctx).compose(&path, &result, opts);

    assert_eq!(outcome, CmdExecutorOutcome::Encoded(expected));
}

#[test]
fn routine_declines_keep_the_existing_jsonl_labels() {
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let adapter = CmdExecutorAdapter::new(ctx);

    let one_hop = PathInfo::new(vec![v2_path().hops[0].clone()]);
    let one_hop_result = solve_result(&one_hop, 1, vec![1], vec![1]);
    assert_eq!(
        adapter.compose(&one_hop, &one_hop_result, options(0)),
        CmdExecutorOutcome::Declined(CmdExecutorDecline::UnsupportedHopShape)
    );
    assert_eq!(
        CmdExecutorDecline::UnsupportedHopShape.label(),
        "unsupported_hop_shape"
    );

    let mut too_wide = solve_result(&v2_path(), 1, vec![1, 1], vec![1, 1]);
    too_wide.optimal_input = U256::from(1u128 << 96);
    assert_eq!(
        adapter.compose(&v2_path(), &too_wide, options(0)),
        CmdExecutorOutcome::Declined(CmdExecutorDecline::AmountExceedsUint96)
    );
    assert_eq!(
        CmdExecutorDecline::AmountExceedsUint96.label(),
        "amount_exceeds_uint96"
    );

    let four_v3 = PathInfo::new(vec![
        v3_path().hops[0].clone(),
        v3_path().hops[1].clone(),
        v3_path().hops[0].clone(),
        v3_path().hops[1].clone(),
    ]);
    let four_result = solve_result(&four_v3, 100, vec![100; 4], vec![99; 4]);
    assert_eq!(
        adapter.compose(&four_v3, &four_result, options(0)),
        CmdExecutorOutcome::Declined(CmdExecutorDecline::CommandStream)
    );
    assert_eq!(
        CmdExecutorDecline::CommandStream.label(),
        "encoding_failed:cmd_stream"
    );

    let v2 = v2_path();
    let v2_result = solve_result(&v2, 100, vec![100, 100], vec![99, 99]);
    assert_eq!(
        adapter.compose(&v2, &v2_result, options(10_001)),
        CmdExecutorOutcome::Declined(CmdExecutorDecline::ExecuteCall)
    );
    assert_eq!(
        CmdExecutorDecline::ExecuteCall.label(),
        "encoding_failed:execute_call"
    );

    let mismatched_v4 = v4_path(address!("000000000000000000000000000000000000dead"));
    let mismatched_result = solve_result(&mismatched_v4, 100, vec![100, 100], vec![99, 99]);
    assert_eq!(
        adapter.compose(&mismatched_v4, &mismatched_result, options(0)),
        CmdExecutorOutcome::Declined(CmdExecutorDecline::MixedPoolManagers)
    );
    assert_eq!(
        CmdExecutorDecline::MixedPoolManagers.label(),
        "mixed_pool_managers"
    );
}

#[test]
fn validator_rejection_is_not_a_routine_decline() {
    let path = v2_path();
    let result = solve_result(&path, 100_000, vec![80_000, 60_000], vec![50_000, 80_000]);
    let adapter = CmdExecutorAdapter::new(EncodeContext::new(EXECUTOR, PM, WETH));

    assert_eq!(
        adapter.compose(&path, &result, options(0)),
        CmdExecutorOutcome::Rejected(CmdExecutorRejection::LedgerValidation)
    );
}

#[test]
fn same_session_adapter_recomposes_for_a_lower_bribe() {
    let path = v2_path();
    let result = solve_result(
        &path,
        1_000_000_000_000_000_000,
        vec![1_000_000_000_000_000_000; 2],
        vec![999_999_999_999_999_999; 2],
    );
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let adapter = CmdExecutorAdapter::new(ctx);
    let high = options(1_000);
    let low = options(400);

    let first = adapter.compose(&path, &result, high);
    let second = adapter.compose(&path, &result, low);

    assert_ne!(first, second);
    assert_eq!(
        second,
        CmdExecutorOutcome::Encoded(reference_calldata(ctx, &path, &result, low))
    );
    assert_eq!(adapter.context(), &ctx);
}

#[test]
fn composes_v2_calldata_through_session_adapter() {
    let path = v2_path();
    let result = solve_result(
        &path,
        1_000_000_000_000_000_000,
        vec![1_000_000_000_000_000_000; 2],
        vec![999_999_999_999_999_999; 2],
    );
    let opts = options(1_000);
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let expected = reference_calldata(ctx, &path, &result, opts);

    let outcome = CmdExecutorAdapter::new(ctx).compose(&path, &result, opts);

    assert_eq!(outcome, CmdExecutorOutcome::Encoded(expected));
}
