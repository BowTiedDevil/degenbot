#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]
//! The ADR-089 phase-6 offline engine benchmark: one corpus,
//! four engine modes, first-run and steady-state latency, plus the JIT
//! backend's compile telemetry.
//!
//! # Corpus
//!
//! The fixture spine mirrors the executor grammar harness (`crate::harness`,
//! revm-based): the REAL `cmd_executor` creation artifact plus the
//! `Token`/`Pair`/`PoolManager` stubs from `tier3-oracle/artifacts/`,
//! deployed as CREATE frames through the production-policy evm2 EVM and
//! committed. The workload is one encoded V2 two-hop cycle
//! (`encode_cmd_stream` → `execute()` calldata) replayed with the `discard`
//! verb, so every iteration re-executes identical real bytecode over an
//! identical warm read cache — the executor's 15KB of runtime code
//! dominates each run, which is exactly the code the JIT/AOT modes compile.
//!
//! # Modes
//!
//! - `interp`    — no interpreter runner (the stock evm2 interpreter).
//! - `jit-block` — `JitInterpreterRunner`, `blocking: true`: the first
//!   execution of each contract compiles synchronously (iteration 0 absorbs
//!   the full compile cost; the parity-gate-friendly mode).
//! - `jit-pool`  — the runner with `blocking: false`: frames interpret
//!   until the worker pool delivers the compiled program, then upgrade.
//! - `aot`       — the runner over a backend whose store is pre-populated
//!   via `prepare_aot_sync` for every contract the corpus deployed; the
//!   first timed iteration already runs compiled code. The prep wall time
//!   is reported separately (the build-time cost a production AOT lane
//!   would absorb once).
//!
//! Every mode asserts a run-to-run identical verdict (status, gas, output
//! hash) and the report folds a cross-mode verdict identity check — a
//! compiled mode that diverges from the interpreter is a correctness bug,
//! not a performance result.
//!
//! # Usage
//!
//! ```sh
//! cargo run --release -p degenbot-simulation --example sim_mode_bench \
//!   --features evm2-jit -- [all|interp|jit-block|jit-pool|aot]
//! ```
//!
//! `SIM_BENCH_ITERS` sets the iteration count (default 100).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use alloy::consensus::transaction::Recovered;
use alloy::consensus::TxEip1559;
use alloy::primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
use alloy::sol_types::{SolCall, SolValue};

use ::evm2::env::BlockEnvExt;
use ::evm2::ethereum::{ethereum_tx_registry, RecoveredTxEnvelope, TxEnvelope};
use ::evm2::evm::InMemoryDB;
use ::evm2::{BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, Version};

use ::evm2_jit::evm2_evm::JitInterpreterRunner;
use ::evm2_jit::runtime::{
    AotRequest, JitBackend, ProgramKind, RuntimeArtifactStore, RuntimeConfig,
};

use degenbot_executor::composers::{EncodeOptions, EncodeRequest, HopInfo, PathInfo, V2HopInfo};
use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;

alloy::sol! {
    /// The harness `Token` stub's funding surface.
    interface BenchToken {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
    }

    /// The harness V2 `Pair` stub's seed surface.
    interface BenchPair {
        function initialize(address tokenA, address tokenB) external;
        function sync() external;
    }
}

/// The deployer/owner of every fixture contract (and the `execute()` caller —
/// the executor's owner immutable is the creation sender).
const DEPLOYER: Address = Address::new([
    0xde, 0xb0, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
]);

const SETUP_GAS: u64 = 30_000_000;
const EXECUTE_GAS: u64 = 30_000_000;

/// The four engine modes under measurement.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Interp,
    JitBlock,
    JitPool,
    Aot,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Interp => "interp",
            Mode::JitBlock => "jit-block",
            Mode::JitPool => "jit-pool",
            Mode::Aot => "aot",
        }
    }
}

/// A deployed contract's live code identity — the `(code_hash, code)` pair
/// the JIT lookup keys on, captured from the creation's output.
struct Deployed {
    address: Address,
    code_hash: B256,
    code: Bytes,
}

/// The deployed fixture contracts.
struct Deployments {
    weth: Deployed,
    pool_manager: Deployed,
    executor: Deployed,
    token: Deployed,
    pair1: Deployed,
    pair2: Deployed,
}

