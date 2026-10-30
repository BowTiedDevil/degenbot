//! The layered-stack parity gate: the same cold-served transaction through
//! the production wrapper shapes on both engines — `CacheDB` over
//! `WarmCodeCache` over `BotStateDb` over a scripted fallback — must produce
//! identical verdicts, and the second run at the same block must serve the
//! contract account from the warm cache on both engines (zero fallback
//! reads). Unit-test placement (not `tests/`) so the gate can name the
//! engine's regular dependencies (`parking_lot`, the substrate oracle).

use alloy::primitives::{Address, Bytes, TxKind, B256, U256};
use std::cell::Cell;
use std::rc::Rc;
// ── Inspector parity ──

/// Store a 32-byte word in memory, then `LOG1` with topic 0xaa and the
/// whole word as data.
fn log1_code() -> Bytes {
    let mut code = vec![0x7f];
    code.extend_from_slice(&[0xde; 32]);
    code.extend_from_slice(&[0x5f, 0x52, 0x60, 0xaa, 0x60, 0x20, 0x60, 0x00, 0xa1, 0x00]);
    Bytes::from(code)
}

/// The captured-log shape both inspectors are compared on.
#[derive(Debug, PartialEq, Eq)]
struct CapturedLog {
    address: Address,
    topics: Vec<B256>,
    data: Bytes,
}

fn normalize(log: &alloy::primitives::Log) -> CapturedLog {
    CapturedLog {
        address: log.address,
        topics: log.topics().to_vec(),
        data: log.data.data.clone(),
    }
}

/// The revm side: an inspector capturing LOG events via `log_full` (revm's
/// LOG-opcode hook — `log` fires only for the frame-init path).
fn run_revm_inspected(code: Bytes) -> (Verdict, Vec<CapturedLog>) {
    use revm::context::TxEnv;
    use revm::context_interface::result::{ExecutionResult, Output};
    use revm::database::CacheDB;
    use revm::database_interface::EmptyDB;
    use revm::state::AccountInfo;
    use revm::{InspectEvm, MainBuilder, MainContext};
    use std::cell::RefCell;

    struct Capture {
        logs: Rc<RefCell<Vec<alloy::primitives::Log>>>,
    }

    impl<CTX, INTR: revm::interpreter::InterpreterTypes> revm::inspector::Inspector<CTX, INTR>
        for Capture
    {
        fn log_full(
            &mut self,
            _interp: &mut revm::interpreter::Interpreter<INTR>,
            _ctx: &mut CTX,
            log: alloy::primitives::Log,
        ) {
            self.logs.borrow_mut().push(log);
        }
    }

    let mut db = CacheDB::new(EmptyDB::new());
    db.insert_account_info(
        CALLER,
        AccountInfo {
            balance: U256::from(FUND),
            ..Default::default()
        },
    );
    let bytecode = revm::bytecode::Bytecode::new_legacy(code);
    db.insert_account_info(
        CONTRACT,
        AccountInfo {
            balance: U256::ZERO,
            nonce: 1,
            code_hash: bytecode.hash_slow(),
            code: Some(bytecode),
            ..Default::default()
        },
    );

    let logs = Rc::new(RefCell::new(Vec::new()));
    let mut ctx = revm::context::Context::mainnet().with_db(db);
    ctx.cfg
        .set_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::CANCUN);
    let mut evm = ctx.build_mainnet_with_inspector(Capture { logs: logs.clone() });

    let tx = TxEnv::builder()
        .caller(CALLER)
        .kind(TxKind::Call(CONTRACT))
        .data(Bytes::new())
        .value(U256::ZERO)
        .gas_limit(GAS_LIMIT)
        .nonce(0)
        .build()
        .expect("well-formed pilot tx");
    let settled = evm
        .inspect_one(tx, Capture { logs: logs.clone() })
        .expect("revm inspect");

    let gas_used = settled.tx_gas_used();
    let verdict = match settled {
        ExecutionResult::Success { output, .. } => {
            let output = match output {
                Output::Call(bytes) => bytes,
                Output::Create(..) => Bytes::new(),
            };
            Verdict {
                status: true,
                gas_used,
                output,
                slot_value: U256::ZERO,
            }
        }
        other => panic!("revm run did not succeed: {other:?}"),
    };
    let captured = logs.borrow().iter().map(normalize).collect();
    (verdict, captured)
}

/// The evm2 side: the same capture through evm2's `log` hook — which fires
/// for LOG opcodes directly (no `log`/`log_full` split).
fn run_evm2_inspected(code: Bytes) -> (Verdict, Vec<CapturedLog>) {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;

    struct Capture {
        logs: Vec<alloy::primitives::Log>,
    }

    impl ::evm2::Inspector<::evm2::BaseEvmTypes> for Capture {
        fn log(
            &mut self,
            log: &alloy::primitives::Log,
            _host: &mut <::evm2::BaseEvmTypes as ::evm2::EvmTypesHost>::Host<'_>,
        ) {
            self.logs.push(log.clone());
        }
    }

    let mut db = super::pilot_db();
    db.insert_account_info(
        &CALLER,
        ::evm2::evm::AccountInfo::default().with_balance(U256::from(FUND)),
    );
    db.insert_account_info(
        &CONTRACT,
        ::evm2::evm::AccountInfo::default()
            .with_nonce(1)
            .with_code(::evm2::bytecode::Bytecode::new_legacy(code)),
    );
    let mut evm = build_pilot_evm(db);

    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(CONTRACT),
            input: Bytes::new(),
            gas_limit: GAS_LIMIT,
            nonce: 0,
            ..Default::default()
        }),
        CALLER,
    );
    let mut capture = Capture { logs: Vec::new() };
    let executed = evm
        .transact_with_inspector(&tx, &mut capture)
        .expect("evm2 inspected transact");
    let result = executed.discard();
    let verdict = Verdict {
        status: result.status,
        gas_used: result.tx_gas_used(),
        output: result.output,
        slot_value: U256::ZERO,
    };
    let captured = capture.logs.iter().map(normalize).collect();
    (verdict, captured)
}

// ── The JIT interpreter runner (evm2-jit) ──

/// `PUSH0 CALLDATALOAD PUSH0 MSTORE PUSH1 32 PUSH0 RETURN` — echo the
/// calldata word back as 32-byte return data.
#[cfg(feature = "evm2-jit")]
fn return_code() -> Bytes {
    Bytes::from(vec![0x5f, 0x35, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3])
}

/// Run one call against `code` through a pilot EVM built by `build` (the
/// plain interpreter or the JIT-armed variant).
#[cfg(feature = "evm2-jit")]
fn run_evm2_with(
    code: Bytes,
    input: Bytes,
    build: impl FnOnce(
        ::evm2::evm::InMemoryDB,
    ) -> (
        ::evm2::Evm<'static, ::evm2::BaseEvmTypes>,
        Option<::evm2_jit::runtime::JitBackend>,
    ),
) -> (Verdict, Option<::evm2_jit::runtime::RuntimeStatsSnapshot>) {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;

    let mut db = super::pilot_db();
    db.insert_account_info(
        &CALLER,
        ::evm2::evm::AccountInfo::default().with_balance(U256::from(FUND)),
    );
    db.insert_account_info(
        &CONTRACT,
        ::evm2::evm::AccountInfo::default()
            .with_nonce(1)
            .with_code(::evm2::bytecode::Bytecode::new_legacy(code)),
    );
    let (mut evm, backend) = build(db);

    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(CONTRACT),
            input,
            gas_limit: GAS_LIMIT,
            nonce: 0,
            ..Default::default()
        }),
        CALLER,
    );
    let executed = evm.transact(&tx).expect("evm2 jit transact");
    let slot_value = executed
        .account_changes()
        .find(|changes| changes.address == CONTRACT)
        .and_then(|changes| changes.storage.slots.get(&U256::from(SLOT)))
        .map(|slot| slot.value.current)
        .unwrap_or_default();
    let result = executed.discard();
    let verdict = Verdict {
        status: result.status,
        gas_used: result.tx_gas_used(),
        output: result.output,
        slot_value,
    };
    (verdict, backend.map(|b| b.stats()))
}

/// The compiled path must be bit-identical to the interpreter it replaces:
/// same verdict on every pilot bytecode, and the stats must show the frame
/// actually executed compiled code (no silent fallback sweep).
#[cfg(feature = "evm2-jit")]
#[test]
fn jit_runner_matches_the_interpreter() {
    for (name, code) in [
        ("store", store_code()),
        ("return", return_code()),
        ("log1", log1_code()),
    ] {
        let (jit, stats) = run_evm2_with(code.clone(), word(VALUE), |db| {
            let (evm, backend) = super::build_pilot_evm_jit(db);
            (evm, Some(backend))
        });
        let (interp, _) = run_evm2_with(code, word(VALUE), |db| (build_pilot_evm(db), None));
        assert_eq!(jit, interp, "jit diverged from the interpreter on {name}");
        assert!(
            stats.expect("jit backend stats").lookup_hits > 0,
            "the {name} frame interpreted — the compiled path went unproven"
        );
    }
}

