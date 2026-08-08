//! Shared Tier-3 V2-family byte-exact swap oracle driver.
//!
//! Both the Uniswap V2 oracle (`tier3_v2_pair_swap_vs_revm.rs`) and the
//! PancakeSwap V2 fork oracle (`tier3_pancake_v2_swap_vs_revm.rs`) drive the
//! same probe against REAL deployed pair bytecode: the pair is deployed in an
//! in-process revm `CacheDB`, seeded via `setup` (mint reserves + `sync` so
//! slot-8 reserves equal the live `balanceOf`, per ADR-020 D4), then
//! `pair.swap` is driven with the engine's `amountOut` passed as a PARAMETER
//! (the harness carries no swap math — see the harness `.sol`). The fork's
//! pair is either compiled into the harness (`pair_init_artifact: None`, the
//! canonical-source Uniswap V2 build) or deployed from PINNED on-chain
//! creation bytecode passed as the harness's `(bytes)` constructor arg
//! (`pair_init_artifact: Some(…)`, the Sourcify-verified PancakeSwap V2 pair —
//! see `tier3_pancake_v2_swap_vs_revm.rs`).
//!
//! Each case assembles **three independent proofs** of byte-exactness (in
//! increasing strength) via [`assert_byte_exact`](self::assert_byte_exact):
//!
//! 1. **Accept at the boundary with matched flows.** `pair.swap(engine_out)`
//!    succeeds, the pair's emitted `Swap` event decodes to exactly the
//!    engine's realized flows (input token in, output token out, the other two
//!    zero, recipient = harness), and the post-swap `getReserves()` equals the
//!    engine's predicted post-state. This proves the swap is REAL (tokens moved)
//!    and the pair's accounting is consistent with the engine's model.
//! 2. **Reject at `engine_out + 1` with the K-invariant `Error(string)` reason**
//!    — not merely "the call reverted for some reason", but the revert data
//!    decodes to exactly the fork's K-check string (`UniswapV2: K` /
//!    `Pancake: K`). Closing that gap matters: the old accept/`+1`-reject
//!    probe only checked `!Success`, so a `+1` that failed for *any* unrelated
//!    reason would have passed.
//! 3. **Real deployed bytecode, not a Rust twin.** The engine's value is
//!    proven against the on-chain math (event + K-boundary + post-state),
//!    so an implementation bug *shared* by the engine and a re-derived Rust
//!    oracle is still caught — the Tier-3 design goal (ADR-020).
//!
//! The driver is parameterized only by the fork's fee
//! (`gamma_numer`/`fee_denom`), harness artifact name, and K-check error
//! string ([`V2Fork`]), so a V2 fork (Sushi, …) is a one-line declaration and
//! the two tests cannot drift (the HRT356 class).

#![allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
#![expect(clippy::expect_used, clippy::panic)]
use std::path::PathBuf;

use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use proptest::prelude::*;
use revm::context::TxEnv;
use revm::context_interface::result::{ExecutionResult, Output};
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

use degenbot_v2_math::IntHopState;

/// `uint112` max — the V2 pair's reserve width. The post-swap input-token
/// balance must stay at or below this or the pair's `_update` reverts
/// `OVERFLOW` (masking the byte-exact K-boundary), so every generated case
/// keeps `reserve + amount_in ≤ U112_MAX`.
const U112_MAX: u128 = (1u128 << 112) - 1;