impl Deployments {
    /// Every contract whose code the JIT/AOT modes compile.
    fn identities(&self) -> Vec<Deployed> {
        [
            &self.executor,
            &self.pair1,
            &self.pair2,
            &self.weth,
            &self.token,
        ]
        .into_iter()
        .map(|d| Deployed {
            address: d.address,
            code_hash: d.code_hash,
            code: d.code.clone(),
        })
        .collect()
    }
}

/// The seeded reserves, in (WETH, TKN) order per pool.
struct Reserves {
    pool1: (u128, u128),
    pool2: (u128, u128),
}

/// The built corpus: the deployed contracts and the one `execute()` frame
/// the timed loop replays.
struct Corpus {
    contracts: Vec<Deployed>,
    frame: RecoveredTxEnvelope,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

/// Read a hex-text artifact (`0x`-prefixed or bare) from the repo.
fn load_hex(rel: &str) -> Vec<u8> {
    let raw = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let text = raw
        .trim()
        .strip_prefix("0x")
        .unwrap_or(raw.trim())
        .to_string();
    alloy::hex::decode(&text).unwrap_or_else(|e| panic!("hex-decode {rel}: {e}"))
}

/// Load a foundry stub's creation bytecode (`bytecode.object`).
fn load_stub_creation(name: &str) -> Vec<u8> {
    let rel = format!("tier3-oracle/artifacts/harness/{name}.json");
    let raw = std::fs::read_to_string(repo_root().join(&rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    degenbot_simulation::oracle::parse_foundry_creation_bytecode(&raw)
        .unwrap_or_else(|e| panic!("parse {name}: {e}"))
}

fn envelope(from: Address, to: TxKind, data: Bytes, gas: u64, nonce: u64) -> RecoveredTxEnvelope {
    Recovered::new_unchecked(
        TxEnvelope::Eip1559(TxEip1559 {
            chain_id: 1,
            nonce,
            gas_limit: gas,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 0,
            to,
            value: U256::ZERO,
            input: data,
            ..Default::default()
        }),
        from,
    )
}

/// A V2 constant-product output quote (0.3% fee), in U256 to hold the
/// intermediate products.
fn v2_out(input: u128, reserve_in: u128, reserve_out: u128) -> u128 {
    let with_fee = U256::from(input) * U256::from(997u64);
    let num = with_fee * U256::from(reserve_out);
    let den = U256::from(reserve_in) * U256::from(1000u64) + with_fee;
    u128::try_from(num / den).expect("v2 quote fits u128")
}

/// Commit a creation frame; return the deployed contract's live identity.
/// The code bytes are the create's output (the constructor's returned
/// runtime code — the `CacheDB` stores code by hash, so the account changes
/// carry the hash but not the bytes); the hash is the lookup key the JIT
/// runner uses.
fn deploy(
    evm: &mut Evm<'_, BaseEvmTypes>,
    creation: Bytes,
    nonce: u64,
) -> Result<Deployed, String> {
    let frame = envelope(DEPLOYER, TxKind::Create, creation, SETUP_GAS, nonce);
    let executed = evm
        .transact(&frame)
        .map_err(|e| format!("deploy transact: {e:?}"))?;
    let result = executed.commit();
    if !result.status {
        return Err(format!("deploy reverted/halted: {:?}", result.stop));
    }
    let address = result
        .created_address
        .ok_or_else(|| "create returned no address".to_string())?;
    if result.output.is_empty() {
        return Err("create returned empty runtime code".to_string());
    }
    Ok(Deployed {
        address,
        code_hash: keccak256(&result.output),
        code: result.output,
    })
}

/// Commit a plain call frame; require success.
fn call_commit(
    evm: &mut Evm<'_, BaseEvmTypes>,
    from: Address,
    to: Address,
    data: Vec<u8>,
    nonce: u64,
) -> Result<(), String> {
    let frame = envelope(from, TxKind::Call(to), Bytes::from(data), SETUP_GAS, nonce);
    let executed = evm
        .transact(&frame)
        .map_err(|e| format!("setup call to {to}: {e:?}"))?;
    let result = executed.commit();
    if !result.status {
        return Err(format!("setup call to {to} failed: {:?}", result.stop));
    }
    Ok(())
}

/// Deploy WETH, the `PoolManager` stub, the real executor, one more token,
/// and the two pairs.
fn deploy_contracts(
    evm: &mut Evm<'_, BaseEvmTypes>,
    nonce: &mut u64,
) -> Result<Deployments, String> {
    let weth = deploy(evm, Bytes::from(load_stub_creation("Token")), take(nonce))?;
    let pool_manager = deploy(
        evm,
        Bytes::from(load_stub_creation("PoolManager")),
        take(nonce),
    )?;
    let mut executor_init = load_hex("tier3-oracle/artifacts/executor/cmd_executor.creation.hex");
    executor_init.extend_from_slice(&(weth.address, pool_manager.address).abi_encode());
    let executor = deploy(evm, Bytes::from(executor_init), take(nonce))?;
    let token = deploy(evm, Bytes::from(load_stub_creation("Token")), take(nonce))?;
    let pair1 = deploy(evm, Bytes::from(load_stub_creation("Pair")), take(nonce))?;
    let pair2 = deploy(evm, Bytes::from(load_stub_creation("Pair")), take(nonce))?;
    Ok(Deployments {
        weth,
        pool_manager,
        executor,
        token,
        pair1,
        pair2,
    })
}

fn take(nonce: &mut u64) -> u64 {
    let n = *nonce;
    *nonce += 1;
    n
}

/// Initialize both pairs, seed reserves in sorted-token order (pool 1 prices
/// WETH at 1000 TKN, pool 2 at 900 — the dislocation the cycle harvests),
/// fund the executor, and approve both pairs to pull either token.
fn seed_and_fund(
    evm: &mut Evm<'_, BaseEvmTypes>,
    d: &Deployments,
    nonce: &mut u64,
) -> Result<Reserves, String> {
    for pair in [&d.pair1, &d.pair2] {
        call_commit(
            evm,
            DEPLOYER,
            pair.address,
            BenchPair::initializeCall {
                tokenA: d.weth.address,
                tokenB: d.token.address,
            }
            .abi_encode(),
            take(nonce),
        )?;
    }
    let (t0, t1) = if d.weth.address < d.token.address {
        (d.weth.address, d.token.address)
    } else {
        (d.token.address, d.weth.address)
    };
    let reserves = Reserves {
        pool1: (
            1_000_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000_000_000u128,
        ),
        pool2: (
            1_000_000_000_000_000_000_000u128,
            900_000_000_000_000_000_000_000u128,
        ),
    };
    for (pair, (r_weth, r_tkn)) in [
        (d.pair1.address, reserves.pool1),
        (d.pair2.address, reserves.pool2),
    ] {
        let (r0, r1) = if t0 == d.weth.address {
            (r_weth, r_tkn)
        } else {
            (r_tkn, r_weth)
        };
        call_commit(
            evm,
            DEPLOYER,
            t0,
            BenchToken::mintCall {
                to: pair,
                amount: U256::from(r0),
            }
            .abi_encode(),
            take(nonce),
        )?;
        call_commit(
            evm,
            DEPLOYER,
            t1,
            BenchToken::mintCall {
                to: pair,
                amount: U256::from(r1),
            }
            .abi_encode(),
            take(nonce),
        )?;
        call_commit(
            evm,
            DEPLOYER,
            pair,
            BenchPair::syncCall {}.abi_encode(),
            take(nonce),
        )?;
    }
    call_commit(
        evm,
        DEPLOYER,
        d.weth.address,
        BenchToken::mintCall {
            to: d.executor.address,
            amount: U256::from(10_000_000_000_000_000_000u128),
        }
        .abi_encode(),
        take(nonce),
    )?;
    for pair in [&d.pair1, &d.pair2] {
        for token in [d.weth.address, d.token.address] {
            call_commit(
                evm,
                d.executor.address,
                token,
                BenchToken::approveCall {
                    spender: pair.address,
                    amount: U256::MAX,
                }
                .abi_encode(),
                0,
            )?;
        }
    }
    Ok(reserves)
}

/// Encode the two-hop cycle WETH → TKN (pool 1) → WETH (pool 2), with
/// solver amounts quoted off the seeded reserves.
fn encode_workload(
    d: &Deployments,
    reserves: &Reserves,
    nonce: u64,
) -> Result<RecoveredTxEnvelope, String> {
    let (t0, t1) = if d.weth.address < d.token.address {
        (d.weth.address, d.token.address)
    } else {
        (d.token.address, d.weth.address)
    };
    let optimal_input = 1_000_000_000_000_000u128; // 0.001 WETH
    let hop1_out = v2_out(optimal_input, reserves.pool1.0, reserves.pool1.1);
    let hop2_out = v2_out(hop1_out, reserves.pool2.1, reserves.pool2.0);
    let weth_zfo = d.weth.address < d.token.address;
    let hop = |pair: Address, zfo: bool| {
        HopInfo::V2(V2HopInfo {
            pool_address: pair,
            token0_address: t0,
            token1_address: t1,
            fee: 30,
            zfo,
        })
    };
    let path = PathInfo::new(vec![
        hop(d.pair1.address, weth_zfo),
        hop(d.pair2.address, !weth_zfo),
    ]);
    let cmd = degenbot_executor::composers::encode_cmd_stream(
        &degenbot_executor::composers::EncodeContext::new(
            d.executor.address,
            d.pool_manager.address,
            d.weth.address,
        ),
        &EncodeRequest::new(
            path,
            optimal_input,
            vec![hop1_out, hop2_out],
            vec![optimal_input, hop1_out],
            EncodeOptions::default(),
        ),
    )
    .ok_or_else(|| "encode_cmd_stream returned None".to_string())?;
    let data =
        degenbot_executor::composers::encode_execute_call(Address::ZERO, &cmd, U256::ZERO).data;
    Ok(envelope(
        DEPLOYER,
        TxKind::Call(d.executor.address),
        Bytes::from(data),
        EXECUTE_GAS,
        nonce,
    ))
}

/// Build the corpus: deploy, seed, encode — all committed, all untimed.
fn build_corpus(evm: &mut Evm<'_, BaseEvmTypes>) -> Result<Corpus, String> {
    let mut nonce = 0u64;
    let deployments = deploy_contracts(evm, &mut nonce)?;
    let reserves = seed_and_fund(evm, &deployments, &mut nonce)?;
    let frame = encode_workload(&deployments, &reserves, nonce)?;
    Ok(Corpus {
        contracts: deployments.identities(),
        frame,
    })
}

/// A fresh production-policy EVM over a funded deployer account. Mirrors
/// the handle's version policy with the fixture deltas: the EIP-7825
/// per-tx cap lifted (the executor creation artifact + init exceeds the
/// 2^24 default — the revm grammar harness lifts it too), the nonce/balance
/// checks and EIP-3607 off (the corpus's approval frames originate from
/// the executor contract itself, which holds no native funds).
fn bench_evm() -> Evm<'static, BaseEvmTypes> {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &DEPLOYER,
        ::evm2::evm::AccountInfo::default().with_balance(U256::from(u128::MAX)),
    );
    let block_env = BlockEnvExt {
        basefee: U256::from(1_000_000_000u64),
        number: U256::from(100u64),
        ..Default::default()
    };
    let mut version = Version::new(EVM2_PRODUCTION_SPEC);
    version.features.remove(EvmFeatures::NONCE_CHECK);
    version.features.remove(EvmFeatures::BLOCK_GAS_LIMIT_CHECK);
    version.features.remove(EvmFeatures::EIP3607);
    version.features.remove(EvmFeatures::BALANCE_CHECK);
    version.tx_gas_limit_cap = u64::MAX;
    let evm = Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(EVM2_PRODUCTION_SPEC, version),
        EVM2_PRODUCTION_SPEC,
        block_env,
        ethereum_tx_registry(EVM2_PRODUCTION_SPEC),
        db,
        Precompiles::base(EVM2_PRODUCTION_SPEC),
    );
    // Inspector-free by design: evm2's frame dispatch is inspector XOR
    // runner (an active inspector suppresses the interpreter-runner path
    // entirely — pin 86fa2648 and head d6c7e21 alike). The bench measures
    // raw engine cost across the runner modes; capture overhead is a
    // separate axis, and the production spine must resolve the composition
    // before JIT can run with capture on.
    evm
}

