//! Honesty-invariant test (candidate 4, `DDNEAB`, riding `3BTR22`).
//!
//! Turns the per-family axis-support declaration into an ENFORCED invariant:
//! for every family row the axes the builder ACTUALLY branches on in its body
//! must equal the axes the row DECLARES (`family_axis_support`). The drift the
//! review names — "the builder that silently ignores an axis can no longer
//! hide" — breaks the equality.
//!
//! **Probe (runtime, not a static fixture):** for each family, encode the
//! stream under each axis value and diff the produced bytes. If the bytes
//! differ when an axis toggles, the builder honors that axis; if identical, it
//! doesn't. The None-decline partition counts as a distinct outcome (a
//! decline-vs-encode toggle is honoring too). Inputs are realistic-but-fixed
//! (the `glopcn_bytepin` fixture shape: WETH/USDC/WBTC cycle, 1e18 amounts) —
//! no production RPC state.
//!
//! If this test fails, do NOT "fix" it by editing the declaration to match a
//! builder, nor the builder to match the declaration — the coordinator decides
//! which side is the truth (per `3BTR22`: the declaration is the intended
//! truth; a builder honoring an un-declared axis is a latent bug; a builder
//! NOT honoring a declared axis is a declaration bug).

#![allow(
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::unreadable_literal,
    clippy::unusual_byte_groupings,
    clippy::panic
)]

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    ComposerInputs, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};
use degenbot_executor::grammar_ledger::{AxisSupport, Bribe, FundingSource, ProfitCapture, Prot};
use degenbot_executor::grammar_shape::{derive_shape, family_axis_support};

const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
const PM: Address = address!("000000000004444c5dc75cB358380D2e3De08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");

/// FNV-1a 64-bit (deterministic across runs/processes — no `RandomState`).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1_0000_0001_b3);
    }
    h
}

/// A `None` encode (the builder declines) is a distinct probe outcome — the
/// encode-vs-None partition is part of honoring (e.g. `v4_v4` + Native on a
/// non-WETH terminal declines).
const DECLINE: u64 = 0xDEAD_BEEF_DEAD_BEEF;

/// Fixed baseline amounts (the `glopcn_bytepin` shape): a 1e18 entry, 1e18
/// hop outputs, and consumed inputs one wei under (so every family encodes).
static OPTIMAL: u128 = 1_000_000_000_000_000_000;
static OUTS: [u128; 3] = [1_000_000_000_000_000_000; 3];
static CONSUMED: [u128; 3] = [999_999_999_999_999_999; 3];

/// Build the family's hops over the WETH/USDC/WBTC cycle (hop `i` is
/// `cycle[i%3] → cycle[(i+1)%3]`), mirroring the `glopcn_bytepin` fixtures so
/// every family encodes under the baseline amounts.
fn combo_hops(prots: &[Prot]) -> Vec<HopInfo> {
    (0..prots.len())
        .map(|i| {
            let (in_t, out_t) = (
                match i % 3 {
                    0 => WETH,
                    1 => USDC,
                    _ => WBTC,
                },
                match (i + 1) % 3 {
                    0 => WETH,
                    1 => USDC,
                    _ => WBTC,
                },
            );
            match prots[i] {
                Prot::V2 => HopInfo::V2(V2HopInfo {
                    pool_address: Address::from([0xA0 + i as u8; 20]),
                    token0_address: in_t,
                    token1_address: out_t,
                    fee: 30,
                    zfo: true,
                }),
                Prot::V3 => HopInfo::V3(V3HopInfo {
                    pool_address: Address::from([0xB0 + i as u8; 20]),
                    token0_address: in_t,
                    token1_address: out_t,
                    fee: 3000,
                    zfo: true,
                }),
                Prot::V4 => HopInfo::V4(V4HopInfo {
                    pool_manager_address: PM,
                    pool_id_hex: format!("0x{i:02x}"),
                    currency0_address: in_t,
                    currency1_address: out_t,
                    fee: 500,
                    tick_spacing: 10,
                    hook_address: Address::ZERO,
                    zfo: true,
                }),
            }
        })
        .collect()
}

/// Encode the family under `opts`, hashing the produced stream (or `DECLINE`).
fn encode_hash(prots: &[Prot], opts: EncodeOptions) -> u64 {
    let path = PathInfo::new(combo_hops(prots));
    let n = prots.len();
    let inputs = ComposerInputs {
        executor_address: EXECUTOR,
        pool_manager_address: PM,
        weth_address: WETH,
        optimal_input: OPTIMAL,
        hop_outputs: &OUTS[..n],
        consumed_inputs: &CONSUMED[..n],
        opts,
    };
    match derive_shape(&path, &inputs) {
        Some(bytes) => fnv1a(&bytes),
        None => DECLINE,
    }
}

/// Runtime-probe the axes a family's builder actually branches on in the
/// stream: toggle each axis off its baseline and diff the bytes (a decline
/// counts as a distinct outcome).
fn probe_axes(prots: &[Prot]) -> AxisSupport {
    let base = encode_hash(prots, EncodeOptions::default());
    let funding = encode_hash(
        prots,
        EncodeOptions {
            funding: FundingSource::SelfFund,
            ..Default::default()
        },
    );
    let capture_erc6909 = encode_hash(
        prots,
        EncodeOptions {
            capture: ProfitCapture::Erc6909,
            ..Default::default()
        },
    );
    let capture_native = encode_hash(
        prots,
        EncodeOptions {
            capture: ProfitCapture::Native,
            ..Default::default()
        },
    );
    let bribe = encode_hash(
        prots,
        EncodeOptions {
            bribe: Bribe::Some {
                bips: 100,
                recipient_idx: 7,
            },
            ..Default::default()
        },
    );
    AxisSupport {
        funding: funding != base,
        capture: capture_erc6909 != base || capture_native != base,
        bribe: bribe != base,
    }
}

#[test]
fn every_family_honors_exactly_the_axes_its_row_declares() {
    let fams = [Prot::V2, Prot::V3, Prot::V4];
    for n in [2usize, 3] {
        for fidx in 0..fams.len().pow(n as u32) {
            let prots: Vec<Prot> = (0..n)
                .map(|i| fams[(fidx / fams.len().pow(i as u32)) % fams.len()])
                .collect();
            let name = format!(
                "{n}hop_{}",
                prots
                    .iter()
                    .map(|p| match p {
                        Prot::V2 => "V2",
                        Prot::V3 => "V3",
                        Prot::V4 => "V4",
                    })
                    .collect::<Vec<_>>()
                    .join("_")
            );
            let path = PathInfo::new(combo_hops(&prots));
            let declared = family_axis_support(&path)
                .unwrap_or_else(|| panic!("[{name}] dispatch table must have a row"));
            let probed = probe_axes(&prots);
            assert_eq!(
                probed, declared,
                "[{name}] honesty invariant broken: the builder branches on \
                 {probed:?} in its stream, but the dispatch row declares \
                 {declared:?}. Do NOT fix by editing the declaration or the \
                 builder — report to the coordinator (declaration is the \
                 intended truth)."
            );
        }
    }
}