// ── Phase 1: engine core — overrides, session, isolation ──

/// Canonical mainnet WETH9 + PoolManager (the override params' targets).
const WETH: Address = Address::new([
    0xc0, 0x2a, 0xaa, 0x39, 0xb2, 0x23, 0xfe, 0x8d, 0x0a, 0x0e, 0x5c, 0x4f, 0x27, 0xea, 0xd9, 0x08,
    0x3c, 0x75, 0x6c, 0xc2,
]);
const PM: Address = Address::new([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x44, 0x4c, 0x5d, 0xc7, 0x5c, 0xb3, 0x58, 0x38, 0x0d, 0x2e,
    0x3d, 0xe0, 0x8a, 0x90,
]);
const OWNER: Address = Address::with_last_byte(0x9e);
const INJECTED: Address = Address::with_last_byte(0xbb);
const RUNTIME: &[u8] = &[0xde, 0xad, 0xbe, 0xef];

/// The override params the gate applies to both engines (the warmup slots
/// derive from the same helper the production path uses).
fn override_params() -> crate::sim::evm::SimulationOverrideParams {
    crate::sim::evm::SimulationOverrideParams {
        owner: OWNER,
        inject_code: true,
        injected_address: Some(INJECTED),
        runtime_bytecode: Bytes::from_static(RUNTIME),
        warmup: degenbot_executor::compute_simulation_warmup_slots(INJECTED, WETH),
        weth_address: WETH,
        pool_manager_address: PM,
    }
}

/// The override application writes identical state on both engines:
/// balances, injected code, the three 1-wei warmup touches, and the
/// operational WETH9 balanceOf raise.
#[test]
fn override_application_writes_match_across_engines() {
    let params = override_params();

    // revm side — the production apply.
    let mut revm_db = revm::database::CacheDB::new(revm::database_interface::EmptyDB::new());
    crate::sim::evm::apply_simulation_overrides(&mut revm_db, &params)
        .expect("revm override application");

    // evm2 side — the port.
    let mut evm2_db = super::pilot_db();
    super::state_override::apply_simulation_overrides(&mut evm2_db, &params);

    let eth = |e: u64| U256::from(e) * U256::from(10u128.pow(18));
    use ::evm2::evm::Database;
    use revm::database_interface::DatabaseRef;
    // Balances.
    assert_eq!(
        revm_db.basic_ref(OWNER).unwrap().unwrap().balance,
        evm2_db.get_account(&OWNER).unwrap().unwrap().balance,
    );
    assert_eq!(
        revm_db.basic_ref(INJECTED).unwrap().unwrap().balance,
        evm2_db.get_account(&INJECTED).unwrap().unwrap().balance,
    );
    assert_eq!(revm_db.basic_ref(OWNER).unwrap().unwrap().balance, eth(100));
    // Injected code hash.
    assert_eq!(
        revm_db.basic_ref(INJECTED).unwrap().unwrap().code_hash,
        evm2_db.get_account(&INJECTED).unwrap().unwrap().code_hash,
    );
    // The three 1-wei warmup touches + the WETH9 operational raise.
    let one_wei = U256::from(1u64);
    for (account, slot, want) in [
        (WETH, params.warmup.weth_balance, eth(10)),
        (PM, params.warmup.erc6909_weth, one_wei),
        (PM, params.warmup.erc6909_native, one_wei),
    ] {
        let revm_got = revm_db.storage_ref(account, slot).unwrap();
        let evm2_got = evm2_db.get_storage(&account, &slot).unwrap();
        assert_eq!(
            (revm_got, evm2_got),
            (want, want),
            "slot {slot} on {account}"
        );
    }
}

/// The settle verb: one store transaction through `Evm2SimSession::run` and
/// the revm `SimSession::run` over identical stacks — identical verdict,
/// gas, output, and storage write.
#[test]
fn session_settle_matches_revm_session() {
    use crate::sim::evm::{SimInspector, SimSession};
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;

    let fixture = Fixture {
        contract_code: store_code(),
    };

    // revm side — the production session over the layered stack.
    let (revm_verdict, revm_gas, revm_output, revm_writes) = {
        use revm::context::TxEnv;
        use revm::{MainBuilder, MainContext};

        let scripted = RevmScripted {
            fixture: &fixture,
            contract_reads: Rc::new(Cell::new(0)),
        };
        let bsd = RevmBotStateDb::new(&degenbot_substrate::NO_SIM_ANCHOR, scripted);
        let warm =
            RevmWarmCodeCache::with_owner(RevmWarmCodeCacheInner::shared_default(), BLOCK, bsd);
        let db = revm::database::CacheDB::new(warm);
        let mut ctx = revm::context::Context::mainnet().with_db(db);
        ctx.cfg
            .set_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::CANCUN);
        let mut evm = ctx.build_mainnet_with_inspector(SimInspector::default());
        let mut session = SimSession::new(&mut evm);
        let tx = TxEnv::builder()
            .caller(CALLER)
            .kind(TxKind::Call(CONTRACT))
            .data(word(VALUE))
            .value(U256::ZERO)
            .gas_limit(GAS_LIMIT)
            .nonce(0)
            .build()
            .expect("well-formed pilot tx");
        let run = session.run(tx).expect("revm session run");
        let writes: Vec<(Address, U256, U256)> = run
            .state
            .get(&CONTRACT)
            .map(|account| {
                account
                    .storage
                    .iter()
                    .map(|(k, s)| (CONTRACT, *k, s.present_value()))
                    .collect()
            })
            .unwrap_or_default();
        (run.verdict, run.gas_used, run.output, writes)
    };

    // evm2 side — the ported session over the pilot layered stack.
    let (evm2_verdict, evm2_gas, evm2_output, evm2_writes) = {
        use super::session::Evm2SimSession;

        let scripted = Evm2Scripted {
            fixture: &fixture,
            contract_reads: Rc::new(Cell::new(0)),
        };
        let bsd = BotStateDb::new(&degenbot_substrate::NO_SIM_ANCHOR, scripted);
        let warm = WarmCodeCache::with_owner(WarmCodeCacheInner::shared_default(), BLOCK, bsd);
        let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(warm));
        let mut evm = build_pilot_evm(db);
        let mut session = Evm2SimSession::new(&mut evm);
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(CONTRACT),
                input: word(VALUE),
                gas_limit: GAS_LIMIT,
                nonce: 0,
                ..Default::default()
            }),
            CALLER,
        );
        let run = session.run(&tx).expect("evm2 session run");
        (run.verdict, run.gas_used, run.output, run.storage_writes)
    };

    assert_eq!(
        (revm_verdict, revm_gas, revm_output),
        (evm2_verdict, evm2_gas, evm2_output),
        "session verdicts diverge"
    );
    let want_write = (CONTRACT, U256::from(SLOT), U256::from(VALUE));
    assert!(
        revm_writes.contains(&want_write),
        "revm sanity: {revm_writes:?}"
    );
    assert!(
        evm2_writes.contains(&want_write),
        "evm2 write missing: {evm2_writes:?}"
    );
}

/// The isolation contract: a settled run's writes are invisible to the next
/// run and to the view reads between runs — on both engines.
#[test]
fn a_run_is_invisible_to_the_next_run_on_both_engines() {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;

    let fixture = Fixture {
        contract_code: store_code(),
    };
    let scripted = Evm2Scripted {
        fixture: &fixture,
        contract_reads: Rc::new(Cell::new(0)),
    };
    let bsd = BotStateDb::new(&degenbot_substrate::NO_SIM_ANCHOR, scripted);
    let warm = WarmCodeCache::with_owner(WarmCodeCacheInner::shared_default(), BLOCK, bsd);
    let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(warm));
    let mut evm = build_pilot_evm(db);
    let mut session = super::session::Evm2SimSession::new(&mut evm);
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(CONTRACT),
            input: word(VALUE),
            gas_limit: GAS_LIMIT,
            nonce: 0,
            ..Default::default()
        }),
        CALLER,
    );

    let first = session.run(&tx).expect("first run");
    assert_eq!(first.verdict, crate::sim::evm::RunVerdict::Success);
    // The settled write is invisible to the view between runs.
    let between = session
        .read_word(CONTRACT, U256::from(SLOT))
        .expect("view read");
    assert_eq!(
        between,
        U256::ZERO,
        "the first run's write leaked into the shared view"
    );
    // And to the next run's execution (the contract re-stores from zero —
    // verdict + gas identical to the first run).
    let second = session.run(&tx).expect("second run");
    assert_eq!(
        second.gas_used, first.gas_used,
        "the next run saw the first run's state"
    );
}

