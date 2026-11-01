# LLVM 23 on the release builders

The shipped Python extension links the evm2 JIT, and the JIT links LLVM 23. That
makes an LLVM 23 toolchain a **hard requirement of every job that compiles the
workspace or the extension** — release CI included. This page is the operator
side of that requirement: where it lands, the change that provisions it, the
measurements behind the choices, and what is still a decision.

## The requirement, exactly

`degenbot-simulation` forwards `degenbot-simulation/evm2-jit` from both
consumers: the binding's `simulation` feature (in `default`) and the umbrella
`degenbot` crate's `jit` feature (in `default`). `evm2-jit` enables
`evm2-jit-llvm`, which depends on `llvm-sys` — locked at **231.0.2**, i.e. LLVM
23.1 (`rust/Cargo.lock`).

`llvm-sys`'s build script discovers a toolchain in exactly two places:

| Mechanism | Detail |
| --- | --- |
| `$LLVM_SYS_231_PREFIX` | `llvm-config` must live at `$PREFIX/bin/`. The number is the **crate's** major version (`231`), not the LLVM major — `LLVM_SYS_23_PREFIX` and `LLVM_SYS_230_PREFIX` are both ignored. |
| `PATH` | Probing order: `llvm-config-23`, `llvm-config23`, `llvm23-config`, `llvm-config-23.1`, `llvm-config231`, `llvm-config`. |

Version acceptance (non-strict mode): LLVM **>= 23.1**. When the prefix variable
is set, `PATH` is not consulted. `pkg-config`, CMake, and distro tags play no
part, so "an LLVM is installed" is not the same as "llvm-sys can find it".

The devcontainer satisfies this through Fedora 45's `llvm-devel`
(`llvm-config-23` → 23.1.2, prefix `/usr/lib64/llvm23`).

## Where the requirement lands

| `release.yml` job | Compiles LLVM-linked code | Provisioned by this change |
| --- | --- | --- |
| `validate-branch` | no | — |
| `ci` (`uses: ./ci.yml`) | **yes** — `just lint-rust-check`, `test-rust`, `build-rust-extension` all build default features, i.e. the umbrella's `jit` | **no — see "Not applied" below** |
| `build-wheels` (Linux x86_64, Linux aarch64) | yes — inside the manylinux container | yes |
| `build-wheels` (macOS universal2, Windows MSVC) | yes — on the runner | **no — blocked, decision below** |
| `build-sdist` | no — packages sources only | — |
| `test-wheels` | no — installs the already-linked wheel | — |
| `publish-crates-io` | **yes** — `cargo publish --dry-run --workspace` verifies each package by building it, and the umbrella `degenbot` package builds with `jit` in its defaults | yes |
| `publish-to-pypi` / `publish-to-test-pypi` / `github-release` | no | — |

Two of those are easy to miss and both would have failed the next release before
any publish: the crates.io verify leg (a *dry-run* still builds) and the `ci`
gate job that the release runs first.

## The change

### `scripts/provision-llvm23.sh` (new)

One provisioning primitive, used by both kinds of consumer:

```bash
eval "$(scripts/provision-llvm23.sh --system-deps)"   # containers / before-scripts
scripts/provision-llvm23.sh --system-deps --github-env # runner steps ($GITHUB_ENV)
scripts/provision-llvm23.sh --check                    # assert discovery resolves 23.x
```

It pins LLVM **23.1.4** and the tarball's sha256 (taken from the release's
published build attestation):

| Asset | sha256 |
| --- | --- |
| `LLVM-23.1.4-Linux-X64.tar.xz` | `25d814fda50d5fb0084b42be09605d8b1595d49d5e2e03a3345d7e112ff6f9f3` |
| `LLVM-23.1.4-Linux-ARM64.tar.xz` | `9b27cf263d711aa6ebfd4045ab241d947bd09b3cd19ffa5728f48a80c9eb3a91` |

Four behaviours are load-bearing:

1. **The prefix variable is derived from the lock.** It reads `llvm-sys`'s major
   from `rust/Cargo.lock` and refuses to run when that crate targets a different
   LLVM major than the pinned tarball — a crate bump cannot silently leave the
   variable stale.
2. **A subset extraction**, not the whole tarball: `bin/llvm-config`, `include/`,
   `lib/libLLVM*.a`, and `lib/libPolly*.a` (~0.57 GiB) instead of ~8 GiB.
   `libPolly*.a` matters: `llvm-config --libs` names Polly outside the `LLVM*`
   component set, and the link fails with `could not find native static library
   'Polly'` without it.
