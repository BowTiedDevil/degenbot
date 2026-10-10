# 3SCUHJ (PC11, epic GZPPS3): the direct-pool zero-balance refusal goes core-side

STATUS: **GREEN.** The direct-pool zero-balance contract now lives in the Rust core
(`check_pool_liquidity` -> `CurveInputsError::NoLiquidity`, raised from `curve_get_dy`), the Python
`raise NoLiquidity(...)` in `calculate_tokens_out_from_tokens_in` is deleted, and the exception
identity handed to Python callers is unchanged on both paths. Both required negative probes are
recorded verbatim below, each flipped once and reverted byte-for-byte. The manager's whole-workspace
ladder is PENDING (not mine to run); I ran it locally in the foreground and it is green — see §7.

Baseline cited: `docs/ergo-results/hqylek-result.md` shrink-counts table; landed PC4/PC5b notes
`docs/ergo-results/qngzvr-result.md` (§5, "the NoLiquidity contract moves core-side") and
`docs/ergo-results/iyl6qd-result.md` (item 5, the deferred direct-pool relocation). Landed chunk
range `937153a29..37d6d1c2e` was **not** re-opened.

---

## 1. Call-graph note (re-verified at HEAD `b2cacba82`, before any edit)

The entry is `CurveStableswapPool.calculate_tokens_out_from_tokens_in`
(`src/degenbot/curve/curve_stableswap_liquidity_pool.py`). Its `all(tokens_used_this_pool)` arm is
the **direct**-pool path (both coins are pool coins - a plain pool always, a metapool's own coins
too). It called `self.get_dy` after a Python pre-check; the metapool-with-base arm
(`any(...) and any(...)`) went to `_get_dy_metapool_with_base` -> `_get_dy_underlying`.

Both delegate straight into the Rust shell handle:

| Python | shell (`rust/crates/shells/degenbot-python/src/bot/pool.rs`) | core (`degenbot-substrate`) |
|---|---|---|
| `get_dy` :694 -> `self._py_pool.curve_get_dy(...)` | `PyLiquidityPool::curve_get_dy` :2103 | `BotState::curve_get_dy` :230 |
| `_get_dy_underlying` :721 -> `curve_get_dy_underlying(...)` | `PyLiquidityPool::curve_get_dy_underlying` :2146 | `BotState::curve_get_dy_underlying` :287 |

So the **direct-pool core method is `BotState::curve_get_dy`** (not the metapool
`curve_get_dy_underlying`). Per the settled shape, that is where the refusal must live; the check is
placed where `state.balances` is read, mirroring the metapool arm's placement before its
`resolve_dy_inputs`. `resolve_dy_inputs` itself is shared (also drives `curve_calc_token_amount` /
`curve_calc_withdraw_one_coin`), so the check goes in `curve_get_dy` only - not in the shared
resolver - keeping the blast radius to the swap path.

## 2. What changed (from the diff)

```
 rust/crates/foundation/degenbot-pools/src/curve_dy_io.rs            | 17 +++++-
 rust/crates/foundation/degenbot-pools/src/curve_dy_io/tests.rs      | 16 +++++-
 rust/crates/foundation/degenbot-pools/src/lib.rs                    |  4 +-
 rust/crates/foundation/degenbot-substrate/src/balance_vector_orchestration.rs | 10 +++-
 rust/crates/foundation/degenbot-substrate/src/lib.rs                |  2 +-
 rust/crates/foundation/degenbot-substrate/src/tests/registration.rs | 61 +++++++++++++++
 rust/crates/shells/degenbot-python/src/bot/pool.rs                  |  6 +++
 src/degenbot/curve/curve_stableswap_liquidity_pool.py               | 11 ++--
 8 files changed, 116 insertions(+), 11 deletions(-)
```

### 2a. The core-side check (Acceptance 1)

**`rust/crates/foundation/degenbot-pools/src/curve_dy_io.rs:91`**

```rust
pub fn check_pool_liquidity(balances: &[U256]) -> Result<(), CurveInputsError> {
    if balances.iter().any(U256::is_zero) {
        return Err(CurveInputsError::NoLiquidity);
    }
    Ok(())
}
```

A new single-vector sibling of the landed `check_metapool_liquidity` (which ORs a base vector and a
meta vector). The predicate shape differs (one vector vs two), so the paths are **not** force-merged:
`check_metapool_liquidity` is untouched, byte-for-byte, so the landed PC4 helper is not re-opened.
The typed-refusal *contract* is shared: both return `CurveInputsError::NoLiquidity`. The variant's
doc-comment widened from "A metapool base swap ..." to "A direct-pool or metapool swap where a
balance vector has a zero entry."