/// Identifies one V2-family fork for the oracle. A fork differs only in its
/// hardcoded swap fee (the pair's K-check constants baked into bytecode) and
/// the error-string/reference strings the test prints.
pub struct V2Fork {
    /// Harness artifact file name (under `tier3-oracle/artifacts/`).
    pub harness_sol: &'static str,
    /// Harness contract name within that artifact.
    pub harness_contract: &'static str,
    /// Retained fraction numerator (e.g. 997 for Uniswap's 0.3%).
    pub gamma_numer: u64,
    /// Fee denominator (e.g. 1000 for Uniswap's 0.3%).
    pub fee_denom: u64,
    /// The exact Solidity require-reason of the pair's K-invariant check.
    pub k_error: &'static str,
    /// Optional pinned on-chain pair creation-bytecode artifact (path under
    /// `tier3-oracle/artifacts/`). When `Some`, the harness deploys the pair
    /// from these Sourcify-verified on-chain bytes (raw `create`) instead of a
    /// locally-compiled `new Pair()`; the harness's creation bytecode is then
    /// passed this init code as its `(bytes)` constructor arg. `None` = the
    /// harness embeds its own compiled pair (Uniswap V2's canonical source is
    /// a reproducible build).
    pub pair_init_artifact: Option<&'static str>,
}

/// The `Swap` event the pair emitted, hand-decoded from its ABI data.
#[derive(Clone, Debug)]
pub struct DecodedSwap {
    pub amount0_in: U256,
    pub amount1_in: U256,
    pub amount0_out: U256,
    pub amount1_out: U256,
    pub to: Address,
}

/// Outcome of a single pristine probe (deploy harness → setup → doSwap).
pub enum ProbeOutcome {
    /// `pair.swap` accepted the tested `amountOut`.
    Accepted {
        /// The decoded `Swap` event the pair emitted.
        swap: DecodedSwap,
        /// Post-swap `(reserve0, reserve1)` read back via `getReserves()`.
        post_reserves: (u128, u128),
    },
    /// `pair.swap` reverted; `reason` is the raw revert return-data.
    Reverted { reason: Bytes },
    /// The probe pipeline itself broke (deploy/setup/view failed or the EVM
    /// halted) — a fixture problem, not a math divergence.
    Halted(String),
}

/// First 4 bytes of `keccak256(signature)` — the Solidity function selector.
fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&h.0[0..4]);
    out
}

/// Repo path to a built harness artifact (foundry `out/<File>.sol/<Contract>.json`).
fn harness_artifact_path(file: &str, contract: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tier3-oracle/artifacts")
        .join(file)
        .join(format!("{contract}.json"))
}

/// Load the creation (`bytecode.object`) hex for a harness.
fn load_creation_bytecode(file: &str, contract: &str) -> Vec<u8> {
    let path = harness_artifact_path(file, contract);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing harness artifact {}: {e}", path.display()));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid harness JSON");
    let hex_str = v["bytecode"]["object"]
        .as_str()
        .expect("artifact has bytecode.object (creation)");
    hex::decode(hex_str.trim_start_matches("0x")).expect("hex creation bytecode")
}

/// Load a PINNED on-chain pair creation-bytecode artifact (a path relative to
/// `tier3-oracle/artifacts/`, e.g. `PancakeV2Pair/PancakeV2Pair.json`). The
/// artifact carries Sourcify `exact_match` provenance (see the committed JSON);
/// its `bytecode.object` is the deployable init code passed to the harness's
/// `(bytes pairInitCode)` constructor.
fn load_pair_creation_code(artifact: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tier3-oracle/artifacts")
        .join(artifact);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing pinned pair artifact {}: {e}", path.display()));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid pinned pair JSON");
    let hex_str = v["bytecode"]["object"]
        .as_str()
        .expect("pinned pair artifact has bytecode.object (creation)");
    hex::decode(hex_str.trim_start_matches("0x")).expect("hex pinned pair creation bytecode")
}

/// ABI-encode a Solidity `bytes` value as a single function argument
/// `(offset=0x20, length, data… padded to 32)`. Used to forward the pinned pair
/// init code to the harness's `(bytes pairInitCode)` constructor.
fn abi_encode_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + data.len().next_multiple_of(32));
    out.extend_from_slice(&U256::from(0x20u64).to_be_bytes::<32>()); // offset
    out.extend_from_slice(&U256::from(data.len()).to_be_bytes::<32>()); // length
    out.extend_from_slice(data);
    let rem = data.len() % 32;
    if rem != 0 {
        out.extend(std::iter::repeat_n(0u8, 32 - rem));
    }
    out
}