3. **`--system-deps`** installs the archives the static link pulls in
   (`zlib, libzstd, ncurses, libxml2, libffi` — `libzstd-static` on dnf/yum, whose
   static archive Fedora keeps out of `libzstd-devel`).
4. **Recorded-path repair.** `llvm-config --system-libs` from the prebuilt prints
   its own build host's absolute path — `/usr/lib/x86_64-linux-gnu/libzstd.a` —
   and `llvm-sys` panics unless that path is a file where the build runs:

   ```
   thread 'main' panicked at llvm-sys-231.0.2/build.rs:383:
   Unable to parse result of llvm-config --system-libs: /usr/lib/x86_64-linux-gnu/libzstd.a
   ```

   The script links each such recorded path at the archive the machine actually
   ships. Without it, *every* Linux build with the prebuilt fails, on any distro
   layout that is not the tarball's own.

### `release.yml` (modified)

- `build-wheels` matrix: `manylinux: 2_28` → **`2_34`** for both Linux entries,
  because the prebuilt cannot run or link on glibc 2.28 (measurements below).
  **Ratified by the operator 2026-10-31**: the glibc-2.34 install floor ships;
  if a real user on an older distro appears, the dual-wheel fallback (a
  JIT-less `2_28` wheel — the interpreter fail-soft makes it functional) is the
  response, not a revert.
- The aarch64 entry moves to the **`ubuntu-24.04-arm`** runner (the platform
  `test-wheels` already uses) so the manylinux container runs natively; linking
  the LLVM archives is minutes of linker work that QEMU emulation would multiply.
  maturin-action maps `2_34` only for x86_64/i686, so the image is named
  explicitly (`quay.io/pypa/manylinux_2_34_aarch64`). The now-unused
  `Set up QEMU` step is removed.
- The maturin step gains `before-script-linux`, which maturin-action splices into
  the same shell as the `maturin` invocation, so the script's exports reach the
  build:

  ```yaml
  before-script-linux: |
    set -eo pipefail
    eval "$(bash scripts/provision-llvm23.sh --system-deps)"
    bash scripts/provision-llvm23.sh --check
  ```

- `publish-crates-io` gains a provisioning step before the verify. It resolves
  the script the same way that job already resolves the audit tooling (tagged
  tree, else `origin/main`), so a resume of a tag that predates this change still
  works.
- The macOS and Windows legs gain a **fail-loud step** that names the missing
  decision instead of leaving an `llvm-sys` discovery panic an hour into the job.
  It is one line to delete when the platform decision lands.

### Not applied: the `ci.yml` gate

`release.yml`'s `ci` job calls `ci.yml`, whose rust job builds the workspace with
default features. It needs the same provisioning and does not have it. Ready to
paste as the first step after the checkout in `ci.yml`'s `rust` job (and in the
`python` job, which runs `just dev` through `just bootstrap`):

```yaml
      - name: Provision LLVM 23 (evm2 JIT link)
        run: |
          bash scripts/provision-llvm23.sh --system-deps --github-env
          eval "$(bash scripts/provision-llvm23.sh)"
          bash scripts/provision-llvm23.sh --check
```

Left unapplied here to keep this change to the release workflow; it is the
release's first gate, so it must land with (or before) this one.

## Evidence (measured on the devcontainer, 2026-10-31)

**The prebuilt cannot run on manylinux_2_28.** From the extracted 23.1.4 tarball:

| Measurement | Result |
| --- | --- |
| `bin/llvm-config` max required glibc symbol version | `GLIBC_2.34` |
| `__libc_single_threaded` (glibc 2.32+) references in `libLLVM*.a` | 250 undefined references |
| manylinux_2_28 glibc | 2.28 → cannot run `llvm-config`, cannot link the archives |
| manylinux_2_34 glibc | 2.34 → satisfies both |

**Discovery through the prefix variable works end to end.** With the official
tarball extracted to `/tmp/llvm23/prefix`:

```
$ LLVM_SYS_231_PREFIX=/tmp/llvm23/prefix just build-rust-extension
...
   Compiling llvm-sys v231.0.2
   Compiling evm2-jit-llvm v0.1.0 (https://github.com/alloy-rs/evm2?branch=main#86fa2648)
   Compiling degenbot_rs v0.6.0-alpha.19 (/workspaces/degenbot/rust/crates/shells/degenbot-python)
    Finished `dev` profile [optimized + debuginfo] target(s) in 58.69s
```

The extension links LLVM statically: `rust/target/debug/libdegenbot_rs.so` grows
to 311 MB unstripped (a pre-flip build of the same profile is 250 MB). Wheel size
is worth a look on the first release run — that growth lands in the shipped
artifact.