**Call site : `rust/crates/foundation/degenbot-substrate/src/balance_vector_orchestration.rs:249`**

```rust
        // The contract's zero-balance refusal, evaluated where the balance
        // vector is read (core-side; the companion's
        // `calculate_tokens_out_from_tokens_in` pre-validation is gone).
        check_pool_liquidity(&state.balances)?;
```

It uses `state.balances` - the pool's stored vector - not `override_balances`, exactly as the metapool
arm and exactly as the deleted Python pre-check (`any(balance == 0 for balance in self.balances)`)
did. No reachable semantics moved.

**Shell mapping : `rust/crates/shells/degenbot-python/src/bot/pool.rs:2134`**

```rust
            Err(CurveInputsError::NoLiquidity) => {
                return Err(curve_no_liquidity_error(
                    py,
                    "One or more of the tokens has a zero balance.",
                ));
            }
```

This is the **existing** mapper (`curve_no_liquidity_error`, :417) already used by the metapool arm
at :2188 - reused, not duplicated. `curve_no_liquidity_error` imports
`degenbot.exceptions.arbitrage.NoLiquidity` and instantiates it with `message=`.

New core test: `curve_get_dy_refuses_a_zero_balance_direct_pool`
(`rust/crates/foundation/degenbot-substrate/src/tests/registration.rs:200`) - a zero coin balance on
a registered direct pool returns `CurveInputsError::NoLiquidity` from `curve_get_dy`, and a fully
funded pool still computes dy. Unit tests `pool_liquidity_rejects_a_zero_balance` /
`pool_liquidity_passes_when_every_balance_is_nonzero` in
`rust/crates/foundation/degenbot-pools/src/curve_dy_io/tests.rs:364`.

### 2b. The Python deletion (Acceptance 2)

`src/degenbot/curve/curve_stableswap_liquidity_pool.py`, the `all(tokens_used_this_pool)` arm
(was lines 769-772, now 771-774):

```diff
         if all(tokens_used_this_pool):
-            if any(balance == 0 for balance in self.balances):
-                raise NoLiquidity(message="One or more of the tokens has a zero balance.")
-
+            # The zero-balance refusal is core-owned: `curve_get_dy` raises the
+            # typed `NoLiquidity` where the balances are read, so this delegates.
             return self.get_dy(
```

Two consequential edits in the same file:
- `from degenbot.exceptions.arbitrage import NoLiquidity` (:28) deleted - it became unused (ruff
  F401). The class itself does **not** move; it stays in `src/degenbot/exceptions/arbitrage.py:147`,
  re-exported from `degenbot.exceptions`.
- The `calculate_tokens_out_from_tokens_in` docstring lost its `NoLiquidity:` line from the
  `Raises:` block - ruff's `docstring-extraneous-exception` (new in the lint gate) rejects a listed
  exception the function no longer raises *explicitly*. The behaviour is still documented, in prose
  in the docstring body (plus the arm comment), because it still reaches callers transitively via the
  core.

`rg -n 'raise NoLiquidity' src/` is now empty; the only remaining literal is the shell's message
string in `pool.rs`.

### 2c. Exception identity (Acceptance 2, constraint c)

`NoLiquidity` -> `ArbitrageError` -> `DegenbotError` -> `Exception` (MRO verified live). It is **not**
a `ValueError`, so the `except ValueError: raise EVMRevertError(...)` wrappers in `get_dy` (:693) and
`_get_dy_underlying` (:722) do **not** swallow it - the typed refusal propagates with its identity
intact. Both paths now raise the same class object, because both shell arms call the one
`curve_no_liquidity_error` helper which imports the one Python class:

```
identity degenbot.exceptions.NoLiquidity is arbitrage.NoLiquidity: True
mro: ['NoLiquidity', 'ArbitrageError', 'DegenbotError', 'Exception', 'BaseException', 'object']
```

`tests/curve` has no narrow `raise NoLiquidity` assertion to update - it only catches the type inside
broad `except (..., NoLiquidity, ...)` tuples (`test_curve_stableswap_pool.py:94,160,333`), which
still match.

## 3. Negative probe (a) - zero balance on the DIRECT path, refusal from the core

Flip: none (this is the landed state). Fault injection via `external_update` (writes the Rust core's
stored balance vector), then a direct FFI call and the companion call. Verbatim:

