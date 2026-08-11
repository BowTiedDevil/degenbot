//! Declarative multi-hop fixture runner (UQOAHA).
//!
//! The pre-`declarative` fixtures each hand-assembled a `PathInfo`, hand-
//! derived the per-hop amount chain, hand-wired the funding buffers and pool
//! approvals, and asserted only `executed(n)` — so every new grammar family
//! cost ~50 lines of near-identical, easy-to-get-wrong plumbing and proved
//! the encoder didn't *revert*, not that its amounts were *right*.
//!
//! This module collapses that plumbing into one line. Feed
//! [`Harness::run_chain`] an ordered list of [`Hop`]s (a protocol tag + src/
//! dst token + the pool) and `optimal_input`; it:
//!
//! - forward-traverses the hops via the same amount math the harness already
//!   proves (V2 `IntHopState`, V3/V4 `v3_amount_out`) to derive every
//!   intermediate amount and the terminal output,
//! - builds the production `PathInfo` (correct `zfo` from each pool's token0),
//! - provisions the universal funding (generous WETH + per-intermediate-token
//!   buffers + `approve_pair` on every V2 pool),
//! - executes, and reports [`ChainResult`]: the classified `outcome`, the
//!   `predicted_profit` (`out_terminal − optimal_input`) and the *measured*
//!   executor WETH-balance delta.
//!
//! The measured delta is the assertion that matters: it proves the encoded
//! amounts actually moved the expected WETH, not merely that the payload
//! didn't revert. A family is then one table row (a few pool lines + one
//! `run_chain` call + one [`assert_profitable`] line) instead of a hand-
//! written test. `assert_profitable` is deliberately tolerant of ±1-wei
//! per-hop rounding (`getAmountIn` round-up, terminal V2 calc recompute) but
//! will catch a silently-wrong amount by orders of magnitude.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use alloy::primitives::{Address, U256};
use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};

use super::{v3_amount_out, ExecOutcome, Harness, V2Pool, V3Pool, V4Pool};
/// A single path hop: the protocol-tagged pool + the src (input) and dst
/// (output) tokens. `src`/`dst` determine the hop's `zero_for_one` from the
/// pool's token0, and the amount computation from the pool's reserves/price.
#[derive(Debug, Clone, Copy)]
pub struct Hop {
    /// Input token of this hop (the previous hop's output, or WETH for hop 0).
    pub src: Address,
    /// Output token of this hop (the next hop's input, or WETH for the last).
    pub dst: Address,
    /// The pool the hop routes through, protocol-tagged.
    pub pool: HopPool,
}

/// A protocol-tagged pool reference for a [`Hop`].
#[derive(Debug, Clone, Copy)]
pub enum HopPool {
    V2(V2Pool),
    V3(V3Pool),
    V4(V4Pool),
}

impl HopPool {
    /// `zero_for_one` for a hop entering at `src`: true iff `src` is the
    /// pool's token0 (sorted for V2; call-ordered for V3/V4).
    fn zfo(&self, src: Address) -> bool {
        match self {
            HopPool::V2(p) => src == p.token0,
            HopPool::V3(p) => src == p.token0,
            HopPool::V4(p) => src == p.currency0,
        }
    }

    /// The V2 constant-product exact-out for `input_in` of a token into the
    /// pool at the seeded reserves — identical to `IntHopState::swap`
    /// (0.3% fee, `in*997/r` rounding down), inlined to avoid a lib-level dep
    /// on `degenbot-v2-math` (which the harness tests use via dev-deps).
    fn v2_amt_out(r_in: u128, r_out: u128, input_in: u128) -> u128 {
        let amp = U256::from(input_in) * U256::from(997u64);
        let num = amp * U256::from(r_out);
        let den = U256::from(r_in) * U256::from(1000u64) + amp;
        (num / den).to::<u128>()
    }

    /// The pool's output for `input_in` of `src` into `dst` — the same math
    /// the harness already proves (V2 constant-product, V3/V4 `v3_amount_out`).
    fn amount_out(&self, src: Address, dst: Address, input_in: u128) -> u128 {
        match self {
            HopPool::V2(p) => {
                let r_in = if src == p.token0 {
                    p.reserve0
                } else {
                    p.reserve1
                };
                let r_out = if dst == p.token0 {
                    p.reserve0
                } else {
                    p.reserve1
                };
                Self::v2_amt_out(r_in, r_out, input_in)
            }
            HopPool::V3(p) => {
                v3_amount_out(p.sqrt_price, p.liquidity, input_in, self.zfo(src), p.fee)
            }
            HopPool::V4(p) => {
                v3_amount_out(p.sqrt_price, p.liquidity, input_in, self.zfo(src), p.fee)
            }
        }
    }

