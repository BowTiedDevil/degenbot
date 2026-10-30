//! Differential parity gate for the evm2 pilot: the same transaction
//! through the revm engine and the evm2 engine at the same spec must produce
//! identical status, gas used, output, and storage writes. Every later
//! migration stage (layered DB stack, async fallback, inspectors) must keep
//! this gate green.

use alloy::primitives::{Address, Bytes, TxKind, U256};
use degenbot_simulation::sim::evm2 as pilot;

const CALLER: Address = Address::with_last_byte(0xaa);
const CONTRACT: Address = Address::with_last_byte(0xcc);
const FUND: u64 = 1_000_000_000_000_000;
const GAS_LIMIT: u64 = 300_000;
const SLOT: u64 = 5;
const VALUE: u64 = 42;

/// `PUSH0 CALLDATALOAD PUSH1 05 SSTORE STOP` — store the calldata word at
/// slot 5.
fn store_code() -> Bytes {
    Bytes::from(vec![0x5f, 0x35, 0x60, 0x05, 0x55, 0x00])
}

/// `PUSH0 CALLDATALOAD PUSH0 MSTORE PUSH1 32 PUSH0 RETURN` — echo the
/// calldata word back as 32-byte return data.
fn return_code() -> Bytes {
    Bytes::from(vec![0x5f, 0x35, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3])
}

fn word(value: u64) -> Bytes {
    U256::from(value).to_be_bytes_vec().into()
}

/// The run verdict both engines are compared on.
#[derive(Debug, PartialEq, Eq)]
struct Verdict {
    status: bool,
    gas_used: u64,
    output: Bytes,
    slot_value: U256,
}

/// Run one call against `code` deployed at `CONTRACT` through the revm
/// engine at [`pilot::PILOT_SPEC`], reading the storage write from the
/// settled (uncommitted) result state — the revm engine's
/// finalize-never-commit discipline.
fn run_revm(code: Bytes, input: Bytes) -> Verdict {
    use revm::bytecode::Bytecode;
    use revm::context::TxEnv;
    use revm::context_interface::result::{ExecutionResult, Output};
    use revm::database::CacheDB;
    use revm::database_interface::EmptyDB;
    use revm::state::AccountInfo;
    use revm::{ExecuteEvm, MainBuilder, MainContext};

    let mut db = CacheDB::new(EmptyDB::new());
    db.insert_account_info(
        CALLER,
        AccountInfo {
            balance: U256::from(FUND),
            ..Default::default()
        },
    );
    let bytecode = Bytecode::new_legacy(code);
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

    let mut ctx = revm::context::Context::mainnet().with_db(db);
    // The revm-side pin of `pilot::PILOT_SPEC` — the two engines carry their
    // own spec enums, so the parity gate keeps the pairing explicit. The
    // setter recomputes the cached gas table: assigning `cfg.spec` directly
    // leaves the default spec's `gas_params` active.
    ctx.cfg
        .set_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::CANCUN);
    let mut evm = ctx.build_mainnet();

    let tx = TxEnv::builder()
        .caller(CALLER)
        .kind(TxKind::Call(CONTRACT))
        .data(input)
        .value(U256::ZERO)
        .gas_limit(GAS_LIMIT)
        .nonce(0)
        .build()
        .expect("well-formed pilot tx");
    let settled = evm.transact(tx).expect("revm transact");

    let gas_used = settled.result.tx_gas_used();
    match settled.result {
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
    }
}

/// The same run through the evm2 pilot engine. The storage write is read
/// from the transaction's account changes and the scratch is then `discard`ed
/// — the finalize-never-commit equivalent of the revm side reading the
/// settled, uncommitted result state.
fn run_evm2(code: Bytes, input: Bytes) -> Verdict {
    use ::evm2::bytecode::Bytecode as Evm2Bytecode;
    use ::evm2::ethereum::TxEnvelope;
    use ::evm2::evm::AccountInfo as Evm2AccountInfo;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;

    let mut db = pilot::pilot_db();
    db.insert_account_info(
        &CALLER,
        Evm2AccountInfo::default().with_balance(U256::from(FUND)),
    );
    db.insert_account_info(
        &CONTRACT,
        Evm2AccountInfo::default()
            .with_nonce(1)
            .with_code(Evm2Bytecode::new_legacy(code)),
    );

    let mut evm = pilot::build_pilot_evm(db);
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
    let executed = evm.transact(&tx).expect("evm2 transact");
    let slot_value = executed
        .account_changes()
        .find(|changes| changes.address == CONTRACT)
        .and_then(|changes| changes.storage.slots.get(&U256::from(SLOT)))
        .map(|slot| slot.value.current)
        .unwrap_or_default();
    let result = executed.discard();
    Verdict {
        status: result.status,
        gas_used: result.tx_gas_used(),
        output: result.output,
        slot_value,
    }
}

// ── Live fork-truth legs (the `just test-fork` lane; reth archive node) ──