```
$ uv run --no-sync python /tmp/probe_a.py            # stdout
pool: 0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7 balances before: (27137121100372139339642768, 27662471158553, 108298644409129)
balances after : (0, 27662471158553, 108298644409129)
--- direct FFI call (core) ---
FFI raised degenbot.exceptions.arbitrage.NoLiquidity: One or more of the tokens has a zero balance.
--- companion calculate_tokens_out_from_tokens_in ---
raised degenbot.exceptions.arbitrage.NoLiquidity: One or more of the tokens has a zero balance.
is NoLiquidity identity: n/a
                                                    # stderr (the traceback)
Traceback (most recent call last):
  File "/tmp/probe_a.py", line 25, in <module>
    lp.calculate_tokens_out_from_tokens_in(token_in=tok_in, token_out=tok_out, token_in_quantity=10**18)
  File "/workspaces/degenbot/src/degenbot/curve/curve_stableswap_liquidity_pool.py", line 771, in calculate_tokens_out_from_tokens_in
    return self.get_dy(
  ...
  File "/workspaces/degenbot/src/degenbot/curve/curve_stableswap_liquidity_pool.py", line 694, in get_dy
    return self._py_pool.curve_get_dy(i, j, dx, block_number, override_balances)
degenbot.exceptions.arbitrage.NoLiquidity: One or more of the tokens has a zero balance.
```

Reading: the exception is raised at the **FFI call boundary** (line 694, inside `curve_get_dy`), not
by any Python arm - the old `raise` frame is gone and line 771 is now the delegating `return
self.get_dy(...)`. The typed refusal, with the contract's generic message, comes out of the core.

## 4. Negative probe (b) - flip the core check to a no-op

Flip (one edit, in `check_pool_liquidity` only):

```diff
-pub fn check_pool_liquidity(balances: &[U256]) -> Result<(), CurveInputsError> {
-    if balances.iter().any(U256::is_zero) {
-        return Err(CurveInputsError::NoLiquidity);
-    }
+pub fn check_pool_liquidity(_balances: &[U256]) -> Result<(), CurveInputsError> {
     Ok(())
 }
```

Rebuilt (`just dev`, receipt 2239), re-ran the same probe. Verbatim:

```
pool: 0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7 balances before: (27137121100372139339642768, 27662471158553, 108298644409129)
balances after : (0, 27662471158553, 108298644409129)
--- direct FFI call (core) ---
FFI raised builtins.ValueError: Curve get_dy failure: Swap(Invariant(Overflow))
--- companion calculate_tokens_out_from_tokens_in ---
raised degenbot.exceptions.pool.EVMRevertError: EVM Revert: Curve get_dy failure: Swap(Invariant(Overflow))
is NoLiquidity identity: n/a
                                                    # stderr
Traceback (most recent call last):
  File "/workspaces/degenbot/src/degenbot/curve/curve_stableswap_liquidity_pool.py", line 694, in get_dy
    return self._py_pool.curve_get_dy(i, j, dx, block_number, override_balances)
ValueError: Curve get_dy failure: Swap(Invariant(Overflow))

The above exception was the direct cause of the following exception:

Traceback (most recent call last):
  File "/tmp/probe_a.py", line 25, in <module>
    lp.calculate_tokens_out_from_tokens_in(token_in=tok_in, token_out=tok_out, token_in_quantity=10**18)
  File "/workspaces/degenbot/src/degenbot/curve/curve_stableswap_liquidity_pool.py", line 771, in calculate_tokens_out_from_tokens_in
    return self.get_dy(
  ...
  File "/workspaces/degenbot/src/degenbot/curve/curve_stableswap_liquidity_pool.py", line 696, in get_dy
    raise EVMRevertError(error=str(e)) from e
degenbot.exceptions.pool.EVMRevertError: EVM Revert: Curve get_dy failure: Swap(Invariant(Overflow))
```

Reading: with the check off, the zero-balance input **reaches the math**, which blows up as
`Swap(Invariant(Overflow))` - surfacing to Python as a generic `ValueError` / `EVMRevertError`, and
(once already inside `get_dy`) getting swallowed into the `EVMRevertError` re-wrap. That is exactly
the loud, misclassified failure the typed check prevents: without it, a zero-balance pool is not
refused as `NoLiquidity` and the caller cannot classify it. (The Python-side pre-check is gone, so
nothing else steps in - the safety now rests solely on the core check.)