/// The per-mode verdict: one `execute()` run's observable identity.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
struct Verdict {
    status: bool,
    gas: u64,
    output: B256,
}

struct ModeReport {
    mode: &'static str,
    first_us: u128,
    p50_us: u128,
    p99_us: u128,
    /// `jit-pool` only: steady-state p50 AFTER the worker pool delivered
    /// every corpus program, plus how long the upgrade took from loop end.
    post_p50_us: Option<u128>,
    upgrade_wait_ms: Option<u128>,
    verdict: Verdict,
    lookup_hits: u64,
    lookup_misses: u64,
    compilations: u64,
    aot_prep_ms: Option<u128>,
}

/// The per-mode backend: none for the interpreter; blocking for the
/// synchronous mode; the worker pool for `jit-pool`; the pool plus a
/// (temp-dir) artifact store for `aot`.
fn make_backend(mode: Mode) -> Option<JitBackend> {
    let config = |blocking: bool| RuntimeConfig {
        enabled: true,
        blocking,
        ..Default::default()
    };
    match mode {
        Mode::Interp => None,
        Mode::JitBlock => Some(JitBackend::new(config(true)).expect("jit backend init")),
        Mode::JitPool => Some(JitBackend::new(config(false)).expect("jit backend init")),
        Mode::Aot => Some(
            JitBackend::new(RuntimeConfig {
                store: Some(Arc::new(
                    RuntimeArtifactStore::new().expect("artifact store"),
                )),
                ..config(false)
            })
            .expect("jit backend init"),
        ),
    }
}