/// The engine's V2 `getAmountOut` at the fork's fee via `IntHopState::swap`.
pub fn engine_amount_out(
    fork: &V2Fork,
    reserve_in: U256,
    reserve_out: U256,
    amount_in: U256,
) -> U256 {
    IntHopState::new(reserve_in, reserve_out, fork.gamma_numer, fork.fee_denom)
        .swap(amount_in)
        .expect("engine swap does not overflow under bounded inputs")
}

/// `keccak256("Swap(address,uint256,uint256,uint256,uint256,address)")` —
/// identical across Uniswap V2 + forks (Pancake/Sushi share the event).
fn swap_topic() -> B256 {
    keccak256(b"Swap(address,uint256,uint256,uint256,uint256,address)")
}

/// Find and decode the pair's `Swap` event out of the execution logs.
///
/// Hand-slices the ABI `data` (4 × 32-byte words) and the indexed `sender`/`to`
/// topics, mirroring `degenbot_decoders::v2_swap_decoder`'s documented format
/// (kept local so the oracle doesn't depend on the decoder crate's
/// `alloy::rpc::types::Log` input type).
fn decode_swap_event(logs: &[revm::primitives::Log], pair: Address) -> Option<DecodedSwap> {
    let topic = swap_topic();
    for log in logs {
        if log.address != pair || log.topics().first() != Some(&topic) {
            continue;
        }
        let data = log.data.data.as_ref();
        if data.len() < 128 {
            continue;
        }
        let word = |i: usize| -> U256 {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&data[i * 32..(i + 1) * 32]);
            U256::from_be_bytes(buf)
        };
        return Some(DecodedSwap {
            amount0_in: word(0),
            amount1_in: word(1),
            amount0_out: word(2),
            amount1_out: word(3),
            to: Address::from_word(log.topics()[2]),
        });
    }
    None
}

