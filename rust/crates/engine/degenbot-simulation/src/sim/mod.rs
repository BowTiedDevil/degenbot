//! Simulation domain — the in-process evm2 engine.
//!
//! `sim::evm2` is the in-process EVM execution core (evm2 over the
//! `CacheDB<WarmCodeCache<BotStateDb<BlockingAsyncDb<AlloyAsyncDb>>>` stack).
//! The revm engine it replaced retired at the ADR-089 hard cutover; the
//! engine-neutral capture vocabulary lives in `sim::capture` (whose access
//! lists feed the `sim::hot_slots` store that `sim::prefetch`'s prewarm reads),
//! and the frame-replay vocabulary in `sim::frame_replay`. The settlement-arbitrage
//! strategy (the dispatch fan-out + the 7-call bundle) lives in
//! `degenbot-arbitrage` (ADR-019 D4/D7, decision R).

pub mod capture;
pub mod divergence_probe;
pub mod evm2;
pub mod frame_replay;
pub mod hot_slots;
pub mod journal_pools;
pub mod prefetch;
pub mod sim_metrics;
pub mod state_override;
pub mod storage_memo;

pub use capture::{CapturedSwap, SwapFamily};
pub use frame_replay::{
    BaseFeeSource, FrameRpcCounter, PredStatus, ReplayFrameError, ReplayableTx, ScratchBlock,
    SequenceReplayError,
};
pub use hot_slots::{
    HotSlotStore, HOT_SLOT_DECAY_BLOCKS, MAX_HOT_ADDRESSES, MAX_HOT_SLOTS_PER_ADDRESS,
};
pub use prefetch::{
    path_prefetch_addresses, path_prefetch_slots, prefetch_into_memo, prefetch_paths_into_memo,
    prewarm_plan, FamilySlotLayout, PrefetchFacts, PrefetchReport, PrefetchSkip, PrewarmPlan,
    V2SlotIndices, V3SlotIndices,
};
pub use state_override::SimulationOverrideParams;
pub use storage_memo::{StorageMemo, StorageMemoCache};

/// How one settled run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunVerdict {
    Success,
    Reverted,
    Halted,
}