/// Typed view reads: the same packed reserves word decodes to the same parts
/// through both sessions' slot tables.
#[test]
fn view_reads_match_revm_session() {
    use crate::sim::evm::{SimInspector, SimSession};
    use degenbot_pools::V2_RESERVES_SLOT;

    // reserve0 = 1000, reserve1 = 2000 packed at the pair's slot 8.
    let reserves_word = U256::from(1000u128) | (U256::from(2000u128) << 112);

    // revm side.
    let revm_parts = {
        use revm::database_interface::DatabaseRef;
        use revm::{MainBuilder, MainContext};

        struct ReservesDb {
            word: U256,
        }
        impl DatabaseRef for ReservesDb {
            type Error = core::convert::Infallible;
            fn basic_ref(
                &self,
                _: Address,
            ) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
                Ok(None)
            }
            fn code_by_hash_ref(&self, _: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
                Ok(revm::bytecode::Bytecode::default())
            }
            fn storage_ref(&self, address: Address, slot: U256) -> Result<U256, Self::Error> {
                Ok(
                    if address == CONTRACT && slot == U256::from(V2_RESERVES_SLOT) {
                        self.word
                    } else {
                        U256::ZERO
                    },
                )
            }
            fn block_hash_ref(&self, _: u64) -> Result<B256, Self::Error> {
                Ok(B256::ZERO)
            }
        }
        let db = revm::database::CacheDB::new(ReservesDb {
            word: reserves_word,
        });
        let ctx = revm::context::Context::mainnet().with_db(db);
        let mut evm = ctx.build_mainnet_with_inspector(SimInspector::default());
        let mut session = SimSession::new(&mut evm);
        session
            .read_v2_reserves(CONTRACT)
            .expect("revm reserves read")
    };

    // evm2 side.
    let evm2_parts = {
        struct ReservesDb {
            word: U256,
        }
        impl ::evm2::evm::Database for ReservesDb {
            type Error = core::convert::Infallible;
            fn get_account(
                &mut self,
                _: &Address,
            ) -> Result<Option<::evm2::evm::AccountInfo>, Self::Error> {
                Ok(None)
            }
            fn get_code_by_hash(
                &mut self,
                _: &B256,
            ) -> Result<::evm2::bytecode::Bytecode, Self::Error> {
                Ok(::evm2::bytecode::Bytecode::default())
            }
            fn get_storage(
                &mut self,
                address: &Address,
                key: &::evm2::interpreter::Word,
            ) -> Result<::evm2::interpreter::Word, Self::Error> {
                Ok(
                    if *address == CONTRACT && *key == U256::from(V2_RESERVES_SLOT) {
                        self.word
                    } else {
                        U256::ZERO
                    },
                )
            }
            fn get_block_hash(
                &mut self,
                _: &::evm2::interpreter::Word,
            ) -> Result<B256, Self::Error> {
                Ok(B256::ZERO)
            }
        }
        let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(ReservesDb {
            word: reserves_word,
        }));
        let mut evm = build_pilot_evm(db);
        let mut session = super::session::Evm2SimSession::new(&mut evm);
        session
            .read_v2_reserves(CONTRACT)
            .expect("evm2 reserves read")
    };

    assert_eq!(revm_parts, evm2_parts);
}

// ── Phase 2: composite inspector suite ──

use std::collections::BTreeSet;
use std::sync::Arc;

use degenbot_decoders::v2_swap_decoder::V2_SWAP_TOPIC;
use degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC;
use degenbot_decoders::v4_swap_decoder::V4_SWAP_TOPIC;

const CHILD: Address = Address::with_last_byte(0x99);

/// Append a minimal-length push of `v` to the bytecode.
fn push_u256(code: &mut Vec<u8>, v: &U256) {
    let bytes = v.to_be_bytes::<32>();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(31).min(31);
    let trimmed = &bytes[first..];
    code.push(0x5f + trimmed.len() as u8);
    code.extend_from_slice(trimmed);
}

/// Append a LOGn emission: `data_words` (32-byte BE words) written to memory
/// at offset 0, then LOG with `topics`. Reusing the memory region across
/// logs is safe — each LOG executes immediately after its own MSTOREs.
fn push_log(code: &mut Vec<u8>, topics: &[B256], data_words: &[U256]) {
    for (i, w) in data_words.iter().enumerate() {
        push_u256(code, w);
        code.extend([0x60, 32 * i as u8]); // PUSH1 offset
        code.push(0x52); // MSTORE
    }
    for t in topics.iter().rev() {
        push_u256(code, &U256::from_be_slice(t.as_slice()));
    }
    code.extend([0x60, 32 * data_words.len() as u8]); // PUSH1 size
    code.push(0x60);
    code.push(0x00); // PUSH1 offset 0
    code.push(0xa0 + topics.len() as u8); // LOGn
}

/// The V2-swap-shaped log the fixtures emit (emitter-bound topics).
fn v2_log_parts(emitter_byte: u8) -> ([B256; 3], [U256; 4]) {
    let sender = Address::with_last_byte(0x11);
    let to = Address::with_last_byte(0x22);
    let _ = emitter_byte;
    (
        [V2_SWAP_TOPIC, sender.into_word(), to.into_word()],
        [
            U256::from(1_000_000_000_000_000_000_u64), // amount0_in
            U256::ZERO,
            U256::ZERO,
            U256::from(3_000_000_000_u64), // amount1_out
        ],
    )
}

/// Bytecode: emit the V2/V3/V4 swap-family logs, STOP.
fn all_families_code() -> Vec<u8> {
    let mut code = Vec::new();
    let (v2_topics, v2_data) = v2_log_parts(0x00);
    push_log(&mut code, &v2_topics, &v2_data);
    push_log(
        &mut code,
        &[
            V3_SWAP_TOPIC,
            Address::with_last_byte(0x33).into_word(),
            Address::with_last_byte(0x44).into_word(),
        ],
        &[
            U256::from(500u64),
            U256::from(4000u64),
            U256::from(12345u64),
            U256::from(999u64),
            U256::from(7u64),
        ],
    );
    push_log(
        &mut code,
        &[
            V4_SWAP_TOPIC,
            B256::from(U256::from(77u64)),
            Address::with_last_byte(0x55).into_word(),
        ],
        &[
            U256::from(111u64),
            U256::from(222u64),
            U256::from(333u64),
            U256::from(444u64),
            U256::from(5u64),
            U256::from(6u64),
        ],
    );
    code.push(0x00); // STOP
    code
}

/// Bytecode: CALL `child` with empty in/out + full-ish gas, POP the result.
fn call_child_code(child: Address) -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 0..5 {
        code.push(0x5f); // out_size, out_offset, in_size, in_offset, value
    }
    code.push(0x73); // PUSH20
    code.extend_from_slice(child.as_slice());
    code.extend([0x61, 0xff, 0xff]); // PUSH2 0xffff gas
    code.push(0xf1); // CALL
    code.push(0x50); // POP
    code
}

/// A scripted fallback serving code for a fixed address set (the
/// multi-contract fixtures).
#[derive(Clone)]
struct CodeMap(Vec<(Address, Vec<u8>)>);

impl CodeMap {
    fn code_at(&self, address: &Address) -> Bytes {
        self.0
            .iter()
            .find(|(a, _)| a == address)
            .map(|(_, c)| Bytes::from(c.clone()))
            .unwrap_or_default()
    }
}

struct RevmCodeMap<'f>(&'f CodeMap);

impl revm::database_interface::DatabaseRef for RevmCodeMap<'_> {
    type Error = core::convert::Infallible;
    fn basic_ref(&self, address: Address) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        let code = revm::bytecode::Bytecode::new_raw(self.0.code_at(&address));
        Ok(Some(revm::state::AccountInfo {
            balance: U256::ZERO,
            nonce: 0,
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        }))
    }
    fn code_by_hash_ref(&self, _: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        Ok(revm::bytecode::Bytecode::default())
    }
    fn storage_ref(&self, _: Address, _: U256) -> Result<U256, Self::Error> {
        Ok(U256::ZERO)
    }
    fn block_hash_ref(&self, _: u64) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

/// Owns its map: evm2's inspector downcast works on a `'static` trait
/// object, so the DB stack must not borrow the fixture.
struct Evm2CodeMap(CodeMap);