/// Run a single pristine swap attempt: deploy a fresh harness, `setup(r0,r1)`
/// (mint reserves + sync), then `doSwap(amount_in, zfo, amount_out)` with
/// `recipient = harness`. Fully self-contained: each call rebuilds the evm +
/// harness so reserves/balances are pristine (a `doSwap` mutates them, so
/// reuse would compound and break the K-boundary crispness).
#[expect(clippy::too_many_lines)] // one logical deploy → setup → swap → read pipeline
pub fn probe(
    fork: &V2Fork,
    r0: u128,
    r1: u128,
    amount_in: U256,
    zfo: bool,
    amount_out: U256,
) -> ProbeOutcome {
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;

    // 1. Deploy the harness (mock tokens + the real pair fork). If the fork
    // pins its pair (Sourcify on-chain bytecode), forward that creation code as
    // the harness's `(bytes)` constructor arg so the harness raw-creates it.
    let mut init_code = load_creation_bytecode(fork.harness_sol, fork.harness_contract);
    if let Some(pair_artifact) = fork.pair_init_artifact {
        let pair_code = load_pair_creation_code(pair_artifact);
        init_code.extend_from_slice(&abi_encode_bytes(&pair_code));
    }
    let deploy_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Create)
                .gas_limit(16_700_000)
                .data(Bytes::from(init_code))
                .build()
                .expect("deploy tx"),
        )
        .expect("deploy transact");
    let harness = match &deploy_res.result {
        ExecutionResult::Success {
            output: Output::Create(_, Some(addr)),
            ..
        } => *addr,
        other => return ProbeOutcome::Halted(format!("harness deploy did not create: {other:?}")),
    };
    evm.commit(deploy_res.state);

    // 2. setup(r0, r1): mint reserves + sync (slot-8 reserves == balanceOf).
    let mut setup_call = selector("setup(uint112,uint112)").to_vec();
    setup_call.extend_from_slice(&U256::from(r0).to_be_bytes::<32>());
    setup_call.extend_from_slice(&U256::from(r1).to_be_bytes::<32>());
    let setup_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(6_000_000)
                .data(Bytes::from(setup_call))
                .build()
                .expect("setup tx"),
        )
        .expect("setup transact");
    if !matches!(&setup_res.result, ExecutionResult::Success { .. }) {
        return ProbeOutcome::Halted(format!("setup failed: {:?}", setup_res.result));
    }
    evm.commit(setup_res.state);

    // 3. Resolve the pair address (public `pair()` storage getter).
    let pair_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("pair()").to_vec()))
                .build()
                .expect("pair() tx"),
        )
        .expect("pair() transact");
    let pair = match &pair_res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&b.as_ref()[0..32]);
            Address::from_slice(&buf[12..32])
        }
        other => return ProbeOutcome::Halted(format!("pair() failed: {other:?}")),
    };
    evm.commit(pair_res.state);

    // 4. doSwap(amount_in, zfo, amount_out, recipient=harness).
    let recipient = harness;
    let mut swap_call = selector("doSwap(uint256,bool,uint256,address)").to_vec();
    swap_call.extend_from_slice(&amount_in.to_be_bytes::<32>());
    swap_call.extend_from_slice(&U256::from(zfo).to_be_bytes::<32>());
    swap_call.extend_from_slice(&amount_out.to_be_bytes::<32>());
    // address: right-aligned in 32 bytes (12 zero bytes prefix + 20-byte address).
    swap_call.extend_from_slice(&[0u8; 12]);
    swap_call.extend_from_slice(recipient.as_slice());
    let swap_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(6_000_000)
                .data(Bytes::from(swap_call))
                .build()
                .expect("doSwap tx"),
        )
        .expect("doSwap transact");

    // Consume the result by value so we can commit the swap state (which a
    // `doSwap` mutates) BEFORE reading post-swap reserves — otherwise the
    // getReserves() read would see the stale pre-swap state.
    let outcome = match swap_res.result {
        ExecutionResult::Success { logs, .. } => {
            let swap = decode_swap_event(&logs, pair).unwrap_or_else(|| {
                panic!("pair {pair} did not emit a decodable Swap event on a successful swap")
            });
            assert_eq!(swap.to, harness, "Swap recipient must be the harness");
            evm.commit(swap_res.state);
            // Post-swap reserves (reserve0, reserve1) from getReserves().
            let gr_res = evm
                .transact(
                    TxEnv::builder()
                        .kind(TxKind::Call(pair))
                        .gas_limit(2_000_000)
                        .data(Bytes::from(selector("getReserves()").to_vec()))
                        .build()
                        .expect("getReserves tx"),
                )
                .expect("getReserves transact");
            let out = match gr_res.result {
                ExecutionResult::Success {
                    output: Output::Call(b),
                    ..
                } => b,
                other => return ProbeOutcome::Halted(format!("getReserves() failed: {other:?}")),
            };
            evm.commit(gr_res.state);
            let word = |i: usize| -> U256 {
                let mut buf = [0u8; 32];
                buf.copy_from_slice(&out.as_ref()[i * 32..(i + 1) * 32]);
                U256::from_be_bytes(buf)
            };
            ProbeOutcome::Accepted {
                swap,
                post_reserves: (word(0).to::<u128>(), word(1).to::<u128>()),
            }
        }
        ExecutionResult::Revert { output, .. } => {
            evm.commit(swap_res.state);
            ProbeOutcome::Reverted { reason: output }
        }
        ExecutionResult::Halt { reason, .. } => {
            evm.commit(swap_res.state);
            ProbeOutcome::Halted(format!("swap halted: {reason:?}"))
        }
    };
    outcome
}