**The whole provisioning path works as CI will run it.** From an empty
`LLVM23_ROOT`, `scripts/provision-llvm23.sh --system-deps` installed the dev
packages, downloaded the .tar.xz, verified its sha256, extracted the subset, and
printed the exports; `eval` then put the script's `llvm-config` (23.1.4) first on
PATH, and the same tree built and linked:

| Command (under the script's exports) | Result |
| --- | --- |
| `just build-rust-extension` | `Finished dev profile … in 14.13s` (warm; 58.69s cold) |
| `just check-rust-extension-release` | `Finished release profile [optimized] … in 27.79s` |
| `just check-rust-consumer` | `Finished dev profile … in 6.03s` — the umbrella `degenbot` with `jit` in its defaults, i.e. the shape the crates.io verify builds |

`--check` reports `discovered /tmp/…/bin/llvm-config (LLVM 23.1.4)`, and stdout
carries the two `export` lines and nothing else, so `eval "$(…)"` cannot trip
over a package manager's chatter.

The failure mode without provisioning, for calibration (both reproduced here):

```
Unable to parse result of llvm-config --system-libs: /usr/lib/x86_64-linux-gnu/libzstd.a   # recorded-path repair missing
could not find native static library 'Polly'                                              # subset missing libPolly*.a
```

**Why not another source.** `apt.llvm.org` ships Debian/Ubuntu packages only
(the manylinux images are RPM-based) and its binaries carry the same glibc 2.35+
floor. EL repos carry no LLVM 23 (`llvm-toolset` is 17.x). pypa publishes no
`manylinux_2_39_x86_64` (only aarch64/riscv64), so 2.34 is the newest tag a
prebuilt LLVM 23 can live under. A source build is 45–90+ minutes of the
120-minute job budget on a 4-core runner, versus a ~2-minute download.

## The open decision: macOS and Windows wheels

Both legs compile the JIT today, and neither can be provisioned as things stand:

- **macOS universal2** needs LLVM 23 for *both* slices. The LLVM project publishes
  `LLVM-23.1.4-macOS-ARM64.tar.xz` and no x86_64 macOS build at all, and
  Homebrew's `llvm` bottles are single-architecture, so a universal2 link has no
  x86_64 toolchain to consume.
- **Windows MSVC** has a published tarball
  (`clang+llvm-23.1.4-x86_64-pc-windows-msvc.tar.xz`), but the JIT's compiled
  artifact loader is ELF-only (`object` is pulled with `features = ["read_core",
  "elf"]` in `evm2-jit-llvm/Cargo.toml`), so a Windows wheel would carry a JIT
  that cannot load a program.

Three ways forward, in ascending cost:

1. **Ship the JIT on Linux only.** Make the JIT forward target-conditional in
   `degenbot-simulation` (the `evm2-jit` dependency under
   `[target.'cfg(target_os = "linux")'.dependencies]`, the runner code and
   `shared_runner()` cfg-gated to the same predicate), so `default` builds on
   macOS/Windows link no LLVM and the wheels ship interpreter-only there. The
   schema key keeps its default (`sim_jit = true`) and the fail-soft semantics
   already handle a build with no runner: every frame interprets, no verdict
   moves.
2. **macOS: drop universal2** for per-arch wheels, and build an x86_64 LLVM 23
   from source for the Intel slice (or drop Intel). Cheapest is arm64-only, at
   the cost of Intel users on the previous wheel.
3. **Build LLVM 23 from source on both platforms** and cache it. Correct, and
   hours per cold build.

Option 1 is the one that matches the JIT's actual reach today (ELF, Linux-tested);
the other two buy a JIT on platforms where its loader cannot run.

## Operator steps

1. **Merge** `scripts/provision-llvm23.sh` and the `release.yml` change as one
   tree (no partial staging — the pre-commit formatter owns the diff).
2. **Land the `ci.yml` provisioning** (snippet above). The release's first job
   calls it; without it the pipeline stops at the CI gate.
3. **Decide the macOS/Windows question** (above) before the next `v*` tag. Until
   then those two legs fail fast with the pointer to this page.
4. **No new secrets or permissions.** Provisioning is a shell step over public
   release assets; no action pins change, and `permissions: contents: read`
   still suffices.
5. **Tag after the merge.** `workflow_dispatch` resume of an *older* tag keeps
   working for the crates leg (it takes the script from `origin/main`) but not
   for the wheel legs, whose tree predates the script.
6. **Watch on the first run**: wheel size (statically linked LLVM), the
   `build-wheels` job duration (download ~2 min + a many-minute static link), and
   runner disk (the subset extraction is ~0.6 GiB; the tarball itself ~1.9 GiB).
