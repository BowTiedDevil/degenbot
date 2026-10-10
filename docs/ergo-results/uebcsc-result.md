# UEBCSC (PC2, epic GZPPS3): registration goes idempotent — get-or-register at the register entries

STATUS: **PC2 structural work STANDS** (get-or-register at the core register entries, 7→0 on
the `py_bot or Bot()` workaround). The manager's whole-workspace ladder is PENDING (not mine to
run). Both required negative probes are recorded below with their deliberate flips.

Chunk: `register_*_pool` becomes the GET half too — a same-address + same-family + same-identity
re-registration answers the EXISTING pool id; only a genuine conflict (different family or a
mismatched identity, i.e. wire drift) raises.

---

## 1. What changed (from the diff, read by this worker)

### Rust — `rust/crates/engine/degenbot-bot/src/bot_core/registration.rs`

The seven `Bot::register_*_pool` facade methods (`register_v2_pool`, `register_v3_pool`,
`register_v4_pool`, `register_curve_pool`, `register_balancer_weighted_pool`,
`register_balancer_stable_pool`, `register_aerodrome_pool`) each gained an address-keyed (or
`(pool_manager, pool_id)`-keyed for V4) **get half** in front of the CREATE2 verify / params
assembly / write:

- **Hit** — the address is registered under the SAME family and the entry's immutable identity
  matches the requested one (`BuiltIdentity` fields: `token0` / `token1` / `factory`; V3 also
  `fee` + `tick_spacing`; V4 the pool-key currencies; Curve the coin list; Balancer the
  vault + pool id + coin list) → return the EXISTING `pool_id`, no second pool, no write.
- **Conflict** — a different family, or a same-family entry with a disagreeing identity → the
  existing loud typed `AlreadyRegistered` (V2/V3/V4/Aerodrome), or the state insert's retained
  already-registered panic (Curve/Balancer, which have no typed core error).

The family guard mirrors `route.rs`'s `registered_address_pool` discipline (an address registered
under a DIFFERENT family is not an answer). The `AlreadyRegistered -> PoolAlreadyRegisteredError`
mapping in `bot/errmap.rs` is UNCHANGED — it stays the genuine-conflict surface (ADR-012 admission
classes intact). PRG-4's engine-side path-dedup ledger (`arb_engine/path_registry.rs`,
`bot/engine/register.rs`) is untouched: a pool get-or-register hit creates no path and never
touches the path dedup counter.