/// Decode a Solidity `Error(string)` revert payload to its message string.
///
/// Returns `None` for anything that isn't an `Error(string)` ABI encoding
/// (`0x08c379a0`, offset 0x20, length, zero-padded bytes) — e.g. a bare
/// reasonless revert or a `Panic(uint256)`.
fn decode_error_string(reason: &[u8]) -> Option<String> {
    if reason.len() < 4 || reason[..4] != [0x08, 0xc3, 0x79, 0xa0] {
        return None;
    }
    let data = &reason[4..];
    if data.len() < 64 {
        return None;
    }
    let len = U256::from_be_bytes::<32>(data[32..64].try_into().ok()?).to::<usize>();
    // Solidity's Error(string) uses offset 0x20, so the payload starts at byte
    // 64 and runs `len` bytes, zero-padded to a 32-byte boundary.
    if 64 + len > data.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&data[64..64 + len]).into_owned())
}

/// Assert the accepted swap's event flows match the engine's realized flows.
fn assert_swap_event_matches(swap: &DecodedSwap, zfo: bool, amount_in: U256, engine_out: U256) {
    if zfo {
        assert_eq!(swap.amount0_in, amount_in, "amount0In");
        assert_eq!(swap.amount1_in, U256::ZERO, "amount1In");
        assert_eq!(swap.amount0_out, U256::ZERO, "amount0Out");
        assert_eq!(swap.amount1_out, engine_out, "amount1Out");
    } else {
        assert_eq!(swap.amount0_in, U256::ZERO, "amount0In");
        assert_eq!(swap.amount1_in, amount_in, "amount1In");
        assert_eq!(swap.amount0_out, engine_out, "amount0Out");
        assert_eq!(swap.amount1_out, U256::ZERO, "amount1Out");
    }
}

/// Assert the pair's post-swap reserves equal the engine's predicted state
/// (input token reserve grew by the input, output token reserve shrank by the
/// output) — the state a degenbot V2 updater reads back from `getReserves()`.
fn assert_post_state(post_reserves: (u128, u128), r0: u128, r1: u128, swap: &DecodedSwap) {
    let expected0 = U256::from(r0) + swap.amount0_in - swap.amount0_out;
    let expected1 = U256::from(r1) + swap.amount1_in - swap.amount1_out;
    assert_eq!(U256::from(post_reserves.0), expected0, "post reserve0");
    assert_eq!(U256::from(post_reserves.1), expected1, "post reserve1");
}

/// The full byte-exact oracle for one case, comprising the three proofs above:
/// engine's `amountOut` is accepted with matched flows + post-state, and
/// `amountOut + 1` is rejected specifically with the fork's K-invariant error.
///
/// `amount_in` must be ≥ 1 (callers' strategies/corpora guarantee this).
pub fn assert_byte_exact(fork: &V2Fork, r0: u128, r1: u128, amount_in: u128, zfo: bool) {
    let amount_in_u = U256::from(amount_in);
    let (reserve_in, reserve_out) = if zfo { (r0, r1) } else { (r1, r0) };
    let engine_out = engine_amount_out(
        fork,
        U256::from(reserve_in),
        U256::from(reserve_out),
        amount_in_u,
    );
    assert!(
        engine_out + U256::from(1u64) <= U256::from(reserve_out),
        "engine_out +1 must be ≤ reserve_out {reserve_out}"
    );

    // Degenerate: the pool cannot produce a positive output at this input
    // (the fee dwarfs the tiny reserves), so `getAmountOut` floors to 0 and
    // `pair.swap` rejects a 0-amount output with `INSUFFICIENT_OUTPUT_AMOUNT`
    // before the K-check. There is no maximal-output boundary to pin, so the
    // byte-exact proofs are undefined — skip (mirrors the original degenerate
    // `amount_in == 0` skip).
    if engine_out.is_zero() {
        return;
    }

    // 1. engine_out is accepted, with realized flows + post-state matching.
    match probe(fork, r0, r1, amount_in_u, zfo, engine_out) {
        ProbeOutcome::Accepted {
            swap,
            post_reserves,
        } => {
            assert_swap_event_matches(&swap, zfo, amount_in_u, engine_out);
            assert_post_state(post_reserves, r0, r1, &swap);
        }
        ProbeOutcome::Reverted { reason } => panic!(
            "engine_out should be accepted; reverted with {:?}",
            decode_error_string(reason.as_ref())
        ),
        ProbeOutcome::Halted(m) => panic!("probe halted: {m}"),
    }

    // 2. engine_out + 1 must revert SPECIFICALLY with the K-invariant error.
    match probe(
        fork,
        r0,
        r1,
        amount_in_u,
        zfo,
        engine_out + U256::from(1u64),
    ) {
        ProbeOutcome::Accepted { .. } => panic!("engine_out + 1 must be rejected"),
        ProbeOutcome::Reverted { reason } => {
            let got = decode_error_string(reason.as_ref())
                .unwrap_or_else(|| format!("raw {:02x?}", reason.as_ref()));
            assert_eq!(
                got, fork.k_error,
                "revert must be the K-invariant check ({}), got {got}",
                fork.k_error
            );
        }
        ProbeOutcome::Halted(m) => panic!("probe halted: {m}"),
    }
}

