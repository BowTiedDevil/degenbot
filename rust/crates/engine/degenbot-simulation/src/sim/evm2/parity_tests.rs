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