Revert: `curve_dy_io.rs` restored from a pre-flip copy; `md5sum` back to
`c92e85b989e9b1a4af65fa98e22b11d1` and `git diff` for the file byte-identical to the pre-flip
intended diff (`diff` of the two diffs is empty). Rebuilt (`just dev`, receipt 2240) - and the
fingerprint returned to the pre-flip `d08f93eecc37d280`, which is independent proof the flip left no
residue in the compiled artifact.

## 5. Gates (run in the foreground; exits captured directly)

| gate | command | exit |
|---|---|---|
| Rust tests | `just test-rust-nextest` | **0** (4735 run, 4735 passed, 29 skipped) |
| Rust lint | `just lint-rust-check` | **0** |
| Python lint (ruff + ty) | `just lint-python-check` | **0** |
| Curve pytest | `uv run --no-sync pytest tests/curve -q` | **0** (47 passed) |
| Stub surface | `just gen-stubs --check` | **0** ("committed stubs match generator output") |
| Comment hygiene | `just lint-comment-hygiene` | **0** |
| rustfmt (touched .rs) | `rustfmt --check --edition 2021 --config skip_children=true -- <7 files>` | **0** |
| ruff format (touched .py) | `uv run --no-sync ruff format --check -- src/degenbot/curve/curve_stableswap_liquidity_pool.py` | **0** |
| Build receipt | `just verify-build-fresh` | **0** (2240 / `d08f93eecc37d280`) |

Scoped red-green loop first: `cargo test -p degenbot-pools -p degenbot-substrate` -> 0, with the two
new core tests observed running
(`curve_dy_io::tests::pool_liquidity_rejects_a_zero_balance`,
`tests::registration::curve_get_dy_refuses_a_zero_balance_direct_pool`).

**Stub surface: ZERO change**, as expected for a behaviour-only move. `just gen-stubs --check` exits 0
after regenerating into a temp dir (27 files written, 20 reformatted by the generator's own formatter)
and comparing against the committed `src/degenbot/_ffi/` tree. No `.pyi` file was touched by this
change; `check_pool_liquidity` is a Rust-internal helper reached only through the existing
`curve_get_dy` method, and the shell method's signature is unchanged.

`gen-stubs` needed `cargo build -p degenbot_rs --features experimental-inspect`; that feature-variant
build did not advance the receipt (source-content fingerprint unchanged).

## 6. What I deliberately left

- **`check_metapool_liquidity` untouched** (byte-for-byte). The task allowed "share the check if the
  shape allows"; the metapool helper ORs two vectors and the direct path has one, and the helper is a
  landed PC4 artifact in a chunk range I was told not to re-open. The sharing is at the *typed-
  refusal* level (`CurveInputsError::NoLiquidity`), with one small duplicated predicate line. If the
  manager prefers a literal dedupe, the one-line delegation is
  `check_metapool_liquidity(meta, base)` = `check_pool_liquidity(base)?; check_pool_liquidity(meta)`.
- **`resolve_dy_inputs` untouched.** Putting the check there would have swept in
  `curve_calc_token_amount` / `curve_calc_withdraw_one_coin` (which legitimately run on partially
  funded pools) and re-checked the metapool arm. The refusal is scoped to `curve_get_dy`.
- **`curve_calc_token_amount` / `curve_calc_withdraw_one_coin` get no zero-balance refusal** - out of
  scope; the Python companions never had one on those paths either.
- **`override_balances` not consulted by the check** - deliberate parity with the deleted Python
  pre-check (`self.balances`) and with the metapool arm (`state.balances`). Passing a zeroed override
  never refused before and does not refuse now.
- **No restructuring.** No curve module was re-shaped; the diff is the new helper + import/re-export
  plumbing + one call line + one shell match arm + the Python arm collapse + tests.
- `NoLiquidity` stays in `src/degenbot/exceptions/arbitrage.py` (re-exported) - untouched.

## 7. PENDING

- **Manager's whole-workspace ladder** (definition of done) - not mine to declare. I ran
  `just test-rust-nextest`, `just lint-rust-check`, `just lint-python-check`, `pytest tests/curve`,
  `just gen-stubs --check`, `just lint-comment-hygiene` and both fmt authorities in the foreground;
  all exit 0 as tabulated above. `just test-rust-nextest` exceeded the harness' 120s foreground bound
  once and completed in the background with exit 0 (4735/4735 passed) - re-runnable, but recorded here
  for honesty.
- **Foreign tree activity:** none observed. `git status --porcelain` shows exactly the eight files
  this chunk touches; no files outside my scope were modified, and the tree is left dirty (no
  commits, no `.ergo/` state changes).
