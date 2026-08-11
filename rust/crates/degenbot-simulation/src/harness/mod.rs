//! Executor grammar harness (UQOAHA).
//!
//! The missing third correctness tool for the swap-encoding grammar: byte
//! parity (the golden corpus) only pins bytes and proves nothing at runtime,
//! while a live sim needs a captured mainnet path per family. This harness
//! deploys the **real** `cmd_executor` bytecode + synthesized pools into a
//! fresh revm `CacheDB<EmptyDB>` and runs a path's
//! [`encode_cmd_stream`](degenbot_executor::composers::encode_cmd_stream)
//! payload through `execute()`, reporting whether it executes, which pools it
//! touched (via `Swap` events), and how it failed — with the production
//! `FailBucket` vocabulary. A composer change therefore regresses only its
//! permutation, with a visible result instead of a silent byte-only assumption.
//!
//! Design (ADR-020 "extend the tier-3 oracles"): reuse the real `cmd_executor`
//! artifact (deploy-proven by [`crate::oracle`]'s fixture driver) + the SWAP
//! math already proven by the tier-3 V2 oracles. The only synthesized pieces
//! are a minimal full-ERC20 `Token` and a Uniswap-V2-faithful `Pair` (see
//! `tier3-oracle/src-harness/V2ExecutorStub.sol`) — enough custody/ordering to
//! exercise the executor's command stream + flash/repay, the exact funding-
//! topology risk the funding-topology conversions are blocked on.
//!
//! This is a fixture/investigation harness (the revm-deploy + seed + execute
//! spine), so it carries the same permits as the `oracle` fixture driver: it
//! may panic/expect on a bad artifact or a broken fixture step (a harness
//! problem, not a verdict) and its doc is `# Errors`/`# Panics`-annotated.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown
)]

use alloy::primitives::{keccak256, Address, Bytes, U256};
use revm::context::TxEnv;
use revm::context_interface::result::Output;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm};
use std::path::PathBuf;

use crate::oracle::{
    call_bytes, decode_error_string, deploy, new_fixture_evm, selector, set_code_size_limits,
    set_disable_nonce_check, set_tx_gas_limit_cap, transact, FixtureEvm, TxSpec, Verdict,
};

/// Repo root = this crate + three up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("canonicalize repo root")
}

/// Read a whole hex file (`0x`-less or prefixed, whitespace tolerated).
fn load_hex(rel: &str) -> Vec<u8> {
    let raw = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let s: String = raw.trim().chars().filter(|c| !c.is_whitespace()).collect();
    let s = s.strip_prefix("0x").unwrap_or(&s);
    alloy::hex::decode(s).expect("hex decodes")
}

/// ABI-encode the two `__init__(address weth, address pool_manager)` args.
fn executor_deploy_args(weth: Address, pool_manager: Address) -> Vec<u8> {
    let mut args = vec![0u8; 64];
    args[12..32].copy_from_slice(weth.as_slice());
    args[44..64].copy_from_slice(pool_manager.as_slice());
    args
}

