//! The bribe axis's one seam: `EncodeOptions.bribe` reaches the `execute()`
//! `config` `uint256` and nothing else.
//!
//! No family declares `bribe` as a stream axis (`tests/honesty_invariant.rs`
//! probes that), so a bribe dropped between the option and the packed config
//! would be a silent no-op configuration. These tests pin the whole path —
//! option → `config_for_options` → the packed word → the `execute(bytes,uint256)`
//! calldata the adapter submits — plus the typed refusals for values the
//! contract cannot honour. The on-chain half (the contract pays the recipient
//! and reports `bribe_paid`) is covered by the release's forge bribe suites and
//! the live `config_for_options_bribe_axis_reaches_contract_and_pays` leg.

#![expect(clippy::expect_used, reason = "must-encode fixture paths fail loudly")]

use alloy::primitives::{address, Address, Bytes, U256};
use alloy::sol_types::SolCall as _;
use degenbot_execution::{HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};
use degenbot_executor::composers::{
    config_for_options, encode_cmd_stream, encode_execute_call, EncodeContext, EncodeOptions,
    EncodeRequest, ICommandExecutor::executeCall,
};
use degenbot_executor::config::{pack_config, ConfigError};
use degenbot_executor::grammar_ledger::{Bribe, ProfitCapture};

const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
const PM: Address = address!("000000000004444c5dc75cB358380D2e3De08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");

const OPTIMAL: u128 = 1_000_000_000_000_000_000;
const MIN_PROFIT: U256 = U256::from_limbs([7, 0, 0, 0]);

/// `bribe_bips` occupies bits 8–23 and `bribe_recipient_idx` bits 24–31.
const BIPS_MASK: U256 = U256::from_limbs([0xFF_FF << 8, 0, 0, 0]);
const RECIPIENT_MASK: U256 = U256::from_limbs([0xFF << 24, 0, 0, 0]);

fn bribe(bips: u16, recipient_idx: u8) -> Bribe {
    Bribe::Some {
        bips,
        recipient_idx,
    }
}

fn two_hop_v2_v3() -> PathInfo {
    PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: address!("000000000000000000000000000000000000DE66"),
            token0_address: WETH,
            token1_address: USDC,
            fee: 30,
            zfo: true,
        }),
        HopInfo::V3(V3HopInfo {
            pool_address: address!("000000000000000000000000000000000000DEA3"),
            token0_address: WETH,
            token1_address: USDC,
            fee: 3000,
            zfo: true,
        }),
    ])
}

fn v4_hop(c0: Address, c1: Address) -> HopInfo {
    HopInfo::V4(V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x0".into(),
        currency0_address: c0,
        currency1_address: c1,
        fee: 3000,
        tick_spacing: 60,
        hook_address: Address::ZERO,
        zfo: true,
    })
}

/// The command stream the adapter submits for `path` under `opts`. Both fixture
/// families are must-encode shapes (`tests/encode_intake.rs`), so a decline here
/// is a fixture break rather than an axis outcome.
fn commands(path: PathInfo, opts: EncodeOptions) -> Vec<u8> {
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let req = EncodeRequest::new(
        path,
        OPTIMAL,
        vec![OPTIMAL, OPTIMAL],
        vec![OPTIMAL - 1, OPTIMAL - 1],
        opts,
    );
    encode_cmd_stream(&ctx, &req).expect("the fixture family encodes")
}

fn config(bribe: Bribe) -> U256 {
    config_for_options(
        EncodeOptions {
            bribe,
            ..Default::default()
        },
        MIN_PROFIT,
    )
    .expect("in-range bribe packs")
}

#[test]
fn the_stream_is_byte_identical_across_every_bribe_value() {
    let families = [
        ("v2_v3", two_hop_v2_v3()),
        (
            "v4_v4",
            PathInfo::new(vec![v4_hop(WETH, USDC), v4_hop(USDC, WETH)]),
        ),
    ];
    for (family, path) in families {
        let baseline = commands(path.clone(), EncodeOptions::default());
        for value in [bribe(1, 0), bribe(5_000, 7), bribe(10_000, 31), Bribe::None] {
            assert_eq!(
                commands(
                    path.clone(),
                    EncodeOptions {
                        bribe: value,
                        ..Default::default()
                    }
                ),
                baseline,
                "[{family}] a bribe must not reach the command stream: {value:?}"
            );
        }
    }
}

