//! The shared executor-corpus fixture for the phase-6 engine examples: the
//! real `cmd_executor` bytecode plus `Token`/`Pair`/`PoolManager` stubs,
//! deployed and seeded through the production-policy evm2 EVM. Included by
//! the examples via `#[path]` (examples cannot share a library target).

use std::path::PathBuf;

use alloy::consensus::transaction::Recovered;
use alloy::consensus::TxEip1559;
use alloy::primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
use alloy::sol_types::{SolCall, SolValue};

use ::evm2::env::BlockEnvExt;
use ::evm2::ethereum::{ethereum_tx_registry, RecoveredTxEnvelope, TxEnvelope};
use ::evm2::evm::InMemoryDB;
use ::evm2::{BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, Version};

use degenbot_executor::composers::{EncodeOptions, EncodeRequest, HopInfo, PathInfo, V2HopInfo};
use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;
use degenbot_simulation::sim::evm2::inspectors::Evm2SimInspector;

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
pub const DEPLOYER: Address = Address::new([
    0xde, 0xb0, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
]);

const SETUP_GAS: u64 = 30_000_000;

/// The replayed frame's gas limit — a production pending-tx shape. EIP-7825
/// caps a transaction's gas limit at 2^24 under Osaka (the production spec)
/// and the frame-replay EVM enforces the cap; only the bench EVM lifts it,
/// for the oversized setup deploys above.
const EXECUTE_GAS: u64 = 16_000_000;

/// The corpus block's base fee — also the replayed frame's
/// `max_fee_per_gas`, so the frame-replay path's projected-base-fee check
/// settles without the disabled-check fallback.
pub const BENCH_BASE_FEE: u128 = 1_000_000_000;

/// The corpus block number; the frame-replay block pin is its successor.
pub const BENCH_BLOCK: u64 = 100;

/// A deployed contract's live code identity — the `(code_hash, code)` pair
/// the JIT lookup keys on, captured from the creation's output.
pub struct Deployed {
    pub address: Address,
    pub code_hash: B256,
    pub code: Bytes,
}

/// The deployed fixture contracts.
pub struct Deployments {
    weth: Deployed,
    pool_manager: Deployed,
    executor: Deployed,
    token: Deployed,
    pair1: Deployed,
    pair2: Deployed,
}

impl Deployments {
    /// Every contract whose code the JIT/AOT modes compile.
    pub fn identities(&self) -> Vec<Deployed> {
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
pub struct Reserves {
    pool1: (u128, u128),
    pool2: (u128, u128),
}

/// The built corpus: the deployed contracts and the one `execute()` frame
/// the timed loop replays.
pub struct Corpus {
    pub contracts: Vec<Deployed>,
    pub frame: RecoveredTxEnvelope,
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

pub fn envelope(
    from: Address,
    to: TxKind,
    data: Bytes,
    gas: u64,
    nonce: u64,
) -> RecoveredTxEnvelope {
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
pub fn deploy(
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
) -> Result<(RecoveredTxEnvelope, PathInfo), String> {
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
            path.clone(),
            optimal_input,
            vec![hop1_out, hop2_out],
            vec![optimal_input, hop1_out],
            EncodeOptions::default(),
        ),
    )
    .ok_or_else(|| "encode_cmd_stream returned None".to_string())?;
    let data =
        degenbot_executor::composers::encode_execute_call(Address::ZERO, &cmd, U256::ZERO).data;
    Ok((
        envelope(
            DEPLOYER,
            TxKind::Call(d.executor.address),
            Bytes::from(data),
            EXECUTE_GAS,
            nonce,
        ),
        path,
    ))
}

/// Build the corpus: deploy, seed, encode — all committed, all untimed.
///
/// Returns the corpus AND the frame's [`PathInfo`] — the hops the frame
/// executes, which is also the input a path-derived slot prefetch consumes
/// ([`degenbot_simulation::sim::prefetch`]).
pub fn build_corpus(evm: &mut Evm<'_, BaseEvmTypes>) -> Result<(Corpus, PathInfo), String> {
    let mut nonce = 0u64;
    let deployments = deploy_contracts(evm, &mut nonce)?;
    let reserves = seed_and_fund(evm, &deployments, &mut nonce)?;
    let (frame, path) = encode_workload(&deployments, &reserves, nonce)?;
    Ok((
        Corpus {
            contracts: deployments.identities(),
            frame,
        },
        path,
    ))
}

/// A fresh production-policy EVM over a funded deployer account. Mirrors
/// the handle's version policy with the fixture deltas: the EIP-7825
/// per-tx cap lifted (the executor creation artifact + init exceeds the
/// 2^24 default — the evm2 grammar harness lifts it too), the nonce/balance
/// checks and EIP-3607 off (the corpus's approval frames originate from
/// the executor contract itself, which holds no native funds).
///
/// `inspect` installs the composite capture inspector. Note evm2's frame
/// dispatch is inspector XOR interpreter-runner: an EVM built with
/// `inspect: true` can never take the JIT path.
pub fn bench_evm(inspect: bool) -> Evm<'static, BaseEvmTypes> {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &DEPLOYER,
        ::evm2::evm::AccountInfo::default().with_balance(U256::from(u128::MAX)),
    );
    let block_env = BlockEnvExt {
        basefee: U256::from(BENCH_BASE_FEE),
        number: U256::from(BENCH_BLOCK),
        ..Default::default()
    };
    let mut version = Version::new(EVM2_PRODUCTION_SPEC);
    version.features.remove(EvmFeatures::NONCE_CHECK);
    version.features.remove(EvmFeatures::BLOCK_GAS_LIMIT_CHECK);
    version.features.remove(EvmFeatures::EIP3607);
    version.features.remove(EvmFeatures::BALANCE_CHECK);
    version.tx_gas_limit_cap = u64::MAX;
    let mut evm = Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(EVM2_PRODUCTION_SPEC, version),
        EVM2_PRODUCTION_SPEC,
        block_env,
        ethereum_tx_registry(EVM2_PRODUCTION_SPEC),
        db,
        Precompiles::base(EVM2_PRODUCTION_SPEC),
    );
    if inspect {
        evm.set_inspector(Evm2SimInspector::default());
    }
    evm
}
