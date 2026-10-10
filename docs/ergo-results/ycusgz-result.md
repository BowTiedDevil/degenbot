# YCUSGZ (PC10, epic GZPPS3): typed block-tag support on the construction seam (ADR-023 D1)

STATUS: **LANDED — implementation + all mandated gates green on the final tree.**

ADR-023 D1 cut the `"latest"` block-tag fallthrough because `RpcConstruction` had no tag
support; Post-adoption A3 re-homed the follow-up here. The trait now carries a typed block
selector (wrapping alloy's `BlockNumberOrTag`), the tag → number resolution lives with the
RPC clock the trait owns, the Rust `extract_block_u64` coercion sites are gone, and the
Python tag-coalescing sites stop pretending a tag is the head's number. Legacy number and
`None` callers keep their exact behaviour (and cost).

Baseline cited: `docs/ergo-results/hqylek-result.md` run 2 (green). Landed pre-existing chunks
not re-opened: `937153a29`, `4a03f5e04`, `231334a8f`, `ad035c48a`, `345a8c118`, `5de576784`,
`d18c49b8d`, `c2f35dafd`, `e33b2c6c8`, `0ec15ef0d`, `8652f5fc8` (tree was clean at HEAD
`8652f5fc8` before this chunk).

## Shapes changed (diff)

| file | intent |
|---|---|
| `rust/crates/engine/degenbot-bot/src/bot_core/construction_io/block_selector.rs` (NEW) | `BlockSelector` — a typed block argument wrapping alloy's `BlockNumberOrTag` (a concrete number or a canonical tag), plus `BlockSelector::parse_tag` (`earliest`/`latest`/`pending`) and `BlockSelectorError::Unsupported` for any other string. Module unit tests pin the parse shape. |
| `.../construction_io/mod.rs` | The trait grows `RpcConstruction::resolve_block_selector(&self, BlockSelector) -> Result<u64, ProviderError>` — the ONE place a tag becomes a number. A number passes through; `earliest` is block 0; `latest` reads the head through `get_block_number`; `pending` is head + 1; any other tag is refused (`ProviderError::InvalidResponse`). Re-exports `BlockSelector`/`BlockSelectorError`. |
| `.../construction_io/handle.rs` | `ConstructionIo::resolve_block_selector` forwarder onto the `rpc` half (the composite is the surface builders reach). |
| `.../construction_io/tests.rs` | Two `FakeRpc`-backed tests: numbers/tags resolve against the clock (`latest` → head 42, `pending` → 43, `earliest` → 0), and an unnamed tag (`Safe`) is refused with the typed error. |
| `rust/crates/shells/degenbot-python/src/bot/py_bot_io.rs` | `extract_block_u64` is DELETED. `extract_block_selector` parses the `block` kw-sentinel into a typed `BlockSelector` (int → number; string → the canonical tag ladder; anything else → `TypeError`); `resolve_block_tag` / `resolve_block_against` resolve a typed tag through a `ConstructionIo`; `PyBotIo::resolve_block` is the 22-site seam helper. A number or `None` short-circuits with no RPC, so legacy number callers pay nothing. Module docs updated (tags now resolve at the seam; non-tag strings are refused). |
| `rust/crates/shells/degenbot-python/src/bot/pool.rs` | `Pool.refresh_via(io, block_number=None)` now takes a typed block argument (`Option<&Bound<PyAny>>`), resolves it through the seam (`resolve_block_against`) and hands the core `refresh_pool` the resolved `Option<u64>` — `refresh_pool` stays numeric. |
| `src/degenbot/types/abstract/liquidity_pool.py` | `web3`-era `refresh_via(io, *, block_number: int \| str \| None)` (the old `BlockIdentifier`) restored; the pass-through no longer pretends the tag is a number, and the docstring records that resolution belongs to the handle seam that owns the RPC clock. |
| `src/degenbot/bot/_bot.py` | `Bot.update`'s `int(block_number)` pre-coercion is DELETED; the identifier is handed through to `Pool.refresh_via` unchanged. |
| `src/degenbot/provider/call_helpers.py` | `raw_call` no longer coalesces a non-int identifier to `None` (which reads as the head): it resolves through `get_number_for_block_identifier` (the provider's own block clock). `None` stays `None` (no extra round-trip). |
| `src/degenbot/_ffi/__init__.pyi` | regenerated stub (one def moved; see the self-check). |
| `tests/builders/test_pool_refresh_via.py` | Two refresh tests (`"latest"` resolves and applies; a decimal string is refused with `unsupported block identifier`) plus a `BotIo` seam test (a non-tag string is refused). |

Behaviour is preserved for number/`None` callers by construction: a number is short-circuited
before any `ConstructionIo` is built, and the core faces (`refresh_pool`, the 7 atomic RPC
methods, the choreography helpers) still receive `Option<u64>` exactly as before.

## Where the tag → number crossing lives

```
Python driver signature          FFI face                       core face
int | str | None          -->    BlockSelector (typed)     -->   u64 / Option<u64>
refresh_via(block_number)        PyBotIo::resolve_block          refresh_pool(.., Option<u64>)
Bot.update(block_number)         Pool.refresh_via                choreography(.., Option<u64>)
raw_call(block_identifier)       ConstructionIo::resolve_block_selector  (head read)
```

`resolve_block_selector` is the single resolution site; every numeric core face is unchanged.

## Stub diff self-check (`just gen-stubs`)

`git diff src/degenbot/_ffi/__init__.pyi` touches exactly ONE def (`Pool.refresh_via`); the
block parameter widens from a tag-rejecting `int | None` to the tag-carrying FFI spelling
(`Any | None`, what a `Bound<PyAny>` introspects to) and the docstring moves with it:

```
-    def refresh_via(self, /, io: BotIo, block_number: int | None = None) -> bool:
+    def refresh_via(self, /, io: BotIo, block_number: Any | None = None) -> bool:
```

`just gen-stubs` → exit 0; `just gen-stubs --check` → exit 0 ("committed stubs match generator
output").

## Negative-probe records (flip once, recorded verbatim, reverted byte-for-byte)

### Probe 1 — the silent tag coalescence is gone (`raw_call`, `"earliest"`)

A provider whose clock is block 7 and which can name block 0; the probe prints the block the
call actually went to.

`block_num = block_identifier if isinstance(block_identifier, int) else None` (the pre-change
body, restored for the probe), `uv run --no-sync python /tmp/ycusgz-probe1.py`:

```
block the call went to: 1000000000

exit:0
```

(`None` → the head; the `"earliest"` tag silently read the head instead of block 0.)

Body restored to the resolving form, same probe:

```
block the call went to: 0

exit:0
```

### Probe 2 — the resolution seam resolves `latest` to a real number (sentinel flip)

`BlockNumberOrTag::Latest => self.get_block_number().await` flipped to
`Ok(u64::MAX)` in `RpcConstruction::resolve_block_selector`, then
`just test-rust-nextest`:

```
        FAIL [   0.007s] (1042/4732) degenbot-bot bot_core::construction_io::tests::block_selector_resolves_numbers_and_tags_against_the_clock
  stderr ───
    thread 'bot_core::construction_io::tests::block_selector_resolves_numbers_and_tags_against_the_clock' (3859855) panicked at crates/engine/degenbot-bot/src/bot_core/construction_io/tests.rs:217:5:
    assertion `left == right` failed
      left: 18446744073709551615
     right: 42
...
     Summary [  10.310s] 4732 tests run: 4731 passed, 1 failed, 29 skipped
error: recipe `test-rust-nextest` failed with exit code 100
```

The flip was reverted byte-for-byte; the same suite re-ran green (4732 passed / 29 skipped,
exit 0).

## Counts delta (baseline `docs/ergo-results/hqylek-result.md`; re-counted read-only @ the final tree)

| # | metric | before | after | delta | attribution |
|---|---|---|---|---|---|
| E1 | `extract_block_u64` sites under `rust/crates/shells` (22 call sites + 1 definition) | 23 | **0** | **−23** | THIS chunk |
| E2 | `BotIo`/`Pool` FFI `block`/`block_number` params annotated `Any \| None` (can carry a tag) | 22 | **23** | **+1** | THIS chunk |
| E3 | FFI `block`/`block_number` params annotated `int \| None` (tag-rejecting) | 16 | **15** | **−1** | THIS chunk |
| E4 | Python driver-seam signatures annotated `block_number: int \| str` | 2 | **3** | **+1** | THIS chunk |
| E5 | silent tag→head coalescions (Rust `extract::<u64>` seam + the Python `raw_call` drop) | 23 + 1 | **0** | **−24** | THIS chunk |
| E6 | trait-level tag-resolution sites | 0 | **1** (`RpcConstruction::resolve_block_selector`) | **+1** | THIS chunk |

Verification one-liners (read-only, foreground):

```bash
git grep -n 'extract_block_u64' HEAD -- rust/crates/shells | wc -l       # -> 23
rg -n 'extract_block_u64' rust/crates/shells | wc -l                     # -> 0
rg -n 'resolve_block\(py, block\)' rust/crates/shells/degenbot-python/src/bot/py_bot_io.rs | wc -l   # -> 22
git grep -n 'block: Any | None\|block_number: Any | None' HEAD -- src/degenbot/_ffi/__init__.pyi | wc -l  # -> 22
rg -n 'block: Any \| None|block_number: Any \| None' src/degenbot/_ffi/__init__.pyi | wc -l          # -> 23
git grep -n 'block: int | None\|block_number: int | None' HEAD -- src/degenbot/_ffi/__init__.pyi | wc -l  # -> 16
git grep -n 'block_number: int | str' HEAD -- src/degenbot | wc -l        # -> 2
rg -n 'block_number: int \| str' src/degenbot | wc -l                    # -> 3
```

## Acceptance (task body)

- `rg "extract_block_u64" rust/crates/shells` — **EMPTY** (exit 1). The 22 call sites now go
  through a typed conversion (`extract_block_selector` → `BlockSelector` → the clock's
  `resolve_block_selector`), never a bare `extract::<u64>`. ✅
- A tag-typed negative probe demonstrates the old fallthrough is gone. ✅ (Probe 1: old body
  silently read the head for `"earliest"`; new body resolves to block 0. Probe 2: the resolve
  seam's `latest` arm flip fails the pin with the sentinel.)

## Deliberately left (recorded, not hidden)

- **`Pool.simulate(zero_for_one, amount_specified, block, ...)`** — its `block` is a REQUIRED
  concrete number threaded into the core `simulate_mainline`, not a value crossing the
  RPC-clock seam (it selects the state block for tick fetches). Per the brief ("`Pool.simulate`
  core faces stay `int | None`") it is untouched.
- **`PyBotIo.fetch_balancer_stable_rates(block_identifier: Option<u64>)`** and its two Python
  callers (`balancer/stable_pools.py:76,348`) keep the documented `str → None` (latest) mapping:
  the Balancer dynamic rate-provider path is not the RPC-clock seam.
- **`contract.call`/`batch_call`** (its own `NotImplementedError` tag ladder, `"latest"` only)
  and **`curve_stableswap._resolve_block_number`** — pre-existing Python-layer passes with their
  own error contracts; not re-opened (scope discipline).
- **`build_*` / `construct_pool` / `verify_*` FFI faces** keep `block: int | None`: they receive
  an already-resolved number from the Python driver, which is the widened driver-seam signature.
- The **2-tuple `TickData::from_py_pairs` backfill** and the Balancer/V4 choreography bodies are
  untouched; only the seam that hands them a block changed shape.

## Scoped + mandated gate exits (captured DIRECTLY, `cmd; echo $?`, no pipes)

| gate | exit |
|---|---|
| `rustfmt --check --edition 2021 --config skip_children=true -- <6 touched .rs>` | **0** |
| `just fmt-check` (cargo fmt --all --check) | **0** |
| `uv run --no-sync ruff format --check -- <4 touched .py>` | **0** (4 already formatted) |
| `uv run --no-sync ruff check <4 touched .py>` | **0** ("All checks passed!") |
| `just lint-python-check` (ruff + `ty`) | **0** |
| `just lint-rust-check` (workspace clippy + arch subset) | **0** |
| `just lint-comment-hygiene` | **0** |
| `just gen-stubs` (regen) | **0** |
| `just gen-stubs --check` | **0** |
| `just rebuild-if-stale` | **0** |
| `just verify-build-fresh` | **0** |
| `just test-rust-nextest` (FULL workspace, final tree) | **0** — 4732 passed / 29 skipped |
| `uv run --no-sync pytest tests/builders tests/test_ffi_boundary.py` | **0** — 107 passed / 1 skipped |
| `uv run --no-sync pytest tests/builders/test_pool_refresh_via.py tests/test_ffi_boundary.py` | **0** — 16 passed |
| FLIP probe `just test-rust-nextest` (temporary, reverted) | **100** — 1 failed, reverted → **0** |

Logs: `/tmp/ycusgz-lint-rust.log`, `/tmp/ycusgz-lint-rust2.log`, `/tmp/ycusgz-lint-python{,2}.log`,
`/tmp/ycusgz-genstubs{,2,-check,-check2}.log`, `/tmp/ycusgz-rebuild{,2}.log`, `/tmp/ycusgz-verify.log`,
`/tmp/ycusgz-comment-hygiene.log`, `/tmp/ycusgz-fmt-check.log`, `/tmp/ycusgz-nextest-{bot,full,final}.log`,
`/tmp/ycusgz-probe1.log`, `/tmp/ycusgz-probe2.log`, `/tmp/ycusgz-pytest-{scoped,builders}.log`.
Probe script: `/tmp/ycusgz-probe1.py`.

One command detached past the harness bound: `just test-rust-nextest -p degenbot-bot` (the recipe
always runs `--workspace`, so it ran the full suite anyway — exit 0, 4732 passed / 29 skipped).
It was re-run in the foreground as the plain `just test-rust-nextest` twice afterwards.

## PENDING

1. **MANAGER LADDER: PENDING** — `just pre-push` whole-workspace lanes were not run by this
   worker (the mandated `just test-rust-nextest`, `just lint-rust-check`, `just lint-python-check`,
   `just gen-stubs --check`, `pytest tests/test_ffi_boundary.py` and the two fmt authorities were).
2. The **full Python suite** (`just test-python`) was not run by this worker (scoped `tests/builders`
   + `tests/test_ffi_boundary.py` were); the dispatcher runs the whole-workspace ladder.

## Tree / foreign edits

Files written by this chunk (10 + this doc): `rust/crates/engine/degenbot-bot/src/bot_core/construction_io/block_selector.rs`
(new), `.../construction_io/mod.rs`, `.../construction_io/handle.rs`, `.../construction_io/tests.rs`,
`rust/crates/shells/degenbot-python/src/bot/py_bot_io.rs`, `.../bot/pool.rs`,
`src/degenbot/_ffi/__init__.pyi` (regenerated), `src/degenbot/bot/_bot.py`,
`src/degenbot/provider/call_helpers.py`, `src/degenbot/types/abstract/liquidity_pool.py`,
`tests/builders/test_pool_refresh_via.py`, plus this doc.

The tree was CLEAN at HEAD `8652f5fc8` before this chunk; no foreign (unowned) modification was
present at any point during the run. No commit; no ergo task-state change; `.ergo/plans.jsonl`
untouched.

## Manager review — verdict PASS (2026-10-10)

Verified against the tree: `extract_block_u64` sites = **ZERO** (deleted, not wrapped); the ONE
resolution site is `resolve_block_selector` at the trait face, forwarded by `PyBotIo::resolve_block`
(`py_bot_io.rs:1500`); `BlockSelector` is a typed newtype over alloy's `BlockNumberOrTag`
(`construction_io/block_selector.rs`); the remaining `extract::<u64>` is the typed parse's own
number arm, not a coercion. The silent tag-to-head coalescing class is gone (probe 1: `"earliest"`
now resolves to 0 where the old body reported head 100000000). Manager battery `/tmp/pc10-gates/`:
pre-push 0, gen-stubs 0, comment-hygiene 0, rustfmt 0, ruff-format 0. GZPPS3 chain complete.
