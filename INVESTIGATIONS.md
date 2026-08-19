# README — Revm Investigation-Harness Architecture

How to assemble an in-process EVM investigation harness (deploy a contract,
drive a call, read the result) against a **real contract**, without re-deriving
the revm plumbing every time.

The tl;dr: the only part that was ever *hard* — deploying pinned bytecode into a
revm `CacheDB`, seeding storage, driving a call, and telling a Solidity `Revert`
apart from a verbless `Halt` — is now a single deep, contract-agnostic module in
the simulation crate. Everything else is a thin per-contract driver you scaffold
at the user layer.

---

## Layering

```
┌─────────────────────────────────────────────────────────────────────────┐
│  USER LAYER — your contract, your investigation                          │
│  · scripts/scaffold_revm_harness.py  →  investigations/<name>/  (standalone)│
│  · degenbot::investigation           →  degenbot's OWN path-fixture tool │
└───────────────────────────────┬─────────────────────────────────────────┘
                                │ builds on (thin, per-contract)
┌───────────────────────────────▼─────────────────────────────────────────┐
│  DEEP REUSABLE — degenbot_simulation::oracle                           │
│  deploy → seed slots → drive call → classify (Revert vs Halt) → read    │
│  Contract-agnostic. No pools, no executor, no backrun.                  │
└───────────────────────────────┬─────────────────────────────────────────┘
                                │ also the landing zone for the tier-3 oracles
┌───────────────────────────────▼─────────────────────────────────────────┐
│  degenbot-simulation (revm owner) — in-process EVM engine              │
└─────────────────────────────────────────────────────────────────────────┘
```

- **Deep reusable** lives in the simulation crate: one EVM spine shared by the
  tier-3 on-chain oracles **and** any investigation harness.
- **Scaffolding** lives at the user layer: a generator that emits a fresh,
  standalone per-contract project for *your* specific use case. Nothing
  user-specific is baked into the core.

---

## Layer 1 — `degenbot_simulation::oracle` (the deep driver)

The genuinely reusable primitive. It knows nothing about pools, executors, or
backruns — it only sequences EVM transactions against a fresh self-contained
`CacheDB`. Add it with `cargo add degenbot-simulation`.

### API

| Function | What it does |
|----------|--------------|
| `new_fixture_evm()` | Build a pristine revm EVM over an empty `CacheDB`. One per probe. |
| `set_disable_nonce_check(&mut, bool)` | Turn off the nonce check for arbitrary staged txs. |
| `set_code_size_limits(&mut, Option<usize>)` | Raise/clear contract+initcode size caps (deploying oversized harnesses, e.g. EIP-170).
| `deploy(&mut, Bytes, gas) -> Result<Address, String>` | Deploy creation bytecode, return its address. |
| `transact(&mut, TxSpec) -> Verdict` | Run a `Deploy`/`Call` and classify the result. |
| `call_bytes(&mut, to, data, gas) -> Result<Bytes, String>` | A call that must succeed; returns the raw output. |
| `read_address(&mut, to, data, gas) -> Result<Address, String>` | Read an address out of a getter that returns a 32-byte word. |
| `seed_slots(&mut, account, &[(U256, U256)])` | Seed raw storage slots (`slot, value`) — no pool/executor interpretation. |
| `selector(&str) -> [u8;4]` | `keccak256(signature)` first 4 bytes. |
| `decode_error_string(&[u8]) -> Option<String>` | Decode a Solidity `Error(string)` revert payload. |
| `load_foundry_creation_bytecode(&Path, file, contract) -> Result<Vec<u8>,String>` | Load a foundry artifact's creation bytecode. |

### The Revert-vs-Halt distinction (write this down)

`transact` returns a [`Verdict`] with three cases:

- `Verdict::Accepted { output, logs }` — the call succeeded.
- `Verdict::Reverted(Bytes)` — a **Solidity `REVERT`**: a math-level *verdict* you
  are expected to match against (e.g. a pool rejecting a swap).
- `Verdict::Halted(String)` — the EVM **halted** (OOG / invalid opcode) with *no
  verdict*. For oracle work this is a legitimate "state not computable", not a
  failure of your harness.

Conflating these two is exactly the kind of silent bug this module exists to
prevent (ADR-020 H1).

### Worked example

```rust
use alloy::primitives::Bytes;
use degenbot_simulation::oracle::{
    deploy, new_fixture_evm, selector, set_disable_nonce_check, transact, TxSpec, Verdict,
};

fn probe() -> Result<(), String> {
    // Load your contract's creation bytecode (or paste hex).
    let init = degenbot_simulation::oracle::load_foundry_creation_bytecode(
        std::path::Path::new("tier3-oracle/artifacts"),
        "MyContract.sol",
        "MyContract",
    )?;

    let mut evm = new_fixture_evm();
    set_disable_nonce_check(&mut evm, true);

    let contract = deploy(&mut evm, Bytes::from(init), 16_700_000)?;
    println!("deployed @ {contract}");

    // Seed raw storage (your layout).
    degenbot_simulation::oracle::seed_slots(&mut evm, contract, &[]);

    // Drive a call and classify the verdict.
    match transact(&mut evm, TxSpec::Call {
        to: contract,
        data: Bytes::from(selector("swap(bool,int256,uint160)").to_vec()),
        gas: 16_700_000,
    }) {
        Verdict::Accepted { output, .. } => println!("accepted: {output:?}"),
        Verdict::Reverted(r) => println!("REVERTED (verdict): {r:?}"),
        Verdict::Halted(h) => println!("HALTED (no verdict): {h}"),
    }
    Ok(())
}
```

