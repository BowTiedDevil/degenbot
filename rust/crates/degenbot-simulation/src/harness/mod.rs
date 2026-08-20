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
#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]

use alloy::primitives::{keccak256, Address, Bytes, U256, U512};
use revm::context::TxEnv;
use revm::context_interface::result::Output;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm};
use std::path::PathBuf;

use crate::oracle::{
    call_bytes, decode_error_string, deploy, native_balance_of, new_fixture_evm,
    set_code_size_limits, set_disable_nonce_check, set_native_balance, set_tx_gas_limit_cap,
    transact, FixtureEvm, TxSpec, Verdict,
};

pub mod declarative;
pub use declarative::{assert_erc6909_capture, assert_profitable, ChainResult, Hop, HopPool};

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

/// A single synthesized V3 pool: one `PoolV3` contract + its two `Token`s
/// (order fixed by the harness, matching `PoolV3.initialize`, which does not
/// sort).
#[derive(Debug, Clone, Copy)]
pub struct V3Pool {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    /// Fee in hundredths of a bip (e.g. 3000 = 0.3%).
    pub fee: u32,
    /// Q64.96 sqrt price.
    pub sqrt_price: U256,
    /// Active liquidity.
    pub liquidity: u128,
}

/// A single synthesized V4 pool inside the shared `PoolManager` stub: a
/// `(currency0, currency1, fee, tick_spacing)` pool key + its seeded price/
/// liquidity.
#[derive(Debug, Clone, Copy)]
pub struct V4Pool {
    pub currency0: Address,
    pub currency1: Address,
    /// Fee in hundredths of a bip (uint24).
    pub fee: u32,
    pub tick_spacing: i32,
    pub sqrt_price: U256,
    pub liquidity: u128,
}

/// A running harness: the real executor + a set of synthesized tokens/pools in
/// one fresh revm `CacheDB<EmptyDB>`.
pub struct Harness {
    pub evm: FixtureEvm,
    pub executor: Address,
    /// The token address the executor treats as WETH (its immutable `WETH_ADDR`).
    pub weth: Address,
    /// The deployed PoolManager stub (the executor's immutable `POOL_MANAGER_ADDR`).
    pub pool_manager: Address,
    /// All deployed V2 pools (in `add_pool` call order).
    pub pools: Vec<V2Pool>,
    /// All deployed V3 pools (in `add_v3_pool` call order).
    pub v3_pools: Vec<V3Pool>,
    /// All registered V4 pools (in `add_v4_pool` call order).
    pub v4_pools: Vec<V4Pool>,
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

        // Deploy the PoolManager stub before the executor — its address is the
        // executor's immutable POOL_MANAGER_ADDR (V4 paths route swaps/deltas
        // through it).
        let pm = deploy(
            &mut evm,
            Bytes::from(load_stub_creation("PoolManager")),
            8_000_000,
        )?;
        let mut init = load_hex("tier3-oracle/artifacts/executor/cmd_executor.creation.hex");
        init.extend_from_slice(&executor_deploy_args(weth, pm));
        let executor = deploy(&mut evm, Bytes::from(init), 30_000_000)?;