impl ::evm2::evm::Database for Evm2CodeMap {
    type Error = core::convert::Infallible;
    fn get_account(
        &mut self,
        address: &Address,
    ) -> Result<Option<::evm2::evm::AccountInfo>, Self::Error> {
        Ok(Some(::evm2::evm::AccountInfo::default().with_code(
            ::evm2::bytecode::Bytecode::new_raw(self.0.code_at(address)),
        )))
    }
    fn get_code_by_hash(&mut self, _: &B256) -> Result<::evm2::bytecode::Bytecode, Self::Error> {
        Ok(::evm2::bytecode::Bytecode::default())
    }
    fn get_storage(
        &mut self,
        _: &Address,
        _: &::evm2::interpreter::Word,
    ) -> Result<::evm2::interpreter::Word, Self::Error> {
        Ok(U256::ZERO)
    }
    fn get_block_hash(&mut self, _: &::evm2::interpreter::Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

/// The captured swap set is identical across engines over V2/V3/V4
/// swap-family logs emitted from one contract.
#[test]
fn swap_capture_matches_across_engines() {
    use crate::sim::evm::SwapEventCaptureInspector;
    use revm::context::TxEnv;
    use revm::{InspectEvm, MainBuilder, MainContext};

    let codes = CodeMap(vec![(CONTRACT, all_families_code())]);

    // revm side — the production composite's swap member.
    let revm_swaps = {
        let db = revm::database::CacheDB::new(RevmCodeMap(&codes));
        let mut ctx = revm::context::Context::mainnet().with_db(db);
        ctx.cfg
            .set_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::CANCUN);
        let mut evm = ctx.build_mainnet_with_inspector(crate::sim::evm::SimInspector::default());
        // `inspect_one`'s inspector type is fixed by the build — the full
        // `SimInspector` tuple; only the swap member's handle is read.
        let (collector, _al) = crate::sim::evm::AccessListCollector::new();
        let (trace_insp, _trace) = crate::sim::evm::CallTraceInspector::new();
        let (swap_insp, swap_handle) = SwapEventCaptureInspector::new();
        let tx = TxEnv::builder()
            .caller(CALLER)
            .kind(TxKind::Call(CONTRACT))
            .value(U256::ZERO)
            .gas_limit(GAS_LIMIT)
            .nonce(0)
            .build()
            .expect("well-formed tx");
        let result = evm
            .inspect_one(tx, (collector, (trace_insp, swap_insp)))
            .expect("revm inspect");
        assert!(result.is_success());
        // Read the count before the drain — `take_swaps` resets it.
        assert_eq!(swap_handle.log_full_count(), 3);
        swap_handle.take_swaps()
    };

    // evm2 side — the composite inspector.
    let evm2_swaps = {
        use super::inspectors::Evm2SimInspector;
        use ::evm2::ethereum::TxEnvelope;
        use alloy::consensus::transaction::Recovered;
        use alloy::consensus::TxLegacy;

        let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(Evm2CodeMap(codes.clone())));
        let mut evm = build_pilot_evm(db);
        evm.set_inspector(Evm2SimInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(CONTRACT),
                gas_limit: GAS_LIMIT,
                nonce: 0,
                ..Default::default()
            }),
            CALLER,
        );
        let result = evm.transact(&tx).expect("evm2 transact").discard();
        assert!(result.status);
        let insp = evm
            .inspector_mut()
            .and_then(|i| i.downcast_mut::<Evm2SimInspector>())
            .expect("composite inspector set");
        assert_eq!(insp.log_count(), 3);
        insp.take_swaps()
    };

    assert_eq!(revm_swaps.len(), 3, "revm sanity: {revm_swaps:?}");
    assert_eq!(revm_swaps, evm2_swaps, "swap captures diverge");
    let families: Vec<_> = evm2_swaps.iter().map(|s| s.family).collect();
    assert_eq!(
        families,
        [
            crate::sim::evm::SwapFamily::V2,
            crate::sim::evm::SwapFamily::V3,
            crate::sim::evm::SwapFamily::V4
        ]
    );
}

/// A swap emitted inside a reverting sub-call is captured into the
/// reverted-frame buffer (NOT the committed set) on both engines, and the
/// call trace attributes the revert to the child frame identically.
#[test]
fn reverting_subcall_capture_matches_across_engines() {
    use crate::sim::evm::{CallTraceInspector, FrameOutcome, SwapEventCaptureInspector};
    use revm::context::TxEnv;
    use revm::{InspectEvm, MainBuilder, MainContext};

    // CHILD: emit a V2 swap log, then REVERT.
    let mut child_code = Vec::new();
    let (topics, data) = v2_log_parts(0x99);
    push_log(&mut child_code, &topics, &data);
    child_code.extend([0x5f, 0x5f, 0xfd]); // PUSH0 PUSH0 REVERT

    // CONTRACT: CALL CHILD, emit its own V2 swap log, STOP.
    let mut parent_code = call_child_code(CHILD);
    let (ptopics, pdata) = v2_log_parts(0x01);
    push_log(&mut parent_code, &ptopics, &pdata);
    parent_code.push(0x00); // STOP

    let codes = CodeMap(vec![(CONTRACT, parent_code), (CHILD, child_code)]);

    // revm side.
    let (revm_swaps, revm_reverted, revm_trace) = {
        let db = revm::database::CacheDB::new(RevmCodeMap(&codes));
        let mut ctx = revm::context::Context::mainnet().with_db(db);
        ctx.cfg
            .set_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::CANCUN);
        let mut evm = ctx.build_mainnet_with_inspector(crate::sim::evm::SimInspector::default());
        let (collector, _al) = crate::sim::evm::AccessListCollector::new();
        let (trace_insp, trace_handle) = CallTraceInspector::new();
        let (swap_insp, swap_handle) = SwapEventCaptureInspector::new();
        let tx = TxEnv::builder()
            .caller(CALLER)
            .kind(TxKind::Call(CONTRACT))
            .value(U256::ZERO)
            .gas_limit(GAS_LIMIT)
            .nonce(0)
            .build()
            .expect("well-formed tx");
        let result = evm
            .inspect_one(tx, (collector, (trace_insp, swap_insp)))
            .expect("revm inspect");
        assert!(result.is_success(), "parent survives the child revert");
        (
            swap_handle.take_swaps(),
            swap_handle.take_reverted_swaps(),
            trace_handle.take_trace(),
        )
    };

    // evm2 side.
    let (evm2_swaps, evm2_reverted, evm2_trace) = {
        use super::inspectors::Evm2SimInspector;
        use ::evm2::ethereum::TxEnvelope;
        use alloy::consensus::transaction::Recovered;
        use alloy::consensus::TxLegacy;

        let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(Evm2CodeMap(codes.clone())));
        let mut evm = build_pilot_evm(db);
        evm.set_inspector(Evm2SimInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(CONTRACT),
                gas_limit: GAS_LIMIT,
                nonce: 0,
                ..Default::default()
            }),
            CALLER,
        );
        let result = evm.transact(&tx).expect("evm2 transact").discard();
        assert!(result.status, "parent survives the child revert");
        let insp = evm
            .inspector_mut()
            .and_then(|i| i.downcast_mut::<Evm2SimInspector>())
            .expect("composite inspector set");
        (
            insp.take_swaps(),
            insp.take_reverted_swaps(),
            insp.take_trace(),
        )
    };

    // Committed = the parent's swap only; reverted = the child's.
    assert_eq!(revm_swaps.len(), 1, "revm committed: {revm_swaps:?}");
    assert_eq!(revm_reverted.len(), 1, "revm reverted: {revm_reverted:?}");
    assert_eq!(revm_swaps, evm2_swaps, "committed swap sets diverge");
    assert_eq!(revm_reverted, evm2_reverted, "reverted swap sets diverge");

    // The trace: two frames — the root call (success) and the child call
    // (revert) — identical across engines.
    assert_eq!(
        revm_trace.frames.len(),
        2,
        "revm sanity: {:?}",
        revm_trace.frames
    );
    assert_eq!(revm_trace.frames, evm2_trace.frames, "call traces diverge");
    assert!(matches!(
        evm2_trace.frames[1].outcome,
        Some(FrameOutcome::Revert { .. })
    ));
    assert!(evm2_trace.deepest_revert().is_some());
}

