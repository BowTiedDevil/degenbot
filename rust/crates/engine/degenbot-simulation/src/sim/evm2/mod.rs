//! The evm2 pilot engine — an opt-in parallel implementation behind the
//! `evm2` feature.
//!
//! Scope is deliberately thin: a constructor over evm2's in-memory DB plus
//! the spec pin, with `tests/evm2_pilot.rs` as the differential gate (one
//! transaction through both engines, identical status/gas/output/storage
//! writes required). Every later migration stage — the `BotStateDb` storage
//! forwarding, the warm code cache, the async RPC cold-miss fallback
//! (`evm2::evm::AsyncDatabase` takes the `WrapDatabaseAsync<AlloyDB>` role),
//! and the inspector surface — builds on this seam and must keep that gate
//! green.
//!
//! evm2 settles a run as `transact` → resolve (`commit` / `discard` /
//! `detach`), the same shape as the revm engine's `transact_one` + `finalize`
//! discipline: `discard` is the finalize-never-commit equivalent, so a pilot
//! run leaves no writes in the shared read cache.

pub mod alloy_async_db;
pub mod bot_state_db;
pub mod frame_replay;
pub mod inspectors;
pub mod session;
pub mod state_override;
pub mod warm_code_cache;

#[cfg(test)]
mod parity_tests;

/// Build a pilot EVM armed with the JIT interpreter runner (`evm2-jit`
/// feature). Fail-soft by construction: a frame without a compiled program
/// falls back to the interpreter, so uncovered bytecode interprets instead
/// of diverging. `blocking` compiles synchronously — the parity gate sees
/// the compiled path on first execution instead of racing the worker pool.
#[cfg(feature = "evm2-jit")]
pub fn build_pilot_evm_jit<'a>(
    db: impl ::evm2::evm::DynDatabase + 'a,
) -> (Evm<'a, BaseEvmTypes>, ::evm2_jit::runtime::JitBackend) {
    let mut evm = build_pilot_evm(db);
    let backend = ::evm2_jit::runtime::JitBackend::new(::evm2_jit::runtime::RuntimeConfig {
        enabled: true,
        blocking: true,
        ..Default::default()
    })
    .expect("the LLVM 23.1 backend initializes in the F45 devcontainer");
    evm.set_interpreter_runner(::evm2_jit::evm2_evm::JitInterpreterRunner::new(
        backend.clone(),
    ));
    (evm, backend)
}

use ::evm2::env::BlockEnvExt;
use ::evm2::ethereum::ethereum_tx_registry;
use ::evm2::evm::InMemoryDB;
use ::evm2::{BaseEvmTypes, Evm, Precompiles, SpecId};

/// The hardfork both engines are pinned to in the parity gate. The opcodes
/// the pilot exercises are stable since Shanghai, so one mid-range spec keeps
/// the comparison exact; production spec wiring follows the block's fork.
pub const PILOT_SPEC: SpecId = SpecId::CANCUN;

/// The pilot's database — evm2's in-memory cache over an empty backing.
pub type PilotDb = InMemoryDB;

/// The pilot's EVM shape — base Ethereum types over the in-memory DB.
pub type PilotEvm = Evm<'static, BaseEvmTypes>;

/// An empty pilot database.
pub fn pilot_db() -> PilotDb {
    InMemoryDB::default()
}

/// Build an evm2 mainnet EVM over `db` at [`PILOT_SPEC`]. Generic over the
/// DB stack: the in-memory `PilotDb`, or the layered `CacheDB` over the
/// warm-cache/`BotStateDb` wrappers (which borrows the oracle + fixtures and
/// therefore shortens the EVM's lifetime).
pub fn build_pilot_evm<'a>(db: impl ::evm2::evm::DynDatabase + 'a) -> Evm<'a, BaseEvmTypes> {
    Evm::new(
        PILOT_SPEC,
        BlockEnvExt::default(),
        ethereum_tx_registry(PILOT_SPEC),
        db,
        Precompiles::base(PILOT_SPEC),
    )
}