/// WETH9 on mainnet.
const WETH: Address = alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
/// A large WETH holder (Binance hot wallet) — the caller + balanceOf subject.
const HOLDER: Address = alloy::primitives::address!("28C6c06298d514Db089934071355E5743bf21d60");
/// A Cancun-era mainnet block the archive node serves (pre-Pectra).
const LIVE_BLOCK: u64 = 22_000_000;

/// `balanceOf(holder)` calldata.
fn balance_of_calldata(holder: Address) -> Bytes {
    let mut data = hex::decode("70a08231").unwrap();
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(holder.as_slice());
    Bytes::from(data)
}

/// The test-local `Arc<dyn Provider>` → `Provider` bridge (the engine's
/// `ArcDynProviderEthereum` newtype is module-private; this mirrors it).
struct TypedProvider(std::sync::Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>);

impl alloy::providers::Provider<alloy::network::Ethereum> for TypedProvider {
    fn root(&self) -> &alloy::providers::RootProvider<alloy::network::Ethereum> {
        self.0.root()
    }
}

/// A live `balanceOf` read at a pinned mainnet block through BOTH engines'
/// cold-miss RPC paths — revm's `WrapDatabaseAsync<AlloyDB>` and evm2's
/// `AlloyAsyncDb` coroutine seam — must return the same word at the same gas.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_evm2_balance_of_matches_revm_fork_truth() {
    use alloy::eips::BlockId;
    use degenbot_rpc::provider::AlloyProvider;

    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let provider = AlloyProvider::new(&rpc_url, 3)
        .await
        .expect("provider builds");
    let calldata = balance_of_calldata(HOLDER);
    let nonce = provider
        .get_transaction_count(&HOLDER, Some(LIVE_BLOCK))
        .await
        .expect("nonce read");

    // revm side — the production cold-miss shape.
    let revm_verdict = {
        use revm::context::TxEnv;
        use revm::context_interface::result::{ExecutionResult, Output};
        use revm::{ExecuteEvm, MainBuilder, MainContext};

        let alloy_db = revm::database::AlloyDB::new(
            TypedProvider(provider.provider_arc()),
            BlockId::Number(LIVE_BLOCK.into()),
        );
        let wrap =
            revm::database_interface::WrapDatabaseAsync::new(alloy_db).expect("ambient runtime");
        let db = revm::database::CacheDB::new(wrap);
        let mut ctx = revm::context::Context::mainnet().with_db(db);
        ctx.cfg
            .set_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::CANCUN);
        let mut evm = ctx.build_mainnet();
        let tx = TxEnv::builder()
            .caller(HOLDER)
            .kind(TxKind::Call(WETH))
            .data(calldata.clone())
            .value(U256::ZERO)
            .gas_limit(100_000)
            .nonce(nonce)
            .build()
            .expect("well-formed live tx");
        let settled = evm.transact(tx).expect("revm live transact");
        let gas_used = settled.result.tx_gas_used();
        match settled.result {
            ExecutionResult::Success { output, .. } => {
                let Output::Call(bytes) = output else {
                    panic!("call output")
                };
                (bytes, gas_used)
            }
            other => panic!("revm live run failed: {other:?}"),
        }
    };

    // evm2 side — the async coroutine seam over the same provider + block.
    let evm2_verdict = {
        use ::evm2::ethereum::TxEnvelope;
        use alloy::consensus::transaction::Recovered;
        use alloy::consensus::TxLegacy;
        use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;

        let adapter = AlloyAsyncDb::new(&provider, LIVE_BLOCK);
        let mut evm = pilot::build_pilot_evm(::evm2::evm::r#async::AsyncDb::new(adapter));
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(WETH),
                input: calldata,
                gas_limit: 100_000,
                nonce,
                ..Default::default()
            }),
            HOLDER,
        );
        let executed = evm.transact_async(&tx).await.expect("evm2 live transact");
        let result = executed.discard();
        let gas_used = result.tx_gas_used();
        assert!(result.status, "evm2 live run failed: {result:?}");
        (result.output, gas_used)
    };

    assert_eq!(revm_verdict, evm2_verdict, "live fork-truth diverged");
    assert_eq!(evm2_verdict.0.len(), 32, "a balanceOf word came back");
}

#[test]
fn evm2_storage_write_matches_revm() {
    let revm = run_revm(store_code(), word(VALUE));
    let evm2 = run_evm2(store_code(), word(VALUE));
    assert_eq!(revm.slot_value, U256::from(VALUE), "revm engine sanity");
    assert_eq!(revm, evm2);
}

#[test]
fn evm2_return_data_matches_revm() {
    let revm = run_revm(return_code(), word(VALUE));
    let evm2 = run_evm2(return_code(), word(VALUE));
    assert_eq!(revm.output, word(VALUE), "revm engine sanity");
    assert_eq!(revm, evm2);
}