/// The EIP-2930 access list the collector records is identical across
/// engines — and on evm2 it matches the executed transaction's account
/// changes (the ADR-019 D3 property on the evm2 journal).
#[test]
fn access_list_matches_across_engines_and_journal() {
    use crate::sim::evm::AccessListCollector;
    use revm::context::TxEnv;
    use revm::{InspectEvm, MainBuilder, MainContext};

    // CONTRACT: SLOAD slot 1, SSTORE slot 2 = 0x99, CALL CHILD, STOP.
    let mut contract_code = vec![0x60, 0x01, 0x54, 0x60, 0x99, 0x60, 0x02, 0x55];
    contract_code.extend(call_child_code(CHILD));
    contract_code.push(0x00);
    // CHILD: SLOAD slot 7, STOP.
    let child_code = vec![0x60, 0x07, 0x54, 0x00];

    let codes = CodeMap(vec![(CONTRACT, contract_code), (CHILD, child_code)]);
    let want: BTreeSet<(Address, B256)> = [
        (CONTRACT, B256::from(U256::from(1u64))),
        (CONTRACT, B256::from(U256::from(2u64))),
        (CHILD, B256::from(U256::from(7u64))),
    ]
    .into_iter()
    .collect();

    let al_set = |al: alloy::rpc::types::eth::AccessList| -> BTreeSet<(Address, B256)> {
        al.iter()
            .flat_map(|item| item.storage_keys.iter().map(move |k| (item.address, *k)))
            .collect()
    };

    // revm side.
    let revm_al = {
        let db = revm::database::CacheDB::new(RevmCodeMap(&codes));
        let mut ctx = revm::context::Context::mainnet().with_db(db);
        ctx.cfg
            .set_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::CANCUN);
        let mut evm = ctx.build_mainnet_with_inspector(crate::sim::evm::SimInspector::default());
        let (collector, handle) = AccessListCollector::new();
        let (trace_insp, _trace) = crate::sim::evm::CallTraceInspector::new();
        let (swap_insp, _swaps) = crate::sim::evm::SwapEventCaptureInspector::new();
        let tx = TxEnv::builder()
            .caller(CALLER)
            .kind(TxKind::Call(CONTRACT))
            .value(U256::ZERO)
            .gas_limit(GAS_LIMIT)
            .nonce(0)
            .build()
            .expect("well-formed tx");
        let result = evm
            .inspect_one(tx, (collector, (trace_insp, swap_insp)))
            .expect("revm inspect");
        assert!(result.is_success());
        handle.take_access_list()
    };

    // evm2 side — one transact drives BOTH the collector and the account
    // changes (the D3 comparison needs the same run).
    let (evm2_al, journal_set) = {
        use super::inspectors::Evm2SimInspector;
        use ::evm2::ethereum::TxEnvelope;
        use alloy::consensus::transaction::Recovered;
        use alloy::consensus::TxLegacy;

        let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(Evm2CodeMap(codes.clone())));
        let mut evm = build_pilot_evm(db);
        evm.set_inspector(Evm2SimInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(CONTRACT),
                gas_limit: GAS_LIMIT,
                nonce: 0,
                ..Default::default()
            }),
            CALLER,
        );
        let executed = evm.transact(&tx).expect("evm2 transact");
        let journal: BTreeSet<(Address, B256)> = executed
            .account_changes()
            .flat_map(|changes| {
                let address = changes.address;
                changes
                    .storage
                    .slots
                    .keys()
                    .map(move |k| (address, B256::from(*k)))
            })
            .collect();
        let _ = executed.discard();
        let al = evm
            .inspector_mut()
            .and_then(|i| i.downcast_mut::<Evm2SimInspector>())
            .expect("composite inspector set")
            .take_access_list();
        (al, journal)
    };

    assert_eq!(
        al_set(revm_al),
        want,
        "revm collector diverges from the fixture"
    );
    assert_eq!(
        al_set(evm2_al),
        want,
        "evm2 collector diverges from the fixture"
    );
    assert_eq!(
        journal_set, want,
        "evm2 account-changes journal diverges (D3)"
    );
}

// ── Phase 3: production stack + frame replay ──

use crate::sim::evm::frame_replay::{FrameRpcCounter, ReplayableTx, ScratchBlock};

const REPLAY_BLOCK: ScratchBlock = ScratchBlock {
    number: BLOCK,
    timestamp: 1_700_000_000,
    base_fee_next: 0,
};

/// The replayable store-frame: sender `CALLER` stores `value` at slot 5 via
/// the pilot contract, with EIP-1559 fees over the zero projected base fee.
fn replay_tx(nonce: u64, value: u64) -> ReplayableTx {
    ReplayableTx {
        from: CALLER,
        to: Some(CONTRACT),
        value: U256::ZERO,
        data: word(value),
        gas_limit: GAS_LIMIT,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 1_000_000_000,
        nonce,
    }
}

/// A single frame replay settles identically across engines — verdict,
/// touched set, storage writes, cold-read count, and base-fee source.
#[test]
fn frame_replay_parity_over_the_scenario_corpus() {
    use crate::sim::evm::frame_replay::{CountingFrameDb, ScratchEvm};

    let codes = CodeMap(vec![(CONTRACT, store_code().to_vec())]);
    let tx = replay_tx(0, VALUE);

    // revm side — CacheDB over the counting bridge over the scripted map.
    let revm_outcome = {
        let counter = Arc::new(FrameRpcCounter::default());
        let db = revm::database::CacheDB::new(CountingFrameDb::new(
            RevmCodeMap(&codes),
            Arc::clone(&counter),
        ));
        let mut scratch = ScratchEvm::with_counter(db, REPLAY_BLOCK, counter);
        scratch.replay(&tx).expect("revm replay")
    };

    // evm2 side — the same stack shape.
    let evm2_outcome = {
        use super::frame_replay::{Evm2CountingFrameDb, Evm2ScratchEvm};

        let counter = Arc::new(FrameRpcCounter::default());
        let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(Evm2CountingFrameDb::new(
            Evm2CodeMap(codes.clone()),
            Arc::clone(&counter),
        )));
        let mut scratch = Evm2ScratchEvm::with_counter(db, REPLAY_BLOCK, counter);
        scratch.replay(&tx).expect("evm2 replay")
    };

    assert_eq!(revm_outcome.status, evm2_outcome.status);
    assert_eq!(
        revm_outcome.touched, evm2_outcome.touched,
        "touched sets diverge"
    );
    // The writes: revm's settled state map vs evm2's captured changes.
    let mut revm_writes: Vec<(Address, U256, U256)> = revm_outcome
        .state
        .iter()
        .flat_map(|(address, account)| {
            account
                .storage
                .iter()
                .map(move |(k, s)| (*address, *k, s.present_value()))
        })
        .collect();
    revm_writes.sort();
    assert_eq!(revm_writes, evm2_outcome.storage_writes, "writes diverge");
    assert_eq!(
        revm_outcome.rpc_reads, evm2_outcome.rpc_reads,
        "cold-read counts diverge"
    );
    assert_eq!(revm_outcome.base_fee_source, evm2_outcome.base_fee_source);
    assert!(
        revm_outcome.rpc_reads > 0,
        "sanity: the frame did forward cold reads"
    );
}

/// Sequence replay: two committed predecessors then the frame — identical
/// dispositions, gas, frame outcome, and accumulated writes across engines.
#[test]
fn replay_sequence_commits_predecessors_identically() {
    use crate::sim::evm::frame_replay::{CountingFrameDb, ScratchEvm};

    let codes = CodeMap(vec![(CONTRACT, store_code().to_vec())]);
    let prefix = [replay_tx(0, 10), replay_tx(1, 20)];
    let frame = replay_tx(2, 30);

    let revm_outcome = {
        let counter = Arc::new(FrameRpcCounter::default());
        let db = revm::database::CacheDB::new(CountingFrameDb::new(
            RevmCodeMap(&codes),
            Arc::clone(&counter),
        ));
        let mut scratch = ScratchEvm::with_counter(db, REPLAY_BLOCK, counter);
        scratch
            .replay_sequence(&prefix, &frame)
            .expect("revm sequence")
    };

    let evm2_outcome = {
        use super::frame_replay::{Evm2CountingFrameDb, Evm2ScratchEvm};

        let counter = Arc::new(FrameRpcCounter::default());
        let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(Evm2CountingFrameDb::new(
            Evm2CodeMap(codes.clone()),
            Arc::clone(&counter),
        )));
        let mut scratch = Evm2ScratchEvm::with_counter(db, REPLAY_BLOCK, counter);
        scratch
            .replay_sequence(&prefix, &frame)
            .expect("evm2 sequence")
    };

    assert_eq!(revm_outcome.predecessors, evm2_outcome.predecessors);
    assert_eq!(
        revm_outcome.predecessor_gas, evm2_outcome.predecessor_gas,
        "predecessor gas diverges"
    );
    assert_eq!(revm_outcome.frame.status, evm2_outcome.frame.status);
    assert_eq!(
        revm_outcome.frame.touched, evm2_outcome.frame.touched,
        "frame touched sets diverge"
    );
    assert_eq!(
        revm_outcome.frame.rpc_reads, evm2_outcome.frame.rpc_reads,
        "sequence cold-read counts diverge"
    );
    // The frame's write is the same slot 5 overwrite on both engines.
    let want_write = (CONTRACT, U256::from(SLOT), U256::from(30u64));
    assert!(
        evm2_outcome.frame.storage_writes.contains(&want_write),
        "evm2 frame write missing: {:?}",
        evm2_outcome.frame.storage_writes
    );
}