        Ok(Self {
            evm,
            executor,
            weth,
            pool_manager: pm,
            pools: Vec::new(),
            v3_pools: Vec::new(),
            v4_pools: Vec::new(),
            tokens: vec![weth],
        })
    }

    /// Deploy any committed stub artifact (`Token`/`Pair`/`PoolV3`/… by name)
    /// at a fresh CREATE address and return it.
    pub fn deploy_stub(&mut self, name: &str) -> Result<Address, String> {
        deploy(
            &mut self.evm,
            Bytes::from(load_stub_creation(name)),
            8_000_000,
        )
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

    /// Deploy a V3 pool over `token_a`/`token_b` at `fee` (hundredths of a
    /// bip), set its Q64.96 price + liquidity, and mint `amt_a`/`amt_b` of
    /// each token to the pool (so it can send swap output). Token order is
    /// fixed by the caller (matching `PoolV3.initialize`, which does not
    /// sort) — `a` is `token0`, `b` is `token1`.
    pub fn add_v3_pool(
        &mut self,
        token_a: Address,
        token_b: Address,
        fee: u32,
        sqrt_price: U256,
        liquidity: u128,
        amt_a: u128,
        amt_b: u128,
    ) -> Result<V3Pool, String> {
        let pool = deploy(
            &mut self.evm,
            Bytes::from(load_stub_creation("PoolV3")),
            8_000_000,
        )?;
        let _ = self.call(pool, &init_v3(token_a, token_b, fee), 500_000)?;
        let _ = self.call(pool, &set_v3_price(sqrt_price), 200_000)?;
        let _ = self.call(pool, &set_v3_liquidity(liquidity), 200_000)?;
        self.call(token_a, &mint_to(pool, amt_a), 200_000)?;
        self.call(token_b, &mint_to(pool, amt_b), 200_000)?;
        for t in [token_a, token_b] {
            if !self.tokens.contains(&t) {
                self.tokens.push(t);
            }
        }
        let v3 = V3Pool {
            pool,
            token0: token_a,
            token1: token_b,
            fee,
            sqrt_price,
            liquidity,
        };
        self.v3_pools.push(v3);
        Ok(v3)
    }

    /// Register a V4 pool in the shared `PoolManager` stub: `initialize` the
    /// `(c0,c1,fee,ts)` pool at `sqrt_price`/`liquidity`, then `_fund` the PM
    /// with `fund_c0`/`fund_c1` of each currency (the PM's holdings back the
    /// executor's `take` of positive deltas).
    pub fn add_v4_pool(
        &mut self,
        c0: Address,
        c1: Address,
        fee: u32,
        tick_spacing: i32,
        sqrt_price: U256,
        liquidity: u128,
        fund_c0: u128,
        fund_c1: u128,
    ) -> Result<V4Pool, String> {
        let _ = self.call(
            self.pool_manager,
            &init_v4(c0, c1, fee, tick_spacing, sqrt_price, liquidity),
            500_000,
        )?;
        for (c, amt) in [(c0, fund_c0), (c1, fund_c1)] {
            let _ = self.call(self.pool_manager, &fund_v4(c, amt), 200_000)?;
            // Native currency has no ERC-20 to mint, so seed the PM's actual
            // native balance too (it's what `take` of a native delta sends).
            if c == Address::ZERO {
                let held = native_balance_of(&mut self.evm, self.pool_manager);
                set_native_balance(&mut self.evm, self.pool_manager, held + U256::from(amt));
            }
        }
        for t in [c0, c1] {
            if !self.tokens.contains(&t) {
                self.tokens.push(t);
            }
        }
        let v4 = V4Pool {
            currency0: c0,
            currency1: c1,
            fee,
            tick_spacing,
            sqrt_price,
            liquidity,
        };
        self.v4_pools.push(v4);
        Ok(v4)
    }

    /// Give `who` `amount` of `token` (mint — the harness's free liquidity).
    /// For the native currency (`Address::ZERO`), sets the recipient's revm
    /// native balance instead (there is no ERC-20 to mint). Minting WETH also
    /// credits the WETH contract the **matching native backing** — real WETH9
    /// is always deposit-backed, and `WETH_WITHDRAW` needs the contract to hold
    /// native to pay out (the executor can't withdraw unbacked minted WETH).
    pub fn fund(&mut self, token: Address, who: Address, amount: u128) -> Result<(), String> {
        if token == Address::ZERO {
            let held = native_balance_of(&mut self.evm, who);
            set_native_balance(&mut self.evm, who, held + U256::from(amount));
            return Ok(());
        }
        self.call(token, &mint_to(who, amount), 200_000)
            .map(|_| ())?;
        if token == self.weth {
            // Back the mint: give the WETH contract native so `withdraw` can pay.
            let backing = native_balance_of(&mut self.evm, token);
            set_native_balance(&mut self.evm, token, backing + U256::from(amount));
        }
        Ok(())
    }

    /// Set an account's native (ETH) balance directly (preserving its code).
    pub fn set_native_balance(&mut self, who: Address, amount: U256) {
        set_native_balance(&mut self.evm, who, amount);
    }

    /// Read an account's native (ETH) balance.
    pub fn native_balance_of(&mut self, who: Address) -> Result<U256, String> {
        Ok(native_balance_of(&mut self.evm, who))
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

    /// Execute an encoded payload with `config=0` (skip profit check, no
    /// bribe) and classify the outcome.
    pub fn execute_payload(&mut self, payload: &[u8], gas: u64) -> Result<ExecOutcome, String> {
        self.execute_payload_config(payload, gas, U256::ZERO)
    }

    /// Execute an encoded payload with an explicit `execute()` `config` uint256
    /// (packed `check_mode`/bribe/expected_value — see [`execute_data_config`])
    /// and classify the outcome. Enables runtime proof of the `erc6909_profit`
    /// (`check_mode=2`) and bribe config axes (EYUWFG / WE45KC).
    pub fn execute_payload_config(
        &mut self,
        payload: &[u8],
        gas: u64,
        config: U256,
    ) -> Result<ExecOutcome, String> {
        let data = execute_data_config(payload, config);
        match transact(
            &mut self.evm,
            TxSpec::Call {
                to: self.executor,
                data,
                gas,
            },
        ) {
            Verdict::Accepted { logs, .. } => {
                let swaps =
                    count_swap_events(&logs, &self.pools, &self.v3_pools, self.pool_manager);
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

    /// Read `PM.balanceOf(account, uint160(currency))` — the executor's
    /// ERC6909 WETH balance held inside the PoolManager (the `erc6909_profit`
    /// capture destination and the value `check_mode=2` verifies).
    pub fn pm_balance_of(&mut self, account: Address, currency: Address) -> Result<U256, String> {
        let out = self.call(
            self.pool_manager,
            &pm_balance_of_data(account, currency),
            200_000,
        )?;
        if out.len() < 32 {
            return Err(format!("PM.balanceOf returned {} bytes", out.len()));
        }
        Ok(U256::from_be_bytes::<32>(
            out.as_ref()[..32].try_into().unwrap(),
        ))
    }

    /// Encode a fully-V2 path via the production entry (`encode_cmd_stream`)
    /// and execute it. Each hop is `(pool index into [`Self::pools`],
    /// `zero_for_one`); `hop_outputs[i]` are the per-hop solver outputs. This
    /// routes all-V2 through the Plan + validator path
    /// (`grammar_shape::derive_all_v2` → `build_walk`) exactly like
    /// production. Returns the classified outcome.
    pub fn run_v2_path(
        &mut self,
        pool_indices: &[usize],
        zfo: &[bool],
        optimal_input: u128,
        hop_outputs: &[u128],
        gas: u64,
    ) -> Result<ExecOutcome, String> {
        use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo};
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
        self.run_path(&path, optimal_input, hop_outputs, gas)
    }

    /// Encode an arbitrary (mixed-V2/V3) `&PathInfo` via the production entry
    /// and execute it. `hop_outputs[i]` are the per-hop solver outputs;
    /// `consumed_inputs` defaults to `[optimal_input, hop_outputs[0], …]`.
    pub fn run_path(
        &mut self,
        path: &degenbot_executor::composers::PathInfo,
        optimal_input: u128,
        hop_outputs: &[u128],
        gas: u64,
    ) -> Result<ExecOutcome, String> {
        self.run_path_with_opts(
            path,
            optimal_input,
            hop_outputs,
            gas,
            degenbot_executor::composers::EncodeOptions::default(),
        )
    }

    /// KO5NNB variant of [`Self::run_path`] with explicit [`EncodeOptions`]
    /// (funding axis etc.), threaded through to `encode_path_with_opts` — used
    /// by [`crate::harness::declarative::Harness::run_chain_with_opts`].
    ///
    /// Executes under the **production axis-aware config** (SMOZG3 — the same
    /// `config_for_options(opts, 0)` the arbitrage strategy packs, Q35IJN):
    /// the on-chain profit check runs exactly like production (default
    /// Custody → `check_mode=1` active assert; `erc6909_profit` → `check_mode=2`).
    pub fn run_path_with_opts(
        &mut self,
        path: &degenbot_executor::composers::PathInfo,
        optimal_input: u128,
        hop_outputs: &[u128],
        gas: u64,
        opts: degenbot_executor::composers::EncodeOptions,
    ) -> Result<ExecOutcome, String> {
        let cmd = self.encode_path_with_opts(path, optimal_input, hop_outputs, opts)?;
        self.execute_payload_config(&cmd, gas, production_config(opts)?)
    }

    /// ADR-033 (D7) variant of [`Self::run_path_with_opts`] with CALLER-supplied
    /// `consumed_inputs` — the per-hop amounts the production solver commits
    /// after `clamp_cl_hop_capacity` re-aligns them — instead of this harness
    /// synthesizing the full-consumption chain (`[optimal_input,
    /// hop_outputs[0], …]`).
    pub fn run_path_with_consumed(
        &mut self,
        path: &degenbot_executor::composers::PathInfo,
        optimal_input: u128,
        hop_outputs: &[u128],
        consumed_inputs: &[u128],
        gas: u64,
        opts: degenbot_executor::composers::EncodeOptions,
    ) -> Result<ExecOutcome, String> {
        let cmd = self.encode_path_with_consumed(
            path,
            optimal_input,
            hop_outputs,
            consumed_inputs,
            opts,
        )?;
        self.execute_payload_config(&cmd, gas, production_config(opts)?)
    }

    /// ADR-033 (D7): encode with CALLER-supplied per-hop `consumed_inputs`
    /// — the production solver's committed amounts (the shape
    /// `clamp_cl_hop_capacity` re-aligns) — instead of synthesizing the
    /// full-consumption chain. The amounts still flow through the production
    /// `encode_cmd_stream` intake (`EncodeContext` + `EncodeRequest`).
    pub fn encode_path_with_consumed(
        &self,
        path: &degenbot_executor::composers::PathInfo,
        optimal_input: u128,
        hop_outputs: &[u128],
        consumed_inputs: &[u128],
        opts: degenbot_executor::composers::EncodeOptions,
    ) -> Result<Vec<u8>, String> {
        if hop_outputs.len() != path.hops.len() || consumed_inputs.len() != path.hops.len() {
            return Err(format!(
                "encode_path_with_consumed: per-hop arrays must have one entry per hop ({} hops, {} outputs, {} consumed)",
                path.hops.len(),
                hop_outputs.len(),
                consumed_inputs.len()
            ));
        }
        degenbot_executor::composers::encode_cmd_stream(
            &self.encode_context(),
            &degenbot_executor::composers::EncodeRequest::new(
                path.clone(),
                optimal_input,
                hop_outputs.to_vec(),
                consumed_inputs.to_vec(),
                opts,
            ),
        )
        .ok_or_else(|| "encode_cmd_stream returned None".to_string())
    }

    /// Encode a PathInfo through the production `encode_cmd_stream` (the raw
    /// payload `execute_payload`/`execute_data` then drive).
    pub fn encode_path(
        &self,
        path: &degenbot_executor::composers::PathInfo,
        optimal_input: u128,
        hop_outputs: &[u128],
    ) -> Result<Vec<u8>, String> {
        self.encode_path_with_opts(
            path,
            optimal_input,
            hop_outputs,
            degenbot_executor::composers::EncodeOptions::default(),
        )
    }

    /// ADR-033 encode intake context: the session-scoped deployment addresses
    /// (executor / PoolManager / WETH), derived once from what the harness
    /// deployed and shared by every `encode_path*` call (mirrors the
    /// arbitrage `SimulateContext::encode_context` projection).
    #[must_use]
    pub fn encode_context(&self) -> degenbot_executor::composers::EncodeContext {
        degenbot_executor::composers::EncodeContext::new(
            self.executor,
            self.pool_manager,
            self.weth,
        )
    }
    /// Encode a path with explicit [`EncodeOptions`] (WE45KC runtime axis proof
    /// — funding source / profit capture / bribe).
    pub fn encode_path_with_opts(
        &self,
        path: &degenbot_executor::composers::PathInfo,
        optimal_input: u128,
        hop_outputs: &[u128],
        opts: degenbot_executor::composers::EncodeOptions,
    ) -> Result<Vec<u8>, String> {
        let n = path.hops.len();
        let consumed: Vec<u128> = std::iter::once(optimal_input)
            .chain(hop_outputs.iter().copied())
            .take(n)
            .collect();
        degenbot_executor::composers::encode_cmd_stream(
            &self.encode_context(),
            &degenbot_executor::composers::EncodeRequest::new(
                path.clone(),
                optimal_input,
                hop_outputs.to_vec(),
                consumed,
                opts,
            ),
        )
        .ok_or_else(|| "encode_cmd_stream returned None".to_string())
    }
}

/// The production axis-aware `execute()` config — the single point where the
/// harness meets the strategy's Q35IJN config expression
/// (Custody → `check_mode=1`, Erc6909 → `check_mode=2`, SweepToAddress →
/// `check_mode=3`).
fn production_config(opts: degenbot_executor::composers::EncodeOptions) -> Result<U256, String> {
    degenbot_executor::composers::config_for_options(opts, U256::ZERO)
        .map_err(|e| format!("config_for_options: {e}"))
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
/// `PM.balanceOf(address,uint256)` ABI: selector + (account, currencyId).
fn pm_balance_of_data(account: Address, currency: Address) -> Vec<u8> {
    let h = keccak256(b"balanceOf(address,uint256)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&pad32(account));
    // id = uint256(uint160(currency)) → the address occupying the low 20 bytes.
    out.extend_from_slice(&pad32(currency));
    out
}
/// The `execute(bytes,uint256)` call: selector + (bytes, config=0) ABI encoding.
#[must_use]
pub fn execute_data(payload: &[u8]) -> Bytes {
    execute_data_config(payload, U256::ZERO)
}
/// The `execute(bytes,uint256)` call: selector + (bytes, config) ABI encoding,
/// where `config` is the packed execute config (check_mode/bribe/expected_value).
///
/// Delegates to the production [`degenbot_executor::composers::encode_execute_call`]
/// (the §YQORTM leaf, uses the proper `encode_rust` ABI encoder) so the config
/// lands in head\[1\] — NOT hand-rolled. The prior hand-rolled encoding wrote
/// `config` at the END of the calldata (after the bytes tail), so the contract
/// read `config = payload.len()` (a silent no-op config); the bug was latent
/// because the erc6909 capture mint is in the command stream (not config-gated)
/// and `check_mode=2`'s verification is skipped when `expected_value=0`. The
/// first test requiring the config to reach the contract (WE45KC bribe) exposed
/// it.
#[must_use]
pub fn execute_data_config(payload: &[u8], config: U256) -> Bytes {
    // encode_execute_call returns EncodedCall { to, data, value }; the calldata
    // is `data` (selector + ABI-encoded (bytes, uint256)). `to`/`value` are the
    // caller's concern (execute_payload_config transacts to self.executor).
    let executor = Address::ZERO; // unused (data only); the caller sets the target.
    match degenbot_executor::composers::encode_execute_call(executor, payload, config) {
        Ok(call) => Bytes::from(call.data),
        Err(e) => {
            // Should not happen with valid inputs; fall back to an empty payload
            // so the transact surfaces a clear contract revert rather than a panic.
            let _ = e;
            Bytes::new()
        }
    }
}

fn pad32(a: Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(a.as_slice());
    w
}

fn init_v3(a: Address, b: Address, fee: u32) -> Vec<u8> {
    let h = keccak256(b"initialize(address,address,uint24)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&pad32(a));
    out.extend_from_slice(&pad32(b));
    out.extend_from_slice(&U256::from(fee).to_be_bytes::<32>());
    out
}
fn set_v3_price(p: U256) -> Vec<u8> {
    let h = keccak256(b"setPrice(uint160)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&p.to_be_bytes::<32>());
    out
}
fn set_v3_liquidity(l: u128) -> Vec<u8> {
    let h = keccak256(b"setLiquidity(uint128)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&U256::from(l).to_be_bytes::<32>());
    out
}

fn init_v4(c0: Address, c1: Address, fee: u32, ts: i32, sqrt: U256, liq: u128) -> Vec<u8> {
    let h = keccak256(b"initialize(address,address,uint24,int24,uint160,uint128)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&pad32(c0));
    out.extend_from_slice(&pad32(c1));
    out.extend_from_slice(&U256::from(fee).to_be_bytes::<32>());
    out.extend_from_slice(&U256::from(ts as u32).to_be_bytes::<32>());
    out.extend_from_slice(&sqrt.to_be_bytes::<32>());
    out.extend_from_slice(&U256::from(liq).to_be_bytes::<32>());
    out
}
fn fund_v4(currency: Address, amt: u128) -> Vec<u8> {
    let h = keccak256(b"_fund(address,uint256)");
    let mut out = h.0[..4].to_vec();
    out.extend_from_slice(&pad32(currency));
    out.extend_from_slice(&U256::from(amt).to_be_bytes::<32>());
    out
}

// ── V3 amount math (via the engine's proven `degenbot-concentrated-liquidity-math`) ──

/// Compute the exact-input V3 output for `amount_in` at `sqrt_price`/`liquidity`
/// with `fee` (hundredths of a bip), mirroring `PoolV3.swap` (single active
/// tick range, target = the unbounded limit, full input consumed). Returns the
/// output amount (token1 for `zero_for_one`, token0 otherwise).
#[must_use]
pub fn v3_amount_out(
    sqrt_price: U256,
    liquidity: u128,
    amount_in: u128,
    zero_for_one: bool,
    fee: u32,
) -> u128 {
    use degenbot_math::cl::sqrt_price_math::{
        get_amount0_delta, get_amount1_delta, get_next_sqrt_price_from_input,
    };
    let fee_retained = U256::from(1_000_000u64 - u64::from(fee));
    let amount_less_fee = full_mul_div(
        U256::from(amount_in),
        fee_retained,
        U256::from(1_000_000u64),
    );
    let next = get_next_sqrt_price_from_input(
        sqrt_price,
        liquidity as i128,
        amount_less_fee,
        zero_for_one,
    )
    .expect("valid v3 next price");
    let out = if zero_for_one {
        get_amount1_delta(next, sqrt_price, liquidity as i128, Some(false))
    } else {
        get_amount0_delta(next, sqrt_price, liquidity as i128, Some(false))
    }
    .expect("valid v3 amount delta");
    out.to::<u128>()
}

/// `min(a*b/denom, …)` rounded down — a tiny 512-bit mulDiv. Called by
/// `v3_amount_out` (and could live in `degenbot-concentrated-liquidity-math`; kept local to avoid widening).
fn full_mul_div(a: U256, b: U256, denom: U256) -> U256 {
    let prod = U512::from(a) * U512::from(b);
    let q = prod / U512::from(denom);
    q.to::<U256>()
}

fn count_swap_events(
    logs: &[revm::primitives::Log],
    pools: &[V2Pool],
    v3_pools: &[V3Pool],
    pool_manager: Address,
) -> usize {
    let v2_topic = keccak256(b"Swap(address,uint256,uint256,uint256,uint256,address)");
    let v3_topic = keccak256(b"SwapV3(address,uint256,uint160,int256)");
    let v4_topic = keccak256(b"V4Swap(bytes32,address,address,uint256,uint256)");
    logs.iter()
        .filter(|l| {
            if l.topics().first() == Some(&v2_topic) {
                pools.iter().any(|p| l.address == p.pair)
            } else if l.topics().first() == Some(&v3_topic) {
                v3_pools.iter().any(|p| l.address == p.pool)
            } else if l.topics().first() == Some(&v4_topic) {
                l.address == pool_manager
            } else {
                false
            }
        })
        .count()
}