/// `aot` mode: pre-compile every corpus contract into the store (timed and
/// reported — the build-time cost a production AOT lane absorbs once), and
/// prove each produced a `ProgramKind::Aot` program (the P0 key check, in
/// executable form).
fn aot_prepare(backend: &JitBackend, contracts: &[Deployed]) -> Result<u128, String> {
    let prep = Instant::now();
    for contract in contracts {
        backend
            .prepare_aot_sync(AotRequest {
                code_hash: contract.code_hash,
                code: contract.code.clone(),
                spec_id: EVM2_PRODUCTION_SPEC,
            })
            .map_err(|e| format!("prepare_aot_sync {:x}: {e}", contract.code_hash))?;
        let compiled = backend
            .get_compiled(contract.code_hash, EVM2_PRODUCTION_SPEC)
            .ok_or_else(|| {
                format!(
                    "aot prep {:x} did not produce a program",
                    contract.code_hash
                )
            })?;
        if compiled.kind != ProgramKind::Aot {
            return Err(format!(
                "aot prep {:x} produced {:?}",
                contract.code_hash, compiled.kind
            ));
        }
    }
    Ok(prep.elapsed().as_millis())
}

/// The timed discard loop: `iters` runs of the corpus frame, every verdict
/// identical to the first.
fn timed_loop(
    evm: &mut Evm<'_, BaseEvmTypes>,
    frame: &RecoveredTxEnvelope,
    iters: usize,
) -> Result<(Vec<u128>, Verdict), String> {
    let mut samples: Vec<u128> = Vec::with_capacity(iters);
    let mut verdict: Option<Verdict> = None;
    for _ in 0..iters {
        let t = Instant::now();
        let executed = evm
            .transact(frame)
            .map_err(|e| format!("timed frame: {e:?}"))?;
        let result = executed.discard();
        samples.push(t.elapsed().as_micros());
        let v = Verdict {
            status: result.status,
            gas: result.total_gas_spent,
            output: keccak256(&result.output),
        };
        match verdict {
            None => verdict = Some(v),
            Some(prev) if prev != v => {
                return Err(format!("run-to-run verdict drift: {prev:?} → {v:?}"));
            }
            _ => {}
        }
    }
    Ok((samples, verdict.expect("one iteration ran")))
}