/// The per-block storage memo serves a repeated read from its own first
/// answer — one fallback fetch, one hit — identically through both ports.
#[test]
fn storage_memo_serves_the_same_block_read_on_both_engines() {
    use ::evm2::evm::Database;
    use revm::database_interface::DatabaseRef;

    let fixture = Fixture {
        contract_code: store_code(),
    };

    let memo_revm = Arc::new(crate::sim::evm::StorageMemo::new());
    let revm_db = crate::sim::evm::BotStateDb::new(
        &degenbot_substrate::NO_SIM_ANCHOR,
        RevmScripted {
            fixture: &fixture,
            contract_reads: Rc::new(Cell::new(0)),
        },
    )
    .with_storage_memo(Arc::clone(&memo_revm));

    let memo_evm2 = Arc::new(crate::sim::evm::StorageMemo::new());
    let mut evm2_db = BotStateDb::new(
        &degenbot_substrate::NO_SIM_ANCHOR,
        Evm2Scripted {
            fixture: &fixture,
            contract_reads: Rc::new(Cell::new(0)),
        },
    )
    .with_storage_memo(Arc::clone(&memo_evm2));

    let slot = U256::from(7u64);
    let first_revm = revm_db
        .storage_ref(CONTRACT, slot)
        .expect("revm first read");
    let second_revm = revm_db
        .storage_ref(CONTRACT, slot)
        .expect("revm second read");
    let first_evm2 = evm2_db
        .get_storage(&CONTRACT, &slot)
        .expect("evm2 first read");
    let second_evm2 = evm2_db
        .get_storage(&CONTRACT, &slot)
        .expect("evm2 second read");

    assert_eq!((first_revm, second_revm), (first_evm2, second_evm2));
    assert_eq!(memo_revm.stats(), (1, 1), "revm memo: one hit, one miss");
    assert_eq!(memo_evm2.stats(), (1, 1), "evm2 memo: one hit, one miss");
}

// ── Phase 4: the settlement bundle on a journaled sequence ──

/// The dispatching accumulate/copy contract: calldata word 0xffff copies
/// slot 5 to slot 6; any other word accumulates it into slot 5. The copy
/// steps prove mid-sequence visibility of accumulated writes.
fn accumulate_code() -> Vec<u8> {
    vec![
        0x60, 0x00, 0x35, // CALLDATALOAD(0)          [in]
        0x80, // DUP1                                 [in, in]
        0x61, 0xff, 0xff, // PUSH2 0xffff            [in, in, 0xffff]
        0x14, // EQ                                   [in, eq]
        0x60, 0x13, // PUSH1 0x13 (copy dest)
        0x57, // JUMPI
        // accumulate:                               [in]
        0x60, 0x05, 0x54, // SLOAD 5                 [in, s5]
        0x01, // ADD                                  [sum]
        0x60, 0x05, 0x55, // SSTORE 5
        0x00, // STOP
        0x5b, // JUMPDEST (copy)                     [in]
        0x50, // POP
        0x60, 0x05, 0x54, // SLOAD 5
        0x60, 0x06, 0x55, // SSTORE 6
        0x00, // STOP
    ]
}

/// The 7-call settlement-bundle shape — journaled accumulation across
/// read/execute/read steps — settles identically on both engines: per-step
/// verdicts + gas, the accumulated writes, and post-sequence invisibility.
#[test]
fn settlement_bundle_sequence_matches_across_engines() {
    use crate::sim::evm::{SimInspector, SimSession, SimStep};
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    use revm::context::TxEnv;
    use revm::{MainBuilder, MainContext};

    const COPY: u64 = 0xffff;
    let codes = CodeMap(vec![(CONTRACT, accumulate_code())]);
    // The 7-step vector: accumulate ×3, copy, accumulate ×2, copy.
    let inputs = [1u64, 2, 3, COPY, 4, 5, COPY];

    // revm side — the production sequence verb.
    let (revm_calls, revm_writes, revm_after) = {
        let db = revm::database::CacheDB::new(RevmCodeMap(&codes));
        let mut ctx = revm::context::Context::mainnet().with_db(db);
        ctx.cfg
            .set_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::CANCUN);
        let mut evm = ctx.build_mainnet_with_inspector(SimInspector::default());
        let mut session = SimSession::new(&mut evm);
        let steps: Vec<SimStep<_>> = inputs
            .iter()
            .enumerate()
            .map(|(nonce, &input)| {
                SimStep::Transact(
                    TxEnv::builder()
                        .caller(CALLER)
                        .kind(TxKind::Call(CONTRACT))
                        .data(word(input))
                        .value(U256::ZERO)
                        .gas_limit(GAS_LIMIT)
                        .nonce(nonce as u64)
                        .build()
                        .expect("well-formed step"),
                )
            })
            .collect();
        let seq = session.run_sequence(steps).expect("revm sequence");
        let mut writes: Vec<(Address, U256, U256)> = seq
            .state
            .get(&CONTRACT)
            .map(|account| {
                account
                    .storage
                    .iter()
                    .map(|(k, s)| (CONTRACT, *k, s.present_value()))
                    .collect()
            })
            .unwrap_or_default();
        writes.sort();
        let after = session
            .read_word(CONTRACT, U256::from(SLOT))
            .expect("revm view read");
        (
            seq.calls
                .iter()
                .map(|c| {
                    (
                        c.is_success(),
                        c.tx_gas_used(),
                        c.output().cloned().unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>(),
            writes,
            after,
        )
    };

    // evm2 side — the checkpoint-scoped sequence verb.
    let (evm2_calls, evm2_writes, evm2_after) = {
        let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(Evm2CodeMap(codes.clone())));
        let mut evm = build_pilot_evm(db);
        let mut session = super::session::Evm2SimSession::new(&mut evm);
        let steps: Vec<_> = inputs
            .iter()
            .enumerate()
            .map(|(nonce, &input)| {
                super::session::Evm2SimStep::Transact(Recovered::new_unchecked(
                    TxEnvelope::Legacy(TxLegacy {
                        to: TxKind::Call(CONTRACT),
                        input: word(input),
                        gas_limit: GAS_LIMIT,
                        nonce: nonce as u64,
                        ..Default::default()
                    }),
                    CALLER,
                ))
            })
            .collect();
        let seq = session.run_sequence(&steps).expect("evm2 sequence");
        let after = session
            .read_word(CONTRACT, U256::from(SLOT))
            .expect("evm2 view read");
        (
            seq.calls
                .iter()
                .map(|c| {
                    (
                        c.verdict == crate::sim::evm::RunVerdict::Success,
                        c.gas_used,
                        c.output.clone(),
                    )
                })
                .collect::<Vec<_>>(),
            seq.storage_writes,
            after,
        )
    };

    assert_eq!(revm_calls.len(), 7, "revm sanity");
    assert_eq!(
        revm_calls, evm2_calls,
        "per-step verdicts/gas/output diverge"
    );
    assert_eq!(revm_writes, evm2_writes, "accumulated writes diverge");
    // The accumulated final state: slot 5 = 15, slot 6 = 15 (the copy ran
    // after the last accumulate — mid-sequence visibility proven).
    assert_eq!(
        evm2_writes,
        [
            (CONTRACT, U256::from(5u64), U256::from(15u64)),
            (CONTRACT, U256::from(6u64), U256::from(15u64))
        ],
        "accumulated state sanity"
    );
    // Post-sequence invisibility: the shared view carries nothing.
    assert_eq!((revm_after, evm2_after), (U256::ZERO, U256::ZERO));
}

/// The per-block production handles: both build over the same scripted
/// provider, apply the same overrides, and settle the same frame — and
/// their scratch engines replay the same foreign frame with the same
/// cold-read count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_handles_settle_and_replay_identically() {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    use revm::context::TxEnv;

    let provider = scripted_transport::provider(Fixture {
        contract_code: store_code(),
    });
    let params = override_params();
    let warm_revm = crate::sim::evm::WarmCodeCacheInner::shared_default();
    let warm_evm2 = WarmCodeCacheInner::shared_default();

    let mut revm_handle = crate::sim::evm::BlockSimHandle::build(
        &provider,
        0,
        BLOCK,
        1_700_000_000,
        &params,
        &degenbot_substrate::NO_SIM_ANCHOR,
        &warm_revm,
        None,
        false,
    )
    .expect("revm handle builds over the scripted provider");
    let mut evm2_handle = super::handle::Evm2BlockSimHandle::build(
        &provider,
        0,
        BLOCK,
        1_700_000_000,
        &params,
        &degenbot_substrate::NO_SIM_ANCHOR,
        &warm_evm2,
        None,
        false,
    )
    .expect("evm2 handle builds over the scripted provider");

    // The settle verb through both sessions.
    let (revm_verdict, revm_gas, revm_output) = {
        let mut session = revm_handle.session();
        let tx = TxEnv::builder()
            .caller(CALLER)
            .kind(TxKind::Call(CONTRACT))
            .data(word(VALUE))
            .value(U256::ZERO)
            .gas_limit(GAS_LIMIT)
            .nonce(0)
            .build()
            .expect("well-formed tx");
        let run = session.run(tx).expect("revm session run");
        (run.verdict, run.gas_used, run.output)
    };
    let (evm2_verdict, evm2_gas, evm2_output) = {
        let mut session = evm2_handle.session();
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(CONTRACT),
                input: word(VALUE),
                gas_limit: GAS_LIMIT,
                nonce: 0,
                ..Default::default()
            }),
            CALLER,
        );
        let run = session.run(&tx).expect("evm2 session run");
        (run.verdict, run.gas_used, run.output)
    };
    assert_eq!(
        (revm_verdict, revm_gas, revm_output),
        (evm2_verdict, evm2_gas, evm2_output),
        "handle session verdicts diverge"
    );

    // The scratch engines: same foreign frame, same cold-read count.
    let revm_replay = revm_handle
        .scratch_evm()
        .expect("revm scratch builds")
        .replay(&replay_tx(0, VALUE))
        .expect("revm scratch replay");
    let evm2_replay = evm2_handle
        .scratch_evm()
        .expect("evm2 scratch builds")
        .replay(&replay_tx(0, VALUE))
        .expect("evm2 scratch replay");
    assert_eq!(revm_replay.status, evm2_replay.status);
    assert_eq!(
        revm_replay.touched, evm2_replay.touched,
        "scratch touched sets diverge"
    );
    assert_eq!(
        revm_replay.rpc_reads, evm2_replay.rpc_reads,
        "scratch cold-read counts diverge"
    );
    assert!(
        evm2_replay.rpc_reads > 0,
        "sanity: the scratch forwarded cold reads"
    );
}

