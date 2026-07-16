# ADR-013: The `_ffi` Seam Is Private (Pydantic Barrier)

**Status: accepted.** The decision is canonical; the cutover (rerouting
leaf imports, dissolving grab-bag files into mirror homes, tightening the
boundary test) is tracked as the candidate-3 epic. This ADR records the
seam decision so future architecture reviews do not re-suggest making
`degenbot._ffi` a stable public surface or re-introducing the flat-root
allowlist back-door.

## Context

`degenbot._ffi` is the Rust extension module (`degenbot._ffi.abi3.so`)
produced by maturin. After the flat→submodule conversion epic
(`XZ54NW`) and the companion-homes remap (`WLAB6U`), the seam was left in
an **inconsistent state**:

1. **A boundary test bans flat-root symbol imports**
   (`tests/test_ffi_boundary.py`: `from degenbot._ffi import <Symbol>` is
   banned in leaf code), signalling the intent that `_ffi` is private.
2. **An allowlist back-doors the ban** for 8 files that bridge flat-root
   symbols (`PyBot`, `UniswapArbEngine`, `PyErc20Token`, `PyLiquidityPool`,
   the `Verification*Error`s, `to_checksum_address`, `find_paths_rust`) —
   because those symbols have **no typed submodule** and therefore no
   stable home.
3. **Typed submodule imports are leaf-permitted**
   (`from degenbot._ffi.<sub> import X`), so `_ffi.<sub>` names appear in
   the import paths of ~28 leaf files, making `_ffi` half-public despite
   the ban and the `_` prefix.
4. **The companion-home convention is applied inconsistently**: the math
   leaves (`curve/math.py`, `balancer/math.py`, `uniswap/math.py`,
   `aerodrome/math.py`) are pure pass-through re-exporters over
   `_ffi.<sub>`; ~20 other domains import `_ffi.<sub>` directly into leaf
   code that carries the real logic (`contract/__init__.py`,
   `provider/__init__.py`, `database/operations.py`, `cli/*`, …).

The result: `_ffi` is half-treated as private (underscore, banned in leaf
code, absent from `__all__`, drivers never reach it) and half-treated as
public (it's the only place N symbols live, and `_ffi.<sub>` is a
leaf-importable path). The boundary test enforces a rule with an
allowlist, an AST-aware submodule-vs-symbol distinction, and a stale-
allowlist guard — the hardest-to-enforce part of degenbot's discipline.

### Survey of established Python-Rust projects

| Project | Raw `.so` name | Who imports it? | Ban? |
|---|---|---|---|
| Polars | `polars._plr` | 80 leaf files directly | No ban |
| **pydantic-core** | `_pydantic_core` | **One file**: `pydantic_core/__init__.py` | Structural — companion `pydantic` never touches `_pydantic_core` |
| cryptography | `hazmat.bindings._rust.*` | 42 leaf files via `from .._rust` | No ban, namespaced under `bindings/` |
| Ruff | `ruff` (no underscore) | N/A — `.so` IS the public module | N/A |

None mix models the way degenbot does today. **pydantic-core is the
match**: it has a strict one-module barrier (`_pydantic_core` imported
only by `pydantic_core/__init__.py`, which re-exports every symbol under
the stable `pydantic_core` name), the companion `pydantic` package
imports from `pydantic_core` (the stable name) and never `_pydantic_core`,
no allowlist, no boundary test. The `_` means what it says because there
is a complete re-export barrier and zero back-doors. pydantic-core scales
this model to a large, heavily-used companion-over-Rust-core — proven.

## Decision

### `degenbot._ffi` is private — the Pydantic barrier

`degenbot._ffi` is a **raw Rust extension imported by one barrier per
domain, never by leaf code.** The model is pydantic-core: every Rust
symbol reaches Python through a stable `degenbot.<domain>` home; leaf
code and drivers import from the home, never from `_ffi`.

**Ban rule (target):** *"no file outside its domain's barrier module may
contain the string `degenbot._ffi`."* Mechanically enforceable — a one-
pattern grep, no allowlist, no submodule-vs-symbol distinction. The
existing boundary test **shrinks** to this; the AST-aware ban logic, the
8-entry allowlist, and the stale-allowlist guard are all deleted.

### Home placement: 1:1 mirror

Every **consumed** `_ffi.<sub>` submodule maps to a `degenbot.<domain>`
home at the top level. Cross-cutting concerns are elevated to first-class
domains (`degenbot.abi`, `degenbot.crypto`, `degenbot.db`, `degenbot.fork`)
— **not** a `common` junk-drawer (the deletion test flags a `common.X`
pass-through as shallow). Homes are created **lazily** on first Python
consumer: dead submodules (`executor`, `subscriber`) stay un-homed until a
consumer appears, so no empty pass-through packages are created (the
Pydantic precedent — `pydantic_core` does not create homes for unused
Rust symbols).

