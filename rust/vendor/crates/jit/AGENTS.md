# evm2-jit - EVM JIT/AOT compiler

## Commands

Use the root commands for lint, format, and docs. Targeted JIT commands:

```bash
cargo nextest run -p evm2-jit-codegen --test codegen                             # compiler tests
EVM2_JIT_TEST_DUMP=1 cargo nextest run -p evm2-jit-codegen --test codegen "test_name" # dump single compiler test
cargo nextest run -p evm2-jit-runtime                                            # runtime tests
cargo st "statetests::eest::jit"                                                 # EEST JIT state tests
cargo st "blockchain_tests::eest::jit"                                          # EEST JIT blockchain tests
```

## Architecture

- `evm2-jit` — umbrella crate that re-exports codegen and runtime APIs.
- `evm2-jit-codegen` — EVM compiler, bytecode analysis, linker, and compiler tests.
- `evm2-jit-runtime` — JIT/AOT backend, worker pool, artifact store, and evm2 integration.
- `evm2-jit-backend` — compiler backend trait; `evm2-jit-llvm` implements it.
- `evm2-jit-builtins` — runtime builtins called by compiled code (host calls, gas).
- `evm2-jit-context` — execution context types shared by evm2 and compiled code.
- `evm2-jit-build` — build-script helpers for AOT compilation.

## CLI

Do NOT use `--release`: the dev profile already uses `opt-level = 3`. Use the
`profiling` profile for symbolized optimized builds.

```bash
cargo cli list <fixture.json>             # list replay entrypoints
cargo cli replay --jit <fixture.json>     # replay through JIT (--aot for AOT)
RUST_LOG=evm2_jit_codegen=trace cargo cli replay --jit <fixture.json>
```

Dump directories hold `bytecode.txt` (parsed bytecode with blocks, gas, and
stack info), `bytecode.dot`/`.svg` (CFG), `unopt.ll`, `opt.ll`, `opt.s`, and
`remarks.txt` (compile timings and sizes).

## LLVM options

`EVM2_JIT_LLVM_ARGS` passes space-separated LLVM flags (e.g.
`-print-after-all`); only the first call takes effect, and flags that `init_`
already sets do not reliably override it. `EVM2_JIT_PASSES` replaces the pass
pipeline for every function size, dropping the LICM size gate in
`optimize_module`. `bench.py` passes both to the base and head, so make
pipeline changes in code to diff them.

Before trying a pass, flag, or codegen change, read
[EXPERIMENTS.md](EXPERIMENTS.md) for what was already rejected, and add new
rejections there.

## Dynamic jump resolution

```bash
./scripts/jit/bench.py /tmp/bench --jump-resolution [benches...]
RUST_LOG=evm2_jit_codegen::bytecode=trace ./scripts/jit/bench.py /tmp/bench --jump-resolution usdc_proxy |& rg 'jump|JUMP'
```

In the trace, `JUMP bb<N>` (or `bb<N>, bb<M>`) is resolved, `JUMP ; pc=<N>` is
unresolved, and `unresolved dynamic jumps remain n=N` gives the count.

## Benchmarking

`./scripts/jit/bench.py <dump_dir> --diff origin/main` compares codegen and
compile cost against a base revision; see `--help` for the other analyses.
Fetch first: a stale local `main` drags unrelated commits into the diff. The
output also goes to `<dump_dir>/results.md`. The base builds in
`<dump_dir>.base`, which caches it per commit; delete it and run
`git worktree prune` to drop the cache.

- `opt.s changed in N/M` counts benchmarks whose assembly changed; `0/N` shows
  a refactor leaves codegen alone.
- Judge compile cost by `instructions` (`perf stat`, about 0.3% noise), not
  `time`, which moves by more than 10% on a busy machine. The phase times in
  the details table only show which phase moved. Don't read `finalize` or
  `total` in `remarks.txt` as compile time: they include writing dumps.
- `--runtime` counts instructions per execution. It is opt-in, so pass
  `--codegen-lines --compile-times --runtime` for all three. CodSpeed has the
  final word on runtime; cite its report.
- All runs pass `--no-frame-pointers`, as release builds do. Dumps otherwise
  force frame pointers, which costs a register and adds spills.

CI runs `bench.py --output --runtime` on every PR and push to `main`, and
`bench-comment.yml` posts a sticky comment with links to
`https://www.getfoundry.sh/perf/evm2/`. Its cycle numbers vary with the runner;
compare instructions. To reproduce the comment locally:

```bash
./scripts/jit/bench.py /tmp/bench --diff origin/main --output /tmp/out/head --base-output /tmp/out/base
./scripts/jit/compare.py /tmp/out/base /tmp/out/head --pr-comment-output /tmp/out/comment.md
```

## Bench-and-PR workflow

For perf changes, or when asked to "bench and open pr":

1. Run `./scripts/jit/bench.py <dump_dir> --diff origin/main`.
2. Write the PR body: prose on what the PR does and why, then a
   `## Benchmarks` section with a short summary of the headline numbers
   (`TOTAL`/`GEOMEAN` rows and notable wins or regressions), then
   `<dump_dir>/results.md` appended verbatim. Never edit or summarize the
   tables themselves, and don't post them as PR comments.
3. Update the PR with `gh pr edit <number> --body-file <body.md>`.

```bash
{
  cat <<'EOF'
What this PR does and why.

## Benchmarks

JIT size -7.5%, compile instructions -4.1%; `counter` opt.s +26%.

EOF
  cat /tmp/bench/results.md
} > /tmp/pr-body.md
```

## Important

- NEVER delete or modify `./tmp/`: it holds hand-made IR/asm dumps
  (`tmp/dump/` from `main`, `tmp/dump2/` from the current branch).

## Code style

- Never call `.index()` on an index type just to rebuild the same type; use
  arithmetic on the index (`self.insts[term_inst - 1]`).
- Don't prefix log messages with the pass or function name; `#[instrument]`
  spans provide it (`trace!("resolved jump")`).
- Name IR values after the Rust field path they access: `<base>.<field>` for a
  loaded value and `<base>.<field>.addr` for its address, where `<base>` is the
  Rust name of the struct pointer (`ecx`, `interpreter`, `message`). Locals
  take the Rust name too (`stack_bytes.addr`). The `field!` macro builds these
  names. When a field moves or changes meaning, update its names.
- Keep the IR dumps readable: no instructions that do nothing (`get_field`
  returns the base pointer for a field at offset 0 instead of a zero GEP), and
  no names that contradict the value. Check `unopt.ll` after changing codegen.