/// Inspector-driven capture — the engine's swap-event oracle's seam — is
/// identical across engines: same log, same verdict.
#[test]
fn inspector_log_capture_matches_revm() {
    let (revm_verdict, revm_logs) = run_revm_inspected(log1_code());
    let (evm2_verdict, evm2_logs) = run_evm2_inspected(log1_code());
    assert_eq!(revm_verdict, evm2_verdict, "inspected-run verdicts diverge");
    assert_eq!(revm_logs, evm2_logs, "captured logs diverge");
    assert_eq!(evm2_logs.len(), 1, "capture sanity");
    assert_eq!(evm2_logs[0].address, CONTRACT);
    assert_eq!(evm2_logs[0].topics, vec![B256::with_last_byte(0xaa)]);
}

use super::bot_state_db::BotStateDb;
use super::build_pilot_evm;
use super::warm_code_cache::{WarmCodeCache, WarmCodeCacheInner};
// ── The async RPC cold-miss fallback (AlloyAsyncDb) ──────────────────────

/// A scripted alloy `Transport` serving the four RPCs [`AlloyAsyncDb`]
/// consumes, from the same [`Fixture`] the in-memory stacks serve — the
/// offline stand-in for the live node.
mod scripted_transport {
    use super::*;
    use alloy::providers::RootProvider;
    use alloy::rpc::client::RpcClient;
    use alloy::rpc::json_rpc::{
        ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
    };
    use alloy::transports::{TransportError, TransportFut};
    use degenbot_rpc::provider::AlloyProvider;
    use serde_json::value::{to_raw_value, RawValue};
    use std::sync::Arc;

    /// Build an `AlloyProvider` whose transport serves `fixture` (no network).
    pub fn provider(fixture: Fixture) -> AlloyProvider {
        let client = RpcClient::new(
            ScriptedTransport {
                fixture: Arc::new(fixture),
            },
            true,
        );
        let root: RootProvider = RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    #[derive(Clone)]
    struct ScriptedTransport {
        fixture: Arc<Fixture>,
    }

    impl ScriptedTransport {
        fn handle(self, packet: RequestPacket) -> ResponsePacket {
            match packet {
                RequestPacket::Single(req) => ResponsePacket::Single(self.respond(&req)),
                RequestPacket::Batch(reqs) => ResponsePacket::Batch(
                    reqs.iter().map(|req| self.clone().respond(req)).collect(),
                ),
            }
        }

        fn respond(self, req: &SerializedRequest) -> Response {
            let id = req.id().clone();
            let params = req
                .params()
                .and_then(|rv| serde_json::from_str::<serde_json::Value>(rv.get()).ok())
                .unwrap_or(serde_json::Value::Null);
            let payload = match req.method() {
                "eth_getBalance" => self.balance(&params),
                "eth_getTransactionCount" => self.nonce(&params),
                "eth_getCode" => self.code(&params),
                "eth_getStorageAt" => self.storage(),
                _ => Err(ErrorPayload {
                    code: -32601,
                    message: format!("method not found: {}", req.method()).into(),
                    data: None,
                }),
            };
            Response {
                id,
                payload: payload.unwrap_or_else(ResponsePayload::Failure),
            }
        }

        fn address_param(params: &serde_json::Value) -> Option<Address> {
            params.as_array()?.first()?.as_str()?.parse().ok()
        }

        fn balance(&self, params: &serde_json::Value) -> Result<ResponsePayload, ErrorPayload> {
            let value = match Self::address_param(params) {
                Some(CALLER) => U256::from(FUND),
                _ => U256::ZERO,
            };
            success(&format!("{value:#x}"))
        }

        fn nonce(&self, params: &serde_json::Value) -> Result<ResponsePayload, ErrorPayload> {
            let value = match Self::address_param(params) {
                Some(CONTRACT) => 1u64,
                _ => 0,
            };
            success(&format!("0x{value:x}"))
        }

        fn code(&self, params: &serde_json::Value) -> Result<ResponsePayload, ErrorPayload> {
            let code = match Self::address_param(params) {
                Some(CONTRACT) => format!("0x{}", hex::encode(&self.fixture.contract_code)),
                _ => "0x".to_string(),
            };
            success(&code)
        }

        fn storage(&self) -> Result<ResponsePayload, ErrorPayload> {
            success(&format!("0x{:064x}", 0))
        }
    }

    fn success(value: &str) -> Result<ResponsePayload, ErrorPayload> {
        let raw: Box<RawValue> =
            to_raw_value(&serde_json::Value::String(value.to_string())).expect("valid JSON string");
        Ok(ResponsePayload::Success(raw))
    }

    impl tower::Service<RequestPacket> for ScriptedTransport {
        type Response = ResponsePacket;
        type Error = TransportError;
        type Future = TransportFut<'static>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: RequestPacket) -> Self::Future {
            let this = self.clone();
            Box::pin(async move { Ok(this.handle(req)) })
        }
    }
}

/// The async cold-miss path end-to-end: the same store transaction, with the
/// contract account served "over RPC" through `AlloyAsyncDb` + evm2's
/// coroutine entrypoint, must match the revm layered stack's verdict.
#[tokio::test(flavor = "current_thread")]
async fn async_rpc_cold_miss_matches_revm_layered() {
    use super::alloy_async_db::AlloyAsyncDb;
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;

    let fixture = Fixture {
        contract_code: store_code(),
    };
    let provider = scripted_transport::provider(Fixture {
        contract_code: fixture.contract_code.clone(),
    });
    let adapter = AlloyAsyncDb::new(&provider, BLOCK);
    let mut evm = build_pilot_evm(::evm2::evm::r#async::AsyncDb::new(adapter));

    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(CONTRACT),
            input: word(VALUE),
            gas_limit: GAS_LIMIT,
            nonce: 0,
            ..Default::default()
        }),
        CALLER,
    );
    let executed = evm.transact_async(&tx).await.expect("evm2 async transact");
    let slot_value = executed
        .account_changes()
        .find(|changes| changes.address == CONTRACT)
        .and_then(|changes| changes.storage.slots.get(&U256::from(SLOT)))
        .map(|slot| slot.value.current)
        .unwrap_or_default();
    let result = executed.discard();
    let evm2_verdict = Verdict {
        status: result.status,
        gas_used: result.tx_gas_used(),
        output: result.output,
        slot_value,
    };

    let (revm_verdict, _) = run_revm_layered(&fixture, RevmWarmCodeCacheInner::shared_default());
    assert_eq!(revm_verdict, evm2_verdict, "RPC-served async run diverged");
}

use crate::sim::evm::{
    BotStateDb as RevmBotStateDb, WarmCodeCache as RevmWarmCodeCache,
    WarmCodeCacheInner as RevmWarmCodeCacheInner,
};

const CALLER: Address = Address::with_last_byte(0xaa);
const CONTRACT: Address = Address::with_last_byte(0xcc);
const FUND: u64 = 1_000_000_000_000_000;
const GAS_LIMIT: u64 = 300_000;
const SLOT: u64 = 5;
const VALUE: u64 = 42;
const BLOCK: u64 = 100;

