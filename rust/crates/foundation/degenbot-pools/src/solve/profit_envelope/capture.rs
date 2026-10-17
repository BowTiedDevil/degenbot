//! Degenerate-path capture events — the typed data the gate hands to a
//! caller-supplied sink on the `DegenerateHop` reject exit.
//!
//! The solver performs NO file I/O (its crate contract): it assembles the
//! event and calls the sink. The sink is the caller's adapter — the engine
//! implements it over an append-only JSONL file
//! (`degenbot-bot/src/arb_engine/solver_capture.rs`); tests substitute an
//! in-memory collector.
//!
//! The JSONL ROW format lives here, on the event ([`DegeneratePathEvent::to_jsonl_row`]),
//! so the row stays byte-identical to the historical in-solver capture while
//! the file handling leaves the crate.

use crate::int_v3_hop::IntV3TickRangeSequence;

use super::derive::HopMath;

/// One hop slot of a degenerate-path capture event: its index, a
/// family/rejection descriptor string, and (concentrated liquidity only) the full tick-range
/// sequence the offline replay harness reconstructs from.
#[derive(Clone, Debug)]
pub struct CapturedHop<'a> {
    pub hop_index: usize,
    /// `"unmapped"`, `"concentrated_liquidity"`, or the family name + key scalars
    /// (e.g. `v2(r_in=..,r_out=..)`) — the row's `family` field. Produced by
    /// [`HopMath::family_label`], shared with the gate's diagnostic.
    pub family: String,
    /// The hop's concentrated liquidity tick-range sequence, or `None` for every non-concentrated-liquidity family
    /// (serialized as JSON `null`).
    pub ranges: Option<&'a IntV3TickRangeSequence>,
}

/// The typed degenerate-path capture EVENT: what the gate produced when a hop
/// could not be bounded. Borrows the hop views; hand it to a
/// [`DegeneratePathSink`], which serializes it via [`Self::to_jsonl_row`].
#[derive(Clone, Debug)]
pub struct DegeneratePathEvent<'a> {
    /// The hop index whose derivation was rejected.
    pub reject_hop: usize,
    /// The precise rejection reason (e.g. `reject=zero_liq@k=..`).
    pub reject_reason: String,
    /// Every slot of the rejected path, in order.
    pub hops: Vec<CapturedHop<'a>>,
}

impl<'a> DegeneratePathEvent<'a> {
    /// Build the event from the gate's hop views: serialize every hop's concentrated liquidity
    /// ranges (V2/Solidly/Weighted/ReserveCap are captured as their family
    /// name + key scalars — the off-line harness reconstructs concentrated liquidity from ranges,
    /// V2 from `(reserve_in, reserve_out)`, etc.).
    #[must_use]
    pub fn from_hops(
        hops: &[Option<HopMath<'a>>],
        reject_hop: usize,
        reject_reason: String,
    ) -> Self {
        let hops = hops
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                // One label shape for the row's `family` field and the
                // gate's degenerate-hop diagnostic ([`HopMath::family_label`]).
                let family = match slot {
                    None => "unmapped".to_string(),
                    Some(hop) => hop.family_label(),
                };
                let ranges = match slot {
                    Some(HopMath::ConcentratedLiquidity(ch)) => Some(ch.seq),
                    _ => None,
                };
                CapturedHop {
                    hop_index: i,
                    family,
                    ranges,
                }
            })
            .collect();
        Self {
            reject_hop,
            reject_reason,
            hops,
        }
    }

    /// Serialize the event to its one-line JSONL row (no trailing newline) —
    /// byte-identical to the historical in-solver capture record.
    #[must_use]
    pub fn to_jsonl_row(&self) -> String {
        let hops_json: Vec<serde_json::Value> = self
            .hops
            .iter()
            .map(|h| {
                let ranges = h
                    .ranges
                    .map_or(serde_json::Value::Null, concentrated_liquidity_seq_to_json);
                serde_json::json!({
                    "hop_index": h.hop_index,
                    "family": h.family,
                    "ranges": ranges,
                })
            })
            .collect();
        serde_json::json!({
            "reject_hop": self.reject_hop,
            "reject_reason": self.reject_reason,
            "n_hops": self.hops.len(),
            "hops": hops_json,
        })
        .to_string()
    }
}

/// Serialize a concentrated liquidity hop's tick-range sequence to a JSON value for the
/// degenerate-path capture harness. Each range carries the 8 primitive fields
/// the offline replay harness needs to reconstruct an `IntV3TickRangeSequence`
/// (decimal-string big-ints, matching the `HeavyPathCapture` JSONL schema in
/// `arb_engine/solver_capture.rs`).
fn concentrated_liquidity_seq_to_json(seq: &IntV3TickRangeSequence) -> serde_json::Value {
    serde_json::Value::Array(
        seq.ranges
            .iter()
            .map(|r| {
                serde_json::json!({
                    "liquidity": r.liquidity.to_string(),
                    "sqrt_price_x96": r.sqrt_price_x96.to_string(),
                    "sqrt_price_lower_x96": r.sqrt_price_lower_x96.to_string(),
                    "sqrt_price_upper_x96": r.sqrt_price_upper_x96.to_string(),
                    "gamma_numer": r.gamma_numer,
                    "fee_denom": r.fee_denom,
                    "zero_for_one": r.zero_for_one,
                    "word_boundary_prices": r.word_boundary_prices
                        .iter()
                        .map(std::string::ToString::to_string)
                        .collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// Caller-supplied sink for degenerate-path capture events (the `WalkMemo`
/// owner-handle precedent: the solver holds only a borrowed handle).
///
/// The crate performs no I/O — the engine implements this over an append-only
/// JSONL file and owns the cap (`max_paths`), the write lock, and the append;
/// tests substitute an in-memory collector. `Send + Sync` because the gate
/// fires on rayon workers; `Debug` so the handle can ride `GateDeps`.
pub trait DegeneratePathSink: std::fmt::Debug + Send + Sync {
    /// Reserve one capture slot against the sink's cap. `false` means the cap
    /// is exhausted, so the caller skips building the (100KB+) event. Must be
    /// called before [`Self::capture`]; the sink owns the counter (the retired
    /// in-solver process-global `AtomicU64`).
    fn try_reserve(&self) -> bool;

    /// Receive one capture event. Called on the solver worker thread that hit
    /// the degenerate reject, only after [`Self::try_reserve`] returned `true`.
    fn capture(&self, event: &DegeneratePathEvent<'_>);
}