// ── Proptest strategies ──────────────────────────────────────────────

/// Nominal wide dynamic range, with amount covering 1 wei, ~reserve − 1, the
/// midpoint, and a uniform sweep. Reserves ≤ 1e18 keep `reserve + amount`
/// well under `uint112`-max.
fn nominal_reserve_arm() -> impl Strategy<Value = (u128, u128, u128, bool)> {
    (
        1_000_000u128..1_000_000_000_000_000_000u128,
        1_000_000u128..1_000_000_000_000_000_000u128,
    )
        .prop_flat_map(|(r0, r1)| {
            let m = r0.min(r1);
            let amount = prop_oneof![
                Just(1u128), // 1 wei
                Just(m - 1), // near reserve − 1
                Just(m / 2), // midpoint
                1u128..=m,   // uniform sweep
            ];
            (Just(r0), Just(r1), amount, any::<bool>())
        })
}

/// Single-digit / tiny reserves — the rounding- and floor-div-sensitive
/// region where an off-by-one in the EVM `DIV` would surface.
fn tiny_reserve_arm() -> impl Strategy<Value = (u128, u128, u128, bool)> {
    (1u128..10_000u128, 1u128..10_000u128).prop_flat_map(|(r0, r1)| {
        let m = r0.min(r1);
        let amount = prop_oneof![Just(1u128), 1u128..=m];
        (Just(r0), Just(r1), amount, any::<bool>())
    })
}

/// Reserves near `uint112`-max with wei-scale inputs — the other numeric
/// extreme (huge reserves, tiny swap) where integer round-down of `amountOut`
/// is the dominant concern. Bounds keep `reserve + amount ≤ U112_MAX` so the
/// pair's `_update` never reverts `OVERFLOW`.
fn max_reserve_arm() -> impl Strategy<Value = (u128, u128, u128, bool)> {
    (
        (U112_MAX - 10_000_000_000u128)..(U112_MAX - 1_000_000u128),
        (U112_MAX - 10_000_000_000u128)..(U112_MAX - 1_000_000u128),
    )
        .prop_flat_map(|(r0, r1)| {
            let amount = prop_oneof![Just(1u128), 1u128..1_000_000u128];
            (Just(r0), Just(r1), amount, any::<bool>())
        })
}

/// Strategy producing `(r0, r1, amount_in, zfo)` over the V2 swap's
/// numerically-risky regions, never overflowing the pair's `uint112` reserve
/// width. `amount_in` is always ≥ 1 (zero-input degenerates are excluded — the
/// K-boundary `+1` proof is undefined at `getAmountOut(0) = 0`).
pub fn fork_case_strategy() -> impl Strategy<Value = (u128, u128, u128, bool)> {
    prop_oneof![nominal_reserve_arm(), tiny_reserve_arm(), max_reserve_arm(),]
}