### It is the tier-3 oracle landing zone

The driver is not speculative surface — the tier-3 V3 on-chain oracle
(`rust/crates/degenbot-pools/tests/tier3_v3_common/mod.rs::run_onchain_swap`) now
drives the real `UniswapV3Pool`/PancakeSwap bytecode through it, and all 9
byte-exact tests still pass. When you add a new concentrated liquidity math 
capability, extend the tier-3 oracle slice per 
[ADR-020](docs/adr/ADR-020-tier3-onchain-accuracy-oracle.md).

---

## Layer 2 — Scaffold your own contract (user layer)

Don't hand-write a harness for each new contract. Generate it:

```bash
python3 scripts/scaffold_revm_harness.py \
    --name my_executor \
    --artifact /path/to/out/MyExecutor.sol/MyExecutor.json \
    [--slot 0=0x… --slot 5=0x…]
```

This writes a standalone project to `investigations/my_executor/` with the
`degenbot_simulation::oracle` plumbing pre-wired, the contract's real creation
bytecode baked in, and `⬇ EDIT` markers where you drop in your contract's calls,
constructor args, and storage slots. Then:

```bash
cd investigations/my_executor
cargo run      # after editing src/main.rs for YOUR contract
```

The scaffold is contract-agnostic by construction: it depends only on
`degenbot-simulation` + `alloy`, and contains zero degenbot pool/executor logic.
Use it for **your** executor, a lending market, a new pool family — anything.

---

## Layer 3 — degenbot's own path investigations

For reproducing degenbot's captured failing backrun paths (the
`tests/fixtures/path<N>_…_block<B>.json` files), use the
`degenbot::investigation` toolkit:

```rust
use degenbot::investigation::{PathFixture, register_v2, register_v3, build_v3_state, v3_hop_output};

let fx = PathFixture::load("tests/fixtures/path5000_v2v4v3_block25704509.json")?;
let pid0 = register_v2(&mut engine.core().write(), &fx.pools["v2_0"])?;
let st   = build_v3_state(&fx.pools["v3_2"]);
v3_hop_output(&st, fee, spacing, zero_for_one, input);  // vs the tier-3-validated twin
```

This is degenbot's **own** pool-level scaffold (its capture format, its V2/V3/V4
reconstruction). It contains no executor logic. Its per-hop oracle checks use the
fast Rust twins (`v2_get_amount_out` / `v3_simulate_swap` / `v4_simulate_swap`),
which the tier-3 oracles prove byte-exact to the real contracts; for a deep
bytecode-level probe, drive the same state through `degenbot_simulation::oracle`
instead.

---

## Choosing a layer

| You want to… | Use |
|--------------|-----|
| Sequence raw EVM transactions against a contract (any contract) | `degenbot_simulation::oracle` |
| Investigate a specific new contract you don't want in the core | `scripts/scaffold_revm_harness.py` |
| Reproduce a degenbot captured backrun path (V2/V3/V4 hops) | `degenbot::investigation` |
| Prove a pool-swap math change against real bytecode | tier-3 oracle slice (ADR-020) |

---

## Migration notes

- **Don't** hand-roll `CacheDB::new(EmptyDB::default())` + `Context::mainnet()` +
  `transact` + `commit` + Revert/Halt matching in a new harness — call the driver.
- **Don't** add executor-specific driver logic to the core crates. If a new
  use case can't be expressed with the existing driver, extend
  `degenbot_simulation::oracle` with a *generic* primitive, and let the user-layer
  scaffold consume it.
- When you move a harness onto the driver, the tier-3 tests are the guard: they
  stay byte-exact or they fail loudly (`bucket=…`), so a driver change never
  silently corrupts an oracle.
- **Retired one-off capture scripts (2026-08):** `capture_path13822_full_snapshot.py`
  and `capture_path205_v2v4v3_fixture.py` plus their fixtures
  (`path13822_v3v3v3_block25696004_onchain.json`, `path205_v2v4v3_block25695845.json`)
  were deleted as dead weight — no live test/example/doc consumed them (verified by
  whole-tree `rg` + git history). `capture_path13822_v3v3v3_fixture.py` (the
  parameterized `FIX_PATH_ID` generator for the `path13827` fixture) was deleted
  in the 2026-08-19 sweep (HAVRUW/SEG2PS) together with that fixture's last
  consumer (a run-once example).
- **Deleted one-shot path-debug examples (2026-08-19, HAVRUW/SEG2PS):** the 19
  `rust/crates/degenbot/examples/` run-once `path*`/`fee1`/`desync`/probe
  harnesses plus their fixtures and one-off capture/verify/watch scripts — no
  live test, example, or doc consumed them (verified by whole-tree `rg`; per
  CONTEXT.md, ad-hoc path fixtures are weak cross-checks to DELETE once the revm
  harnesses cover them). Kept: `standalone_consumer.rs`, the `path5000` /
  `path73385` fixtures + capture scripts (consumed by committed tier-3 regression
  tests). The Layer-3 worked example now points at the kept `path5000` fixture,
  and the `EXECUTE_CONFIG` constant whose calldata-dump examples went is deleted.