The three top-level grab-bag `.py` files dissolve **into** their mirror
home rather than remaining floating peers: `abi_adapter.py` (483 lines,
the companion-over-Rust bridge, now dissolved) became `degenbot.abi`;
`crypto.py` (81 lines) becomes `degenbot.crypto`; `anvil_fork.py` (514
lines) becomes `degenbot.fork`.

### The `price` exception — two pyclasses, two domains

`_ffi.price` exposes two distinct pyclasses consumed by two different
domains: `PyChainlinkPriceFeed` → `degenbot.chainlink` (already
re-exported from `chainlink/__init__.py`), `PyAavePriceOracle` →
`degenbot.aave` (already re-exported from `aave/__init__.py`). The Rust
crate `degenbot-price` is implementation; its pyclasses belong to their
consuming domains. There is **no** `degenbot.price` home — the mirror
here is "each pyclass goes to its consuming domain," not "one home per
Rust submodule." This is the one place where the 1:1 mirror is per-
pyclass, not per-submodule, and it's correct because the two pyclasses
are genuinely different domain concepts (a Chainlink feed vs an Aave
oracle).

### `deployments` stays placed (not cross-cutting)

`degenbot._ffi.deployments` mirrors to
`degenbot.uniswap.deployments`, not a top-level `degenbot.deployments`.
PancakeSwap/SushiSwap/Swapbased/Camelot/Aerodrome are Uniswap-V2/V3
protocol forks; their CREATE2 deployment identity is genuinely
Uniswap-protocol-family data. The factory→identity lookup
(`resolve_deployer`, `resolve_v2/v3_init_hash`, `verify_v2/v3`) is the
standalone-Rust-core verification mechanism (ADR-005 / Fork A, `JC6OFG`):
`register_v2/v3_pool` re-resolves `(deployer, init_hash)` from the
embedded JSON and verifies the CREATE2 address at registration time. A
standalone `Bot` verifies with no Python; if the builder carried identity
in, Rust would trust rather than verify. **The lookup is load-bearing and
correctly placed; it is not eliminated and not cross-cutting.** (See
`CONTEXT.md` "deployments" disposition for the deferred Balancer carve-
out.)

## Consequences

### Positive

- **One seam, one rule.** `_ffi` is private by construction, not by
  discipline. The boundary test becomes a grep; the allowlist and its
  stale-entry guard are deleted.
- **Locality.** "Where does degenbot reach Rust?" has one answer per
  symbol: its `degenbot.<domain>` home. Navigating from a Python symbol
  to its Rust backing is one hop.
- **Leverage.** Drivers and leaf code learn one import shape
  (`degenbot.<domain>.*`), not three (flat-root, typed-submodule,
  companion-home). New modules have one obvious place to import from.
- **The `_` means what it says.** No ambiguity about whether `_ffi.<sub>`
  is public — it isn't.

### Negative / transitional

- **Reroute work.** ~28 leaf files currently importing
  `_ffi.<sub>` must reroute to `degenbot.<domain>`. The 8 allowlist
  files must gain (or consolidate to) stable homes for their flat-root
  symbols. No Rust structural change — the Rust crate structure was
  already right; this is a Python-side reroute + structural dissolution.
- **Three top-level packages are created** (`degenbot.abi`,
  `degenbot.crypto`, `degenbot.fork`) by dissolving the grab-bag files
  into them. The top-level surface grows; the grab-bag ambiguity shrinks.
- **Dead submodules stay un-homed.** `executor` (0 callers) and
  `subscriber` (test-only callers) do not get homes until a production
  consumer appears. The ban rule ("no `_ffi` outside barrier modules")
  does not require every submodule to have a home, only every consumed
  one.
- **`abi_adapter.py`'s backend-dispatch logic moved into `degenbot.abi`.**
  The `eth_abi` fallback for fixed-point types has been removed — Rust is
  the only backend, and unsupported types raise `AbiEncodeError` /
  `AbiDecodeError`. `degenbot.abi` is a deep module, not a shallow
  re-exporter.

### Does not change

- The Rust crate structure (`rust/crates/degenbot-*`) is unchanged.
  This ADR is about **the Python side of the FFI seam**, not the Rust
  crate topology. The standalone-Rust-core constraint (ADR-005) is
  unaffected: Rust owns everything; Python is a driver shell.
- ADR-005's three-layer architecture (Rust core / PyO3 wrapper / Python
  companion) is the foundation this seam sits on. This ADR specializes
  ADR-005's "Python companion" layer by pinning where the FFI seam lives
  (the `degenbot.<domain>` home) and what it hides (`_ffi`).