/// `PUSH0 CALLDATALOAD PUSH1 05 SSTORE STOP` — store the calldata word at
/// slot 5.
fn store_code() -> Bytes {
    Bytes::from(vec![0x5f, 0x35, 0x60, 0x05, 0x55, 0x00])
}

fn word(value: u64) -> Bytes {
    U256::from(value).to_be_bytes_vec().into()
}

/// The run verdict both engines are compared on (same shape as
/// `tests/evm2_pilot.rs`).
#[derive(Debug, PartialEq, Eq)]
struct Verdict {
    status: bool,
    gas_used: u64,
    output: Bytes,
    slot_value: U256,
}

/// The logical fixture both scripted fallbacks serve: the funded caller, the
/// deployed contract, an empty storage view.
struct Fixture {
    contract_code: Bytes,
}

// ── revm side ────────────────────────────────────────────────────────────

struct RevmScripted<'f> {
    fixture: &'f Fixture,
    contract_reads: Rc<Cell<u64>>,
}

impl revm::database_interface::DatabaseRef for RevmScripted<'_> {
    type Error = core::convert::Infallible;

    fn basic_ref(&self, address: Address) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        if address == CALLER {
            return Ok(Some(revm::state::AccountInfo {
                balance: U256::from(FUND),
                ..Default::default()
            }));
        }
        if address == CONTRACT {
            self.contract_reads.set(self.contract_reads.get() + 1);
            let code = revm::bytecode::Bytecode::new_legacy(self.fixture.contract_code.clone());
            return Ok(Some(revm::state::AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash: code.hash_slow(),
                code: Some(code),
                ..Default::default()
            }));
        }
        Ok(None)
    }

    fn code_by_hash_ref(&self, _code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        Ok(revm::bytecode::Bytecode::default())
    }

    fn storage_ref(&self, _address: Address, _index: U256) -> Result<U256, Self::Error> {
        Ok(U256::ZERO)
    }

    fn block_hash_ref(&self, _number: u64) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

/// One run through the revm production wrapper stack over the scripted
/// fallback. Returns the verdict + the fallback's CONTRACT read count.
fn run_revm_layered(
    fixture: &Fixture,
    inner: std::sync::Arc<parking_lot::RwLock<RevmWarmCodeCacheInner>>,
) -> (Verdict, u64) {
    use revm::context::TxEnv;
    use revm::context_interface::result::{ExecutionResult, Output};
    use revm::{ExecuteEvm, MainBuilder, MainContext};

    let contract_reads = Rc::new(Cell::new(0));
    let scripted = RevmScripted {
        fixture,
        contract_reads: contract_reads.clone(),
    };
    let bsd = RevmBotStateDb::new(&degenbot_substrate::NO_SIM_ANCHOR, scripted);
    let warm = RevmWarmCodeCache::with_owner(inner, BLOCK, bsd);
    let db = revm::database::CacheDB::new(warm);

    let mut ctx = revm::context::Context::mainnet().with_db(db);
    // The revm-side pin of `PILOT_SPEC` (the setter recomputes the cached gas
    // table — assigning `cfg.spec` directly leaves the default spec's
    // `gas_params` active).
    ctx.cfg
        .set_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::CANCUN);
    let mut evm = ctx.build_mainnet();

    let tx = TxEnv::builder()
        .caller(CALLER)
        .kind(TxKind::Call(CONTRACT))
        .data(word(VALUE))
        .value(U256::ZERO)
        .gas_limit(GAS_LIMIT)
        .nonce(0)
        .build()
        .expect("well-formed pilot tx");
    let settled = evm.transact(tx).expect("revm transact");

    let gas_used = settled.result.tx_gas_used();
    let verdict = match settled.result {
        ExecutionResult::Success { output, .. } => {
            let output = match output {
                Output::Call(bytes) => bytes,
                Output::Create(..) => Bytes::new(),
            };
            let slot_value = settled
                .state
                .get(&CONTRACT)
                .and_then(|account| account.storage.get(&U256::from(SLOT)))
                .map(|slot| slot.present_value())
                .unwrap_or_default();
            Verdict {
                status: true,
                gas_used,
                output,
                slot_value,
            }
        }
        other => panic!("revm run did not succeed: {other:?}"),
    };
    (verdict, contract_reads.get())
}

// ── evm2 side ────────────────────────────────────────────────────────────

struct Evm2Scripted<'f> {
    fixture: &'f Fixture,
    contract_reads: Rc<Cell<u64>>,
}

impl ::evm2::evm::Database for Evm2Scripted<'_> {
    type Error = core::convert::Infallible;

    fn get_account(
        &mut self,
        address: &Address,
    ) -> Result<Option<::evm2::evm::AccountInfo>, Self::Error> {
        if *address == CALLER {
            return Ok(Some(
                ::evm2::evm::AccountInfo::default().with_balance(U256::from(FUND)),
            ));
        }
        if *address == CONTRACT {
            self.contract_reads.set(self.contract_reads.get() + 1);
            return Ok(Some(
                ::evm2::evm::AccountInfo::default().with_nonce(1).with_code(
                    ::evm2::bytecode::Bytecode::new_legacy(self.fixture.contract_code.clone()),
                ),
            ));
        }
        Ok(None)
    }

    fn get_code_by_hash(
        &mut self,
        _code_hash: &B256,
    ) -> Result<::evm2::bytecode::Bytecode, Self::Error> {
        Ok(::evm2::bytecode::Bytecode::default())
    }

    fn get_storage(
        &mut self,
        _address: &Address,
        _key: &::evm2::interpreter::Word,
    ) -> Result<::evm2::interpreter::Word, Self::Error> {
        Ok(::evm2::interpreter::Word::ZERO)
    }

    fn get_block_hash(&mut self, _number: &::evm2::interpreter::Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

/// The same run through the evm2 pilot wrapper stack — `CacheDB` over the
/// warm cache over `BotStateDb` over the scripted fallback.
fn run_evm2_layered(
    fixture: &Fixture,
    inner: std::sync::Arc<parking_lot::RwLock<WarmCodeCacheInner>>,
) -> (Verdict, u64) {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;

    let contract_reads = Rc::new(Cell::new(0));
    let scripted = Evm2Scripted {
        fixture,
        contract_reads: contract_reads.clone(),
    };
    let bsd = BotStateDb::new(&degenbot_substrate::NO_SIM_ANCHOR, scripted);
    let warm = WarmCodeCache::with_owner(inner, BLOCK, bsd);
    let db = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(warm));
    let mut evm = build_pilot_evm(db);

    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(CONTRACT),
            input: word(VALUE),
            gas_limit: GAS_LIMIT,
            nonce: 0,
            ..Default::default()
        }),
        CALLER,
    );
    let executed = evm.transact(&tx).expect("evm2 transact");
    let slot_value = executed
        .account_changes()
        .find(|changes| changes.address == CONTRACT)
        .and_then(|changes| changes.storage.slots.get(&U256::from(SLOT)))
        .map(|slot| slot.value.current)
        .unwrap_or_default();
    let result = executed.discard();
    let verdict = Verdict {
        status: result.status,
        gas_used: result.tx_gas_used(),
        output: result.output,
        slot_value,
    };
    (verdict, contract_reads.get())
}

/// The layered stacks produce identical verdicts, and the second run at the
/// same block serves the contract account from the warm cache on both
/// engines (zero fallback reads).
#[test]
fn layered_stack_parity_and_warm_cache_hits() {
    let fixture = Fixture {
        contract_code: store_code(),
    };
    let revm_inner = RevmWarmCodeCacheInner::shared_default();
    let evm2_inner = WarmCodeCacheInner::shared_default();

    let (revm_first, revm_cold) = run_revm_layered(&fixture, revm_inner.clone());
    let (evm2_first, evm2_cold) = run_evm2_layered(&fixture, evm2_inner.clone());
    assert_eq!(
        revm_first.slot_value,
        U256::from(VALUE),
        "revm engine sanity"
    );
    assert_eq!(revm_first, evm2_first, "cold-run verdicts diverge");
    assert_eq!(
        (revm_cold, evm2_cold),
        (1, 1),
        "one cold CONTRACT read each"
    );

    let (revm_second, revm_warm) = run_revm_layered(&fixture, revm_inner);
    let (evm2_second, evm2_warm) = run_evm2_layered(&fixture, evm2_inner);
    assert_eq!(
        revm_first, revm_second,
        "the warm cache changed revm execution"
    );
    assert_eq!(revm_second, evm2_second, "warm-run verdicts diverge");
    assert_eq!(
        (revm_warm, evm2_warm),
        (0, 0),
        "CONTRACT served from the warm cache"
    );
}
