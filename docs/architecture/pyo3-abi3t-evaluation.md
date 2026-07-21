# Evaluation: PyO3 `abi3t` feature for wheel distribution

**Status: decided — do not adopt now.** Revisit when the project drops
Python 3.12–3.14 and the free-threading audit (see below) is complete.

Filed as `ergo` task `DRZBPR`. PyO3 0.29 (current `rust/Cargo.toml`)
introduces the `abi3t` / `abi3t-py315` features, targeting PEP 803's
"truly stable" limited API. This note records whether `abi3t` should
replace or supplement the current `abi3-py312` feature.

## Current state

```toml
# rust/Cargo.toml
pyo3 = { version = "^0.29", features = ["abi3-py312", "serde"] }
```

```toml
# pyproject.toml
requires-python = ">=3.12"
[tool.maturin]
bindings = "pyo3"
module-name = "degenbot.degenbot_rs"
features = ["pyo3/extension-module"]
```

CI matrix: Python 3.12, 3.13, 3.14 (`.github/workflows/*.yml`).

## Findings against the three checklist items

### 1. APIs available under `abi3t` vs `abi3-py312`

Identical **Rust-visible** API. PyO3 gates every limited-API code path
on the single `Py_LIMITED_API` cfg flag
(`pyo3-build-config/src/impl_.rs`, `InterpreterConfig::to_command()`),
and **both** `abi3` and `abi3t` set it:

```rust
match self.target_abi.kind() {
    PythonAbiKind::Stable(kind) => {
        out.push("cargo:rustc-cfg=Py_LIMITED_API".to_owned());
        if kind == StableAbi::Abi3t {
            out.push("cargo:rustc-cfg=Py_GIL_DISABLED".to_owned());
        }
    }
    // ...
}
```

So `abi3t` is *not* an alternate, smaller API surface than `abi3`. It is
`abi3` plus two extra constraints:

- **`Py_GIL_DISABLED`** — the true difference. `abi3t` is the
  free-threaded ("`t`" = truly stable) stable ABI. `abi3` (non-`t`) is
  rejected on the free-threaded build (`stable_abi(StableAbi::Abi3)` is
  skipped when `gil_disabled`).
- **Minimum Python 3.15.**
  `MINIMUM_SUPPORTED_VERSION_ABI3T = PythonVersion { major: 3, minor: 15 }`
  (`pyo3-build-config/src/impl_.rs`), and the `default_stable_abi_config`
  helper hard-errors below it:

  > `Cannot target an abi3t version below 3.15`

### 2. Does our code use any non-`abi3t`-compatible APIs?

No *compile-time* incompatibility: the crate already builds under
`Py_LIMITED_API` today (via `abi3-py312`), so the limited-API surface is
already in use and clean. The compatibility blocker is **soundness under
free-threading**, not API availability:

- `abi3t` implies `Py_GIL_DISABLED`. Enabling it opts every PyO3 class
  into the free-threaded build's borrow discipline.
- The codebase is mid-migration toward free-threading soundness:
  `PyRef<T>` → `PyClassGuard<T>` (task `TNXKA2`, the two `__aiter__`
  call sites) is done, but the broader audit (GIL release protocol,
  `parking_lot::Mutex` under `Py_GIL_DISABLED`, the Rust-owned bot
  architecture in `docs/architecture/rust-owned-bot.md`) is not
  declared complete.

Adopting `abi3t` would flip on free-threading expectations before the
audit is finished.

### 3. Does maturin support `abi3t` wheel tags yet?

Maturin (project pins `>=1.0,<2.0`, currently resolving 1.13.3) derives
the wheel abi tag from the pyo3 `InterpreterConfig` the Rust build emits
— it does not require a separate config knob. Maturin 1.13+ supports the
`abi3t` tag family. **This is therefore not a blocker**; the blocker is
items 1 + 2 (the Python 3.15 floor and the free-threading implication),
not maturin.

## Decision

**Do not replace `abi3-py312` with `abi3t` now.** `abi3t` cannot *replace*
`abi3-py312` because:

1. It requires **minimum Python 3.15**, but the package declares
   `requires-python = ">=3.12"` and CI tests 3.12–3.14. Switching would
   drop support for every supported Python.
2. It forces `Py_GIL_DISABLED` (free-threaded), which the crate has not
   been fully audited for.

It also cannot meaningfully *supplement* `abi3-py312` in a single wheel:
a wheel is either built against the limited stable ABI (abi3, 3.12+) or
the truly-stable free-threaded ABI (abi3t, 3.15+) — not both. Shipping a
parallel `abi3t` wheel would only be worth it once there is a real
audience on free-threaded Python 3.15+, which there is not yet.

## Revisit triggers

Re-open this evaluation when **all** hold:

- `requires-python` floor moves to `>=3.15` (or a separate free-threaded
  wheel is justified by real demand).
- The free-threading soundness audit is complete (GIL release protocol,
  `Mutex`/`RwLock` discipline under `Py_GIL_DISABLED`, per
  `rust/AGENTS.md`).
- `MINIMUM_SUPPORTED_VERSION_ABI3T` / maturin tag support has not
  regressed across a PyO3 bump.

At that point the change is a one-line Cargo feature swap
(`abi3-py312` → `abi3t-py315`) plus confirming the wheel tag, with no
Rust source changes expected (the limited-API surface is already in use).