    /// Into the executor's `HopInfo`, using `pool_manager` for V4.
    fn to_hop_info(&self, pool_manager: Address, src: Address) -> HopInfo {
        match self {
            HopPool::V2(p) => HopInfo::V2(V2HopInfo {
                pool_address: p.pair,
                token0_address: p.token0,
                token1_address: p.token1,
                fee: 30,
                zfo: self.zfo(src),
            }),
            HopPool::V3(p) => HopInfo::V3(V3HopInfo {
                pool_address: p.pool,
                token0_address: p.token0,
                token1_address: p.token1,
                fee: p.fee,
                zfo: self.zfo(src),
            }),
            HopPool::V4(p) => HopInfo::V4(V4HopInfo {
                pool_manager_address: pool_manager,
                pool_id_hex: "0x0".into(),
                currency0_address: p.currency0,
                currency1_address: p.currency1,
                fee: p.fee,
                tick_spacing: p.tick_spacing,
                hook_address: Address::ZERO,
                zfo: self.zfo(src),
            }),
        }
    }
}

/// The result of a [`Harness::run_chain`] drive: the classified outcome plus
/// the predicted and measured profit.
#[derive(Debug)]
pub struct ChainResult {
    pub outcome: ExecOutcome,
    /// Per-hop outputs derived by forward traversal; `hop_outputs[last]` is
    /// the terminal output.
    pub hop_outputs: Vec<u128>,
    /// `out_terminal − optimal_input`, the profit the amounts were encoded for.
    pub predicted_profit: i128,
    /// Measured executor WETH-balance delta after `execute`. Should be ≈
    /// `predicted_profit`; a large divergence means the encoded amounts moved
    /// the wrong WETH even though the payload didn't revert.
    pub actual_weth_delta: i128,
}

impl Harness {
    /// Declaratively drive an ordered hop chain: derive amounts, build the
    /// `PathInfo`, provision funding + approvals, execute, and report the
    /// outcome plus predicted/measured profit. Generous buffers are used
    /// everywhere (the executor keeps what it doesn't touch), so this works
    /// across all-V2 flash walks and mixed V2/V3/V4 funding topologies.
    pub fn run_chain(
        &mut self,
        hops: &[Hop],
        optimal_input: u128,
        gas: u64,
    ) -> Result<ChainResult, String> {
        let n = hops.len();
        assert!(n >= 1, "run_chain needs >=1 hops");

        // 1. Forward-traverse amounts: output of hop i feeds hop i+1.
        let mut hop_outputs = Vec::with_capacity(n);
        let mut consumed = optimal_input;
        for hop in hops {
            let out = hop.pool.amount_out(hop.src, hop.dst, consumed);
            hop_outputs.push(out);
            consumed = out;
        }
        let out_terminal = consumed;
        let predicted_profit = out_terminal as i128 - optimal_input as i128;

        // 2. Build the production PathInfo.
        let path_hops: Vec<HopInfo> = hops
            .iter()
            .map(|hop| hop.pool.to_hop_info(self.pool_manager, hop.src))
            .collect();
        let path = PathInfo::new(path_hops);

        // 3. Universal funding + approvals.
        //    WETH buffer (flash/repay + terminal return, with `*2` headroom for
        //    the V2 compact's +1-wei getAmountIn round-up).
        self.fund(self.weth, self.executor, optimal_input * 2)?;
        //    Per-intermediate-token buffer: hop i (i>=1) inputs `hop_outputs[i-1]`.
        let mut funded = vec![self.weth];
        for (i, hop) in hops.iter().enumerate().skip(1) {
            if hop.src != self.weth && !funded.contains(&hop.src) {
                self.fund(hop.src, self.executor, hop_outputs[i - 1] * 2)?;
                funded.push(hop.src);
            }
        }
        //    Approve every V2 pair so its `swap`'s `transferFrom` can pull.
        for hop in hops {
            if let HopPool::V2(p) = &hop.pool {
                self.executor_approve_pair(*p)?;
            }
        }

        // 4. Measure, execute, measure.
        let before = self.balance_of(self.weth, self.executor)?.to::<u128>();
        let outcome = self.run_path(&path, optimal_input, &hop_outputs, gas)?;
        let after = self.balance_of(self.weth, self.executor)?.to::<u128>();
        let actual_weth_delta = after as i128 - before as i128;

        Ok(ChainResult {
            outcome,
            hop_outputs,
            predicted_profit,
            actual_weth_delta,
        })
    }
}

/// Assert a [`ChainResult`] is a genuine profitable execution: it reached every
/// pool, the measured WETH delta is positive, and it matches the predicted
/// profit within a small tolerance (covering ±1-wei per-hop rounding so a
/// legitimate path isn't flaky, while a silently-wrong amount — off by orders
/// of magnitude — still fails loudly).
#[track_caller]
pub fn assert_profitable(result: &ChainResult, expected_swaps: usize, label: &str) {
    assert!(
        result.outcome.executed(expected_swaps),
        "[{label}] payload must execute (reach {expected_swaps} pools): {:?}",
        result.outcome
    );
    assert!(
        result.actual_weth_delta > 0,
        "[{label}] expected a profitable (positive) WETH delta, got {:?}",
        result
    );
    // Tolerance: 0.1% of the predicted magnitude, floored at 64 wei — plenty to
    // absorb ±1-wei-per-hop rounding without masking a real mis-encoding.
    let tol = (result.predicted_profit.abs() / 1000).max(64);
    assert!(
        (result.actual_weth_delta - result.predicted_profit).abs() <= tol,
        "[{label}] measured WETH delta {} diverges from predicted {} (tol {}): {:?}",
        result.actual_weth_delta,
        result.predicted_profit,
        tol,
        result
    );
}