Four core unit tests pin the arms (in the file's `tests` module):
`register_v2_pool_get_or_register_returns_the_existing_id`,
`register_v2_pool_same_address_different_identity_refuses_loudly`,
`register_v3_pool_on_a_registered_v2_address_refuses_the_family_collision`,
`register_curve_pool_get_or_register_reuses_the_existing_id`.

### Python — the seven factory files + `tests/helpers/pool_bot.py`

The inline fresh-bot fallback (the C5 workaround) is gone from all seven
`tests/helpers/*_factory.py` sites. **Design judgment (constraint 1):** the factory default stays a
FRESH, ISOLATED `Bot` — now minted through ONE named helper `tests/helpers/pool_bot.py::new_pool_bot()`
(so the factories carry no inline `Bot()` fallback) — because a shared *default* bot was implemented
and MEASURED to break 43 existing mutation tests (see §5): get-or-register would silently reuse one
test's mutated pool in another. That is exactly the mutable-state hazard constraint 1 names as the
done bar, so isolation is the default and sharing is explicit. The `py_bot: Bot | None` parameter is
retained as the dependency-injection seam. The factory docstrings document the contract. With the
runtime `Bot()` use gone, `from degenbot._ffi import Bot` moved into each file's `if TYPE_CHECKING:`
block (ruff `typing-only-first-party-import`); the annotation `py_bot: Bot | None` still resolves.

### Python — the read-only parity payoff (constraint 2)

- `tests/aerodrome/test_aerodrome_v2_onchain_parity.py`: the per-call `Bot()` workaround (the
  mutation the baseline task body cites) is deleted; ONE module-level `_PYBOT` is passed to
  `make_aerodrome_v2_pool` so both parity tests + the keys-diff test share it (get-or-register
  answers the same handle in any order).
- `tests/uniswap/v2/test_camelot_v2_onchain_parity.py`,
  `tests/uniswap/v3/test_uniswap_v3_onchain_parity.py`,
  `tests/uniswap/v4/test_uniswap_v4_onchain_parity.py`,
  `tests/pancakeswap/test_pancakeswap_v2_onchain_parity.py`: the existing module-level `_PYBOT` is
  now threaded into the pool factory (`py_bot=_PYBOT`) so the parity test and the keys-diff test
  share one bot instead of building the pool in a fresh per-call bot.

### Python — golden keys-diff order-independence (constraint 3 / settled shape)

`tests/golden/oracle.py::assert_golden_keys_exact`'s docstring now states order-independence as an
explicit property: the gate compares two key SETS, invariant to the recorded file's entry order and
the replay's build order.

### Python — the seam tests that asserted a same-identity duplicate raises

`tests/exceptions/test_pybot_admission_exceptions.py` is updated to the new contract: the four
`*_duplicate_*_raises_pool_already_registered` tests become `*_duplicate_*_returns_the_existing_handle`
(assert the returned id equals the first), and each of V2/V3/V4/Aerodrome gains a genuine-conflict
probe (different family, or a different pool key) that still raises the typed refusal. These are the
negative probes below.

---

## 2. Counts delta

Baseline source: `docs/ergo-results/hqylek-result.md` (PC0, run 2, 2026-10-10).

| # | shrink target | PC0 baseline | PC2 observed | delta |
|---|---|---|---|---|
| C5 | `py_bot` factory workarounds in `tests/helpers/*.py` (`py_bot or Bot` / `py_bot if py_bot is not None else Bot`) | **7** | **0** | **-7** |

```bash
rg -n 'py_bot or Bot|py_bot if py_bot is not None else Bot' tests/helpers/   # -> no matches
rg -c 'py_bot or Bot|py_bot if py_bot is not None else Bot' tests/helpers/  # -> (nothing) = 0
```

The `_POOL_BOT` shared-default experiment (since reverted) is gone:
`rg '_POOL_BOT' tests/helpers/` -> no matches.

---

## 3. Negative-probe demonstration records (both required, red path shown once each)

### (a) duplicate address + same identity -> returns the EXISTING handle, WITHOUT raising

Python seam probe `TestV2SeamAdmission::test_duplicate_address_returns_the_existing_handle`
(green, exit 0), then the assertion deliberately flipped to `assert second != first`:

```bash
uv run --no-sync pytest 'tests/exceptions/test_pybot_admission_exceptions.py::TestV2SeamAdmission::test_duplicate_address_returns_the_existing_handle' -q -p no:randomly; echo EXIT=$?
#   >       assert second != first  # FLIPPED PROBE: must fail
#   E       assert 1 != 1
#   1 failed in 3.33s
# EXIT=1
```

`assert 1 != 1` proves load-bearing: the second `register_v2_pool` for the same
`(address, token0, token1, factory)` returned the SAME `pool_id` (1) — no second pool, no raise.
Reverted byte-for-byte; the green run is `2 passed in 7.68s`, `EXIT=0`.

Rust twin (already in the crate suite): `register_v2_pool_get_or_register_returns_the_existing_id`.

### (b) different-family collision -> still raises the typed refusal

Python seam probe `TestV2SeamAdmission::test_different_family_collision_still_raises` (register a V2
pool at an address, then attempt `register_v3_pool` at the SAME address; green, exit 0), flipped to
expect the wrong typed exception:

```bash
uv run --no-sync pytest 'tests/exceptions/test_pybot_admission_exceptions.py::TestV2SeamAdmission::test_different_family_collision_still_raises' -q -p no:randomly; echo EXIT=$?
#   with pytest.raises(SpecViolationError) as exc_info:  # FLIPPED PROBE
# E   degenbot._ffi.PoolAlreadyRegisteredError: V3 pool already registered: address=0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11
#   1 failed in 3.36s
# EXIT=1
```

The flip proves load-bearing: a different family at the same address is NOT an answer, so the family
guard does not fire the get half and the collision surfaces as the typed
`PoolAlreadyRegisteredError` (not `SpecViolationError`). Reverted; the file's green run is
`25 passed in 3.35s`, `EXIT=0`.

Rust twin (already in the crate suite):
`register_v3_pool_on_a_registered_v2_address_refuses_the_family_collision`.

---

## 4. Scoped gates (foreground; exits captured directly — no pipes on the captures)

| gate | command | result |
|---|---|---|
| Rust workspace tests (touched crates) | `just test-rust-nextest -p degenbot-bot -p degenbot_rs` | **GREEN** — `4714 tests run: 4714 passed, 29 skipped`, `EXIT=0` |
| Rust get-or-register pins | `just test-rust-nextest -p degenbot-bot get_or_register` exposes 2 new tests | **GREEN** — `2 passed`, `EXIT=0` |
| Rust conflict pins | `… -p degenbot-bot refuses_the_family_collision refuses_loudly` | **GREEN** — `2 passed`, `EXIT=0` |
| Build receipt (rust-rebuild skill) | `just rebuild-if-stale` | **GREEN** — rebuilt via `just dev`, then `extension is fresh` (`2190 532824a25aab98b2`), `EXIT=0` |
| Scoped Python gate | `uv run --no-sync pytest tests/golden tests/pool_companion tests/aerodrome tests/pancakeswap` | **GREEN** — `234 passed, 4 skipped in 5.39s`, `EXIT=0` |
| Order-shuffled proof | same + `tests/exceptions/test_pybot_admission_exceptions.py`, `--randomly-seed=20261010` | **GREEN** — `259 passed, 4 skipped`, `EXIT=0` |
| Touched seam file | `uv run --no-sync pytest tests/exceptions/test_pybot_admission_exceptions.py` | **GREEN** — `25 passed in 3.35s`, `EXIT=0` |
| Negative probe (a) flip | (assertion deliberately flipped) | **RED as designed** — `assert 1 != 1`, `EXIT=1`; reverted |
| Negative probe (b) flip | (expected type deliberately flipped) | **RED as designed** — real `PoolAlreadyRegisteredError`, `EXIT=1`; reverted |
| Python lint (touched files) | `uv run --no-sync ruff check <14 touched files>` | **GREEN** — `All checks passed!`, `EXIT=0` |
| Re-verify after the import move | scoped gate + `tests/exceptions` `--randomly-seed=777`; broad factory-consuming suites | **GREEN** — `259 passed, 4 skipped`, `EXIT=0`; `1958 passed, 30 skipped`, `EXIT=0` |

Cargo gates ran from `/workspaces/degenbot` via the `just` recipe (AGENTS.md law: narrow Rust runs
go through `just test-rust-nextest -p <crate>`, never bare cargo). No pipes on any exit capture;
nothing detached.

---

## Manager review — verdict PASS (2026-10-10)

Independently re-verified against the diff: C5 precise pattern (`py_bot or Bot|py_bot if py_bot is not
None else Bot`) = **0 hits**; the core GET half is family-guarded with the registry's own identity
values (`get_v2_identity`/`get_v3_identity`, V4 `pool_key` currencies) and the loud
`AlreadyRegistered` is retained for genuine conflicts — exactly the settled shape. The isolation\default (`pool_bot.py::new_pool_bot`) is one named, documented policy; `py_bot=` stays the DI seam.
Both negative-probe flips recorded with RED exits and reverted.

Manager ladder (`/tmp/pc2-gates/`): `just pre-push` **exit=0** ("✓ all pre-push gates passed.") +
`just gen-stubs --check` **exit=0**. No foreign build-lock collision this run. Green against the
PC0 baseline (run 2).

---

## 5. Design judgment recorded: the shared factory default is NOT safe here

The task body's settled shape proposed dropping the fresh-bot workaround in favour of "one shared
`Bot` per test". I implemented a per-module shared default (`_POOL_BOT = Bot()`) first and MEASURED
its blast radius against the factory-consuming suites:

```bash
uv run --no-sync pytest tests/uniswap tests/curve tests/balancer tests/pool_handle tests/standalone_parity tests/types tests/registry tests/builders tests/fakes tests/arbitrage -q
# 43 failed, 1915 passed, 30 skipped   (EXIT=1)
#  - tests/balancer/test_tier3_balancer_companion.py::TestWeightedCompanionVsEngine::test_given_in_matches_engine  (42 params)
#  - tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py::test_simulations
#  - tests/curve/test_curve_stableswap_pool.py::test_a_ramping
#  - tests/fakes/test_fake_token.py::TestCanaryAerodromeV2PoolWithFakeToken::test_construct_curve_pool
```

Each failing test built the same pool address more than once with different mutable inputs and (now)
shared one default bot, so get-or-register handed back one test's mutated pool to another — the
exact "silently share mutable pool state between tests that mutate" failure constraint 1 forbids.
Each failed test PASSES in isolation (`1 passed`), confirming the mechanism is cross-test bleed.

Resolution: the factory default stays an isolated fresh `Bot` (now via the one `new_pool_bot()`
helper, so the inline C5 pattern is still eliminated — count 7 -> 0); sharing is explicit
(`py_bot=_PYBOT` in the read-only parity/keys-diff suites, the constraint-2 payoff). Constraint 1
says "Remove only the UNNECESSARY fresh-Bot-per-call workarounds" — the factory default is
NECESSARY, the parity workarounds were not, and those are what died. After the revert the
factory-consuming suites are green: `1958 passed, 30 skipped in 11.81s`, `EXIT=0`.

---

## 6. Manager review — PENDING

**PENDING (manager's to run — not this worker's):** the whole-workspace ladder — `just pre-push` /
`just test-rust-nextest` / `just test-python` (`just test`) and the PC2-scoped Python suites. This
worker ran the scoped gates in §4 only. The manager re-runs every PENDING gate.

---

## 7. Tree / foreign edits

- **Written by this chunk:** `rust/crates/engine/degenbot-bot/src/bot_core/registration.rs`;
  `tests/helpers/{v2,v3,v4,curve,balancer,aerodrome}_pool_factory.py`;
  `tests/helpers/pool_bot.py` (new); `tests/golden/oracle.py`;
  `tests/aerodrome/test_aerodrome_v2_onchain_parity.py`;
  `tests/uniswap/v2/test_camelot_v2_onchain_parity.py`;
  `tests/uniswap/v3/test_uniswap_v3_onchain_parity.py`;
  `tests/uniswap/v4/test_uniswap_v4_onchain_parity.py`;
  `tests/pancakeswap/test_pancakeswap_v2_onchain_parity.py`;
  `tests/exceptions/test_pybot_admission_exceptions.py`; and this file. No `.ergo/plans.jsonl` entry,
  no ergo task-state change, no commit. The two probe flips were reverted byte-for-byte
  (`rg 'FLIPPED' tests/exceptions/test_pybot_admission_exceptions.py` -> no matches).
- **Foreign activity observed:** the docs restructure + wave-5 stream named in the brief
  (`docs/adr/*`, `docs/getting-started.md`, `docs/index.md`, `docs/ergo-results/3r6ley-result.md`,
  `rust/crates/facade/degenbot/examples/generate_aave_config_windows.rs`,
  `rust/crates/integrations/degenbot-aave/tests/wave5_config_span_replay.rs`,
  `tests/fixtures/cassettes/wave5/`, `tests/fixtures/sql_goldens/wave5/`) left the working tree
  during this run — HEAD advanced to `7a8363447` (another session's commit
  `docs(adr): mark superseded ADRs; replace index.md with a provenance-aware README`). Not mine; not
  touched, staged, or reverted.
- No command detached past the harness bound; every gate ran in the foreground.