/// Load a `Token`/`Pair`-style foundry artifact's creation bytecode
/// (`bytecode.object`) from `tier3-oracle/artifacts/harness/<contract>.json`.
fn load_stub_creation(contract: &str) -> Vec<u8> {
    let rel = format!("tier3-oracle/artifacts/harness/{contract}.json");
    let raw = std::fs::read_to_string(repo_root().join(&rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    crate::oracle::parse_foundry_creation_bytecode(&raw)
        .unwrap_or_else(|e| panic!("parse {contract}: {e}"))
}

/// A single synthesized V2 pool: one `Pair` contract + its two `Token`s,
/// in the pair's **sorted** token order, with the seeded (sorted) reserves.
#[derive(Debug, Clone, Copy)]
pub struct V2Pool {
    pub pair: Address,
    /// The pair's `token0` (lower address).
    pub token0: Address,
    /// The pair's `token1` (higher address).
    pub token1: Address,
    /// Seeded reserve of `token0`.
    pub reserve0: u128,
    /// Seeded reserve of `token1`.
    pub reserve1: u128,
}

/// A running harness: the real executor + a set of synthesized tokens/pools in
/// one fresh revm `CacheDB<EmptyDB>`.
pub struct Harness {
    pub evm: FixtureEvm,
    pub executor: Address,
    /// The token address the executor treats as WETH (its immutable `WETH_ADDR`).
    pub weth: Address,
    /// All deployed pools (in `add_pool` call order).
    pub pools: Vec<V2Pool>,
    /// All deployed tokens (deduped across pools, `add_pool` discovery order).
    pub tokens: Vec<Address>,
}

impl Harness {
    /// Build a fresh harness: revm over `CacheDB<EmptyDB>`, deploy the real
    /// executor (creation bytes + weth/pm constructor args) + a WETH token.
    pub fn new() -> Result<Self, String> {
        let mut evm = new_fixture_evm();
        set_disable_nonce_check(&mut evm, true);
        set_code_size_limits(&mut evm, None); // executor init + runtime are large
                                              // The executor creation + a long command stream can exceed the default
                                              // EIP-7825 per-tx cap; lift it for the harness.
        set_tx_gas_limit_cap(&mut evm, u64::MAX);

        // Deploy a WETH token first; its address is the executor's WETH_ADDR.
        let weth = deploy(
            &mut evm,
            Bytes::from(load_stub_creation("Token")),
            8_000_000,
        )?;

        let pm = Address::repeat_byte(0x22); // PoolManager — unused for all-V2
        let mut init = load_hex("tier3-oracle/artifacts/executor/cmd_executor.creation.hex");
        init.extend_from_slice(&executor_deploy_args(weth, pm));
        let executor = deploy(&mut evm, Bytes::from(init), 30_000_000)?;

        Ok(Self {
            evm,
            executor,
            weth,
            pools: Vec::new(),
            tokens: vec![weth],
        })
    }

    /// Deploy a fresh `Token` (a ^0.8 minimal full-ERC20) and return its address.
    pub fn add_token(&mut self) -> Result<Address, String> {
        let t = deploy(
            &mut self.evm,
            Bytes::from(load_stub_creation("Token")),
            8_000_000,
        )?;
        self.tokens.push(t);
        Ok(t)
    }

    /// Deploy a V2 pair over `token_a`/`token_b`, seed reserves (mint to the
    /// pair + `sync`), return the pool. The pair's interior `token0`/`token1`
    /// are **sorted by address** (matching `Pair.initialize`), so the returned
    /// `V2Pool` is in that sorted order; `reserve_a`/`reserve_b` must be passed
    /// in the same order as `token_a`/`token_b`.
    pub fn add_pool(
        &mut self,
        token_a: Address,
        token_b: Address,
        reserve_a: u128,
        reserve_b: u128,
    ) -> Result<V2Pool, String> {
        let pair = deploy(
            &mut self.evm,
            Bytes::from(load_stub_creation("Pair")),
            8_000_000,
        )?;
        // initialize(tokenA, tokenB) — sorts internally.
        let _ = self.call(pair, &init_pair(token_a, token_b), 500_000)?;

        // Map reserves to the pair's sorted token order.
        let (t0, r0) = if token_a < token_b {
            (token_a, reserve_a)
        } else {
            (token_b, reserve_b)
        };
        let (t1, r1) = if token_a < token_b {
            (token_b, reserve_b)
        } else {
            (token_a, reserve_a)
        };

        // Seed reserves: mint `r0`/`r1` of each token to the pair, then `sync()`
        // so slot reserves equal the live balances (ADR-020 D4).
        self.call(t0, &mint_to(pair, r0), 200_000)?;
        self.call(t1, &mint_to(pair, r1), 200_000)?;
        let _ = self.call(pair, &sync_selector(), 200_000)?;

        for t in [t0, t1] {
            if !self.tokens.contains(&t) {
                self.tokens.push(t);
            }
        }
        let pool = V2Pool {
            pair,
            token0: t0,
            token1: t1,
            reserve0: r0,
            reserve1: r1,
        };
        self.pools.push(pool);
        Ok(pool)
    }

    /// Give `who` `amount` of `token` (mint — the harness's free liquidity).
    pub fn fund(&mut self, token: Address, who: Address, amount: u128) -> Result<(), String> {
        self.call(token, &mint_to(who, amount), 200_000).map(|_| ())
    }

    /// Have the executor approve `pool` for both of its tokens to `max`
    /// (so the pair's `swap` `transferFrom(executor, …)` can pull).
    pub fn executor_approve_pair(&mut self, pool: V2Pool) -> Result<(), String> {
        for t in [pool.token0, pool.token1] {
            let data = approve_data(pool.pair, U256::MAX);
            self.call_as_executor(t, &data, 200_000)?;
        }
        Ok(())
    }

    /// A plain state-mutating call from the default caller (deployer).
    pub fn call(&mut self, to: Address, data: &[u8], gas: u64) -> Result<Bytes, String> {
        call_bytes(&mut self.evm, to, Bytes::copy_from_slice(data), gas)
    }

    /// Send a raw call from the executor address (e.g. `approve` on a token so
    /// the pairs can `transferFrom` the executor).
    pub fn call_as_executor(
        &mut self,
        to: Address,
        data: &[u8],
        gas: u64,
    ) -> Result<Bytes, String> {
        let tx = TxEnv::builder()
            .kind(TxKind::Call(to))
            .gas_limit(gas)
            .data(Bytes::copy_from_slice(data))
            .build()
            .expect("valid call tx env");
        match self.evm.transact(tx) {
            Ok(res) => {
                let out = match res.result {
                    revm::context_interface::result::ExecutionResult::Success {
                        output: Output::Call(b),
                        ..
                    } => b,
                    revm::context_interface::result::ExecutionResult::Success {
                        output: Output::Create(..),
                        ..
                    } => return Err("call returned Create".into()),
                    revm::context_interface::result::ExecutionResult::Revert { output, .. } => {
                        self.evm.commit(res.state);
                        return Err(format!("call_as_executor reverted: {output:?}"));
                    }
                    revm::context_interface::result::ExecutionResult::Halt { reason, .. } => {
                        self.evm.commit(res.state);
                        return Err(format!("call_as_executor halted: {reason:?}"));
                    }
                };
                self.evm.commit(res.state);
                Ok(out)
            }
            Err(e) => Err(format!("call_as_executor transact err: {e:?}")),
        }
    }

    /// Execute an encoded payload and classify the outcome.
    pub fn execute_payload(&mut self, payload: &[u8], gas: u64) -> Result<ExecOutcome, String> {
        let data = execute_data(payload);
        match transact(
            &mut self.evm,
            TxSpec::Call {
                to: self.executor,
                data,
                gas,
            },
        ) {
            Verdict::Accepted { logs, .. } => {
                let swaps = count_swap_events(&logs, &self.pools);
                Ok(ExecOutcome::Accepted { swaps })
            }
            Verdict::Reverted(r) => {
                let reason = decode_error_string(&r);
                Ok(ExecOutcome::Reverted {
                    reason,
                    raw: r.to_vec(),
                })
            }
            Verdict::Halted(h) => Ok(ExecOutcome::Halted(h)),
        }
    }

    /// Read `token.balanceOf(account)`.
    pub fn balance_of(&mut self, token: Address, account: Address) -> Result<U256, String> {
        let out = self.call(token, &balance_of_data(account), 200_000)?;
        if out.len() < 32 {
            return Err(format!("balanceOf returned {} bytes", out.len()));
        }
        Ok(U256::from_be_bytes::<32>(
            out.as_ref()[..32].try_into().unwrap(),
        ))
    }

    /// Encode a fully-V2 path via the production entry (`encode_cmd_stream`)
    /// and execute it. Each hop is `(pool index into [`Self::pools`],
    /// `zero_for_one`); `hop_outputs[i]` are the per-hop solver outputs. This
    /// routes all-V2 through `encode_all_v2`/`all_v2_walk` exactly like
    /// production. Returns the classified outcome.
    pub fn run_v2_path(
        &mut self,
        pool_indices: &[usize],
        zfo: &[bool],
        optimal_input: u128,
        hop_outputs: &[u128],
        gas: u64,
    ) -> Result<ExecOutcome, String> {
        use degenbot_executor::composers::{
            encode_cmd_stream, EncodeOptions, HopInfo, PathInfo, V2HopInfo,
        };
        let n = pool_indices.len();
        debug_assert_eq!(n, zfo.len());
        debug_assert_eq!(n, hop_outputs.len());

        let mut hops = Vec::with_capacity(n);
        for (i, &pi) in pool_indices.iter().enumerate() {
            let pool = self.pools[pi];
            let hop = V2HopInfo {
                pool_address: pool.pair,
                token0_address: pool.token0,
                token1_address: pool.token1,
                fee: 30, // 0.3% — matches the stub pair's K-check
                zfo: zfo[i],
            };
            hops.push(HopInfo::V2(hop));
        }
        let path = PathInfo::new(hops);
        let consumed: Vec<u128> = std::iter::once(optimal_input)
            .chain(hop_outputs.iter().copied())
            .take(n)
            .collect();
        let cmd = encode_cmd_stream(
            &path,
            optimal_input,
            hop_outputs,
            &consumed,
            self.executor,
            self.weth,
            Address::repeat_byte(0x22),
            EncodeOptions::default(),
        )
        .ok_or_else(|| "encode_cmd_stream returned None".to_string())?;
        self.execute_payload(&cmd, gas)
    }
}

/// Classification of an executed payload.
#[derive(Debug)]
pub enum ExecOutcome {
    /// The executor's `execute()` returned success and touched `swaps` pools.
    Accepted { swaps: usize },
    /// `execute()` reverted; `reason` is the decoded Solidity error string
    /// (e.g. `Some("UniswapV2: K")` when the pair's K-check tripped).
    Reverted {
        reason: Option<String>,
        raw: Vec<u8>,
    },
    /// The EVM halted (OOG / invalid opcode) with no verdict.
    Halted(String),
}

impl ExecOutcome {
    /// Whether the payload executed (reached every hop) rather than reverting
    /// in the command stream / a pool.
    #[must_use]
    pub fn executed(&self, expected_swaps: usize) -> bool {
        matches!(self, ExecOutcome::Accepted { swaps } if *swaps == expected_swaps)
    }
}

// ── calldata/selector helpers (hand-rolled ABI, no abigen needed) ──

fn init_pair(a: Address, b: Address) -> Vec<u8> {
    let h = keccak256(b"initialize(address,address)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&pad32(a));
    out.extend_from_slice(&pad32(b));
    out
}
fn mint_to(to: Address, amount: u128) -> Vec<u8> {
    let h = keccak256(b"mint(address,uint256)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&pad32(to));
    out.extend_from_slice(&U256::from(amount).to_be_bytes::<32>());
    out
}
fn sync_selector() -> Vec<u8> {
    keccak256(b"sync()").0[..4].to_vec()
}
fn approve_data(spender: Address, amount: U256) -> Vec<u8> {
    let h = keccak256(b"approve(address,uint256)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&pad32(spender));
    out.extend_from_slice(&amount.to_be_bytes::<32>());
    out
}
fn balance_of_data(account: Address) -> Vec<u8> {
    let h = keccak256(b"balanceOf(address)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&pad32(account));
    out
}
/// The `execute(bytes,uint256)` call: selector + (bytes, 0x0) ABI encoding.
fn execute_data(payload: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(4 + 96 + payload.len().next_multiple_of(32));
    out.extend_from_slice(selector("execute(bytes,uint256)").as_slice());
    out.extend_from_slice(&U256::from(0x20u64).to_be_bytes::<32>()); // bytes offset
    out.extend_from_slice(&U256::from(payload.len()).to_be_bytes::<32>()); // length
    out.extend_from_slice(payload);
    let rem = payload.len() % 32;
    if rem != 0 {
        out.extend(std::iter::repeat_n(0u8, 32 - rem));
    }
    out.extend_from_slice(&U256::from(0u64).to_be_bytes::<32>()); // config
    Bytes::from(out)
}

fn pad32(a: Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(a.as_slice());
    w
}

fn count_swap_events(logs: &[revm::primitives::Log], pools: &[V2Pool]) -> usize {
    let topic = keccak256(b"Swap(address,uint256,uint256,uint256,uint256,address)");
    logs.iter()
        .filter(|l| l.topics().first() == Some(&topic) && pools.iter().any(|p| l.address == p.pair))
        .count()
}