/// `jit-pool`: the timed loop usually ends before the worker pool delivers
/// (the compiles cost hundreds of ms; the loop costs a few). Wait for the
/// upgrade, then measure the post-upgrade steady state — the honest
/// async-mode number. Returns `(upgrade wait ms, post-upgrade p50 µs)`.
fn post_upgrade_measure(
    backend: &JitBackend,
    contracts: &[Deployed],
    evm: &mut Evm<'_, BaseEvmTypes>,
    frame: &RecoveredTxEnvelope,
) -> Result<(u128, u128), String> {
    let wait = Instant::now();
    let deadline = std::time::Duration::from_secs(30);
    for contract in contracts {
        while backend
            .get_compiled(contract.code_hash, EVM2_PRODUCTION_SPEC)
            .is_none()
        {
            if wait.elapsed() > deadline {
                return Err(format!(
                    "jit-pool upgrade never landed for {:x}",
                    contract.code_hash
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    let wait_ms = wait.elapsed().as_millis();
    let mut post: Vec<u128> = Vec::with_capacity(20);
    for _ in 0..20 {
        let t = Instant::now();
        let executed = evm
            .transact(frame)
            .map_err(|e| format!("post-upgrade frame: {e:?}"))?;
        let _ = executed.discard();
        post.push(t.elapsed().as_micros());
    }
    post.sort_unstable();
    Ok((wait_ms, post[post.len() / 2]))
}

/// Run one mode end to end: fresh EVM, corpus build (untimed), optional AOT
/// prep (timed, reported separately), then the timed discard loop.
fn run_mode(mode: Mode, iters: usize) -> Result<ModeReport, String> {
    let mut evm = bench_evm();
    let corpus = build_corpus(&mut evm)?;
    let backend = make_backend(mode);
    let mut aot_prep_ms = None;
    if let Some(backend) = &backend {
        if mode == Mode::Aot {
            aot_prep_ms = Some(aot_prepare(backend, &corpus.contracts)?);
        }
        evm.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
    }

    let stats_before = backend.as_ref().map(JitBackend::stats);
    let (samples, verdict) = timed_loop(&mut evm, &corpus.frame, iters)?;
    let (upgrade_wait_ms, post_p50_us) = if mode == Mode::JitPool {
        match &backend {
            Some(b) => {
                let (wait, p50) =
                    post_upgrade_measure(b, &corpus.contracts, &mut evm, &corpus.frame)?;
                (Some(wait), Some(p50))
            }
            None => (None, None),
        }
    } else {
        (None, None)
    };
    let stats_after = backend.as_ref().map(JitBackend::stats);
    let count = |f: fn(&::evm2_jit::runtime::RuntimeStatsSnapshot) -> u64| {
        stats_after.as_ref().map_or(0, &f) - stats_before.as_ref().map_or(0, &f)
    };

    let mut steady: Vec<u128> = samples.iter().copied().skip(1).collect();
    steady.sort_unstable();
    let p = |q: usize| steady[(steady.len() * q / 100).min(steady.len() - 1)];
    Ok(ModeReport {
        mode: mode.name(),
        first_us: samples[0],
        p50_us: p(50),
        p99_us: p(99),
        post_p50_us,
        upgrade_wait_ms,
        verdict,
        lookup_hits: count(|s| s.lookup_hits),
        lookup_misses: count(|s| s.lookup_misses),
        compilations: count(|s| s.compilations_succeeded),
        aot_prep_ms,
    })
}

fn fmt_opt(v: Option<u128>) -> String {
    v.map_or_else(|| "-".to_string(), |n| n.to_string())
}

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_else(|| "all".to_string());
    let iters: usize = std::env::var("SIM_BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let modes = match filter.as_str() {
        "all" => vec![Mode::Interp, Mode::JitBlock, Mode::JitPool, Mode::Aot],
        "interp" => vec![Mode::Interp],
        "jit-block" => vec![Mode::JitBlock],
        "jit-pool" => vec![Mode::JitPool],
        "aot" => vec![Mode::Aot],
        other => {
            eprintln!("unknown mode {other:?} — want all|interp|jit-block|jit-pool|aot");
            std::process::exit(2);
        }
    };

    println!("sim_mode_bench: {iters} iterations per mode (steady-state excludes the first)");
    println!(
        "{:<10} {:>12} {:>12} {:>12} {:>12} {:>12} {:>8} {:>8} {:>8} {:>12}",
        "mode",
        "first µs",
        "p50 µs",
        "p99 µs",
        "post p50 µs",
        "upgrade ms",
        "hits*",
        "misses*",
        "compiled",
        "aot prep ms"
    );
    let mut reports = Vec::new();
    for mode in modes {
        match run_mode(mode, iters) {
            Ok(report) => {
                println!(
                    "{:<10} {:>12} {:>12} {:>12} {:>12} {:>12} {:>8} {:>8} {:>8} {:>12}",
                    report.mode,
                    report.first_us,
                    report.p50_us,
                    report.p99_us,
                    fmt_opt(report.post_p50_us),
                    fmt_opt(report.upgrade_wait_ms),
                    report.lookup_hits,
                    report.lookup_misses,
                    report.compilations,
                    fmt_opt(report.aot_prep_ms),
                );
                reports.push(report);
            }
            Err(e) => {
                println!("{:<10} FAILED: {e}", mode.name());
                std::process::exit(1);
            }
        }
    }
    if let Some((first, rest)) = reports.split_first() {
        let base = first.verdict;
        for report in rest {
            if report.verdict != base {
                eprintln!(
                    "VERDICT DIVERGENCE: {} {base:?} vs {} {:?}",
                    first.mode, report.mode, report.verdict
                );
                std::process::exit(1);
            }
        }
        println!("verdict identity across modes: OK ({base:?})");
        println!(
            "* hits/misses count the blocking lookup path only; async modes record events, not counters."
        );
    }
}