#[test]
fn a_nonzero_bribe_reaches_the_execute_calldata_config_word() {
    let plain = commands(two_hop_v2_v3(), EncodeOptions::default());
    let bribed = commands(
        two_hop_v2_v3(),
        EncodeOptions {
            bribe: bribe(500, 3),
            ..Default::default()
        },
    );
    assert_eq!(bribed, plain, "the bribe changes no stream byte");

    let plain_config = config(Bribe::None);
    let bribed_config = config(bribe(500, 3));
    assert_ne!(
        plain_config, bribed_config,
        "the bribe changes the config word"
    );

    let plain_call = encode_execute_call(EXECUTOR, &plain, plain_config);
    let bribed_call = encode_execute_call(EXECUTOR, &bribed, bribed_config);
    assert_eq!(bribed_call.to, plain_call.to);
    assert_eq!(bribed_call.value, plain_call.value);

    let decoded_plain = executeCall::abi_decode(&plain_call.data).expect("execute() calldata");
    let decoded_bribed = executeCall::abi_decode(&bribed_call.data).expect("execute() calldata");
    assert_eq!(decoded_bribed.commands, Bytes::from(bribed));
    assert_eq!(decoded_bribed.commands, decoded_plain.commands);
    assert_eq!(
        decoded_bribed.config,
        pack_config(1, MIN_PROFIT, 500, 3).expect("in range")
    );
    assert_eq!(
        decoded_plain.config,
        pack_config(1, MIN_PROFIT, 0, 0).expect("in range")
    );
    assert_eq!(
        decoded_bribed.config & BIPS_MASK,
        U256::from(500u64) << U256::from(8u64)
    );
    assert_eq!(
        decoded_bribed.config & RECIPIENT_MASK,
        U256::from(3u64) << U256::from(24u64)
    );
    assert_eq!(
        decoded_bribed.config >> U256::from(32u64),
        MIN_PROFIT,
        "the bribe leaves min_profit untouched"
    );
}

#[test]
fn recipient_zero_is_coinbase_and_one_to_31_indexes_the_address_table() {
    let coinbase = config(bribe(250, 0));
    let table = config(bribe(250, 31));
    assert_eq!(
        coinbase & RECIPIENT_MASK,
        U256::ZERO,
        "idx 0 = block.coinbase"
    );
    assert_eq!(
        table & RECIPIENT_MASK,
        U256::from(31u64) << U256::from(24u64),
        "idx 1..=31 selects a table entry"
    );
    assert_eq!(coinbase & BIPS_MASK, table & BIPS_MASK);
    assert_eq!(coinbase & U256::from(0xFFu64), table & U256::from(0xFFu64));
}

#[test]
fn the_upper_bound_packs_and_out_of_range_values_are_typed_refusals() {
    assert!(
        config_for_options(
            EncodeOptions {
                bribe: bribe(10_000, 31),
                ..Default::default()
            },
            MIN_PROFIT
        )
        .is_ok(),
        "10_000 bips (100%) to table entry 31 is the accepted upper bound"
    );
    let refuse = |value: Bribe| {
        config_for_options(
            EncodeOptions {
                bribe: value,
                ..Default::default()
            },
            MIN_PROFIT,
        )
    };
    assert_eq!(
        refuse(bribe(10_001, 0)),
        Err(ConfigError::BribeBipsOutOfRange(10_001))
    );
    assert_eq!(
        refuse(bribe(0, 32)),
        Err(ConfigError::BribeRecipientOutOfRange(32))
    );
}

#[test]
fn every_nonzero_bips_value_changes_the_packed_word() {
    let no_bribe = config(Bribe::None);
    assert_eq!(
        config(bribe(0, 0)),
        no_bribe,
        "0 bips requests a zero share — nothing is paid, same word as no bribe"
    );
    for bips in [1u16, 7, 500, 5_000, 9_999, 10_000] {
        for recipient_idx in [0u8, 1, 31] {
            assert_ne!(
                config(bribe(bips, recipient_idx)),
                no_bribe,
                "a nonzero bips bribe must reach the config word ({bips} bips, idx {recipient_idx})"
            );
        }
    }
}

#[test]
fn bribe_composes_with_the_capture_axis_without_displacing_it() {
    let cfg = config_for_options(
        EncodeOptions {
            capture: ProfitCapture::Erc6909,
            bribe: bribe(500, 4),
            ..Default::default()
        },
        MIN_PROFIT,
    )
    .expect("in-range bribe packs");
    assert_eq!(
        cfg & U256::from(0xFFu64),
        U256::from(2u64),
        "ERC6909 capture keeps profit_mode=2"
    );
    assert_eq!(cfg & BIPS_MASK, U256::from(500u64) << U256::from(8u64));
    assert_eq!(cfg & RECIPIENT_MASK, U256::from(4u64) << U256::from(24u64));
}
