# Rejected JIT experiments

Experiments that did not pay off, so you don't repeat them blindly. Add new
ones here with the numbers that rejected them.

## Codegen

- Keeping the remaining gas in a local (`alloca`) synced with `ecx.gas` around
  calls: x86 turns `sub qword ptr [ecx], imm; jb` into `cmp`/`lea` on a
  register that stays live everywhere, so spills went up 72%.
- `preserve_most` stubs for host builtins (`SLOAD`, `CALL`, ...): fewer spills
  in some contracts, many more reloads in others, about neutral overall.
- Folding the memory offset range check into the resize branch: LLVM merges
  the conditions back into the same compare chain and the cold path grows.
- A never-taken entry edge into the dynamic jump table, which makes the
  function-wide loops irreducible: misses loops closed by internal-function
  return switches and raised compile cost by up to 50% on some contracts.
  `-sink-insts-to-avoid-spills` handles these loops instead.
- Storing the value last stored to a stack slot where codegen reloads the slot
  only to copy it to another slot, which LLVM does with a 256-bit vector load
  that stalls store forwarding from the four 64-bit stores: spills +57%,
  reloads +32%, JIT size +2.2% for 0.7% fewer runtime instructions. Using the
  stored value for every read of the slot also adds instructions; both save
  cycles in some Solidity loops and cost them in others. The `noimplicitfloat`
  function attribute, which keeps those copies in general-purpose registers,
  made a Solady string loop 13–50% slower in cycles.
- Dropping `relieve_vstack_pressure`, so the virtual stack only stores at
  section boundaries and builtin calls: i256 stores -23%, but JIT size +13%,
  spills ×6 and runtime instructions +9.6% (onchain_lm_v2 +13.5%).
- Making `single_error = false` the default, so the JIT reports the
  interpreter's halt reason instead of `OutOfGas`: `opt.s` changed in 23/31
  benchmarks, JIT size +4.0% total, compile instructions +10.7% total (geomean
  +6.1%) and runtime instructions +3.8% total (geomean +0.5%; `onchain_lm_v2`
  +4.3%, `burntpix` +4.2%, `snailtracer` +2.8%).
- Branch weights on the `MRESIZE` check (`then` block cold): moving the resize
  call out of line made `opt.s` change in 28/31 benchmarks, JIT size
  +1.8% total (geomean +1.3%), spills +3.9%, compile instructions +0.8% and
  runtime instructions +0.8% (cycles +1.4%).

## Optimization passes

- `early-cse<memssa>`: forwards stack loads across blocks but raises register
  pressure; erc20_transfer spills went from 12 to 87.
- More load forwarding of any kind: `gvn` (opt time ×67 on the mainnet corpus,
  spills ×3), `newgvn` (spills +78–94%, JIT size +2.7–4.1%), and
  `-available-load-scan-limit=24`, which forwards within blocks in
  `instcombine` and `jump-threading` (JIT size +1.0–1.6%). Reloading a stack
  slot after a call or store is cheaper than keeping an i256 in four registers
  across it.
- Dropping passes whose codegen effect looks neutral: the `simplifycfg` before
  the first `instcombine`, the `simplifycfg` and `instcombine` after LICM, and
  the last `instcombine` cut opt instructions by 19% at flat JIT size, but
  runtime instructions moved with register allocation: burntpix +1.4% to
  +5.8%, onchain_lm_v2 +4.1% without the last `instcombine`.
- `constraint-elimination`: JIT size -0.1% to -0.3% for 20–32% more opt time.
- LICM at every function size: opt instructions +75% on the mainnet corpus,
  JIT size +0.4%, reloads +12%. No LICM at all: JIT size -0.1%, but hash_10k
  runtime +1.2%.
- LICM at every size, and the old gate of 4000 blocks summed over the module (the
  largest function's block count plus two, on every benchmark). The gate is now
  `LICM_MAX_BLOCKS` in `optimize_module`: 1000 blocks in the largest function.
  Sweep: the pipeline with and without the LICM group (`EVM2_JIT_PASSES`) on the 31
  benchmarks and on the 1371 distinct contract codes of 32 bytes or more in the
  pre-state of `data/mainnet-25347446-25347455.bin.zst`, with compile instructions
  (`perf stat`, median of 3) and JIT size per contract, binned by the largest
  function's block count in `unopt.ll`. The last column is the mean per contract:

  | Largest function (blocks) | Contracts | Compile instructions | JIT size | Extra instructions |
  | --- | ---: | ---: | ---: | ---: |
  | ≤250 | 111 | +0.8% | +3.2% | 0M |
  | 251–500 | 33 | +3.5% | -0.0% | 4M |
  | 501–1000 | 91 | +5.2% | +0.5% | 12M |
  | 1001–2000 | 164 | +8.0% | +0.9% | 37M |
  | 2001–4000 | 217 | +8.8% | +1.0% | 87M |
  | 4001–8000 | 298 | +10.3% | +1.1% | 216M |
  | >8000 | 457 | +20.2% | +0.7% | 1170M |

  The extra cost per block rises from about 15k instructions at 500–1000 blocks to
  30–36k at 2000–8000 and 80k above 8000, so it is mildly super-linear and steepest
  in the largest functions. It is not a cliff: snailtracer (8007 blocks) pays +37%
  compile instructions, seaport (17620) +32%, balancer_vault (20019) +82%.

  Runtime instructions per execution with LICM, on the benchmarks: fibonacci -8.3%
  and factorial -7.8% (43 blocks), hash_10k -1.0% (133), bswap64 +0.9% and
  bswap64_opt +1.2% (about 250), every other function of at most 261 blocks within
  ±1%. From 897 blocks up (22 benchmarks) the median is +0.02% and the range -1.7%
  (balancer_vault) to +1.2% (curve_stableswap); burntpix (1248 blocks) and
  snailtracer (8007), the two compute loops in that range, change by +0.2% and
  +0.04%. JIT size is 0.5–1.1% larger with LICM above 500 blocks.

  So LICM pays only on loops in small functions. Above 1000 blocks it costs 8% or
  more compile instructions and about 1% more code for no measured runtime gain.
  Up to 1000 blocks it costs 3.5–5.2% (at most 12M instructions per contract) and
  stays: the benchmarks have no function between 262 and 896 blocks, so the
  runtime effect there is unmeasured, and a cutoff of 500 would save at most that
  5.2% on 91 contracts. Across the corpus the new gate saves almost no total
  compile cost against the old one (+0.0% over no LICM at all, against +0.7%,
  since the largest functions dominate the total) but removes the 8–9% cost from
  the 381 contracts with 1001–4000 blocks.
- `bdce`, `aggressive-instcombine`, `mldst-motion`, `dfa-jump-threading`, and
  `simplifycfg` with `switch-range-to-icmp`, `switch-to-arithmetic`,
  `forward-switch-cond` or `no-keep-loops` change at most one benchmark.
  `sink`, `reassociate`, a second `jump-threading` and
  `correlated-propagation`, and
  `simplifycfg<hoist-common-insts;sink-common-insts>` move JIT size by less
  than 0.5% for more opt time.
- DSE takes a quarter of opt time, but dropping it grows JIT size by 1%;
  `-dse-memoryssa-walklimit=30` saves 6% of opt instructions for +0.05% size.

- `separate-const-offset-from-gep` at the end: spills -12% and reloads -6% on
  the corpus, but JIT size grows on 33 of 132 contracts and runtime is flat.
  `memcpyopt`, which merges adjacent zero stack stores into a `memset`: JIT
  size -0.2% for +0.3% opt instructions, runtime flat.
- `gvn-sink`: JIT size -0.8% but spills +9%. `gvn-hoist` costs 85% more opt
  time, `load-store-vectorizer` grows JIT size by 6%, `loop(indvars)` raises
  reloads by 13%, and `-jump-threading-across-loop-headers` by 3%.
- `div-rem-pairs`, `speculative-execution`, `infer-alignment`,
  `vector-combine`, `callsite-splitting`, `dce`, `lower-constant-intrinsics`,
  `sroa<preserve-cfg>`, `simplifycfg<switch-to-lookup>`, the SimplifyCFG phi
  folding thresholds, and the DSE partial-store options change nothing;
  `slp-vectorizer`, `slsr`, and `nary-reassociate` change at most 8 contracts
  for 3–16% more opt time. `mem2reg` in place of `sroa` produces the same code
  for 1% fewer opt instructions.

## `llc` flags

- `-grow-region-complexity-budget=1000` raised snailtracer reloads 2.2×, and
  `-disable-machine-licm` changed 12 of 21 benchmarks with seaport JIT size
  +1.6%; `-sink-insts-to-avoid-spills` fixes the function-wide loops better.
- `-x86-cmov-converter=false` churns (big18 JIT size +2%, extra_663c2110
  -4.9%); `-enable-misched=false` raises reloads by 15%;
  `-misched-regpressure=false` raises spills by 4–9%;
  `-enable-tail-merge=false` grows code by 3.4% and
  `-combiner-store-merging=false` by 1%; `-pre-RA-sched=source` changes
  nothing.
- `-disable-lsr`: reloads -10% on the corpus, but fibonacci-calldata JIT size
  +8% and runtime +3.9%. On the function-wide loops LSR mostly adds
  loop-simplify blocks and LCSSA phis. `-lsr-drop-solution`,
  `-lsr-insns-cost=false`, `-lsr-exp-narrow`, and the LSR limits change little.
- `-disable-constant-hoisting`: ConstantHoisting keeps the saturated memory
  offset 2^63-1, which needs `movabs`, in a register across much of the
  function. Turning it off saves 0.8% of llc instructions and 0.1% of JIT
  size, but reloads move both ways: seaport +25%, hot47 +103%, hot12 -69%.
- `-jump-is-expensive` merges the gas and stack checks into one branch through
  `setcc`/`or`: reloads -18% and spills -10% on the corpus, but burntpix and
  curve_stableswap runtime +1.6–1.7% in every run.
- `-enable-machine-outliner`: JIT size -11.5%, runtime +1.2% geomean and
  onchain_lm_v2 +8.4%.
- `-fast-isel` grows JIT size by 4.5%, `-global-isel` falls back to
  SelectionDAG, and `-regalloc=basic` grows JIT size by 19%.
- Codegen level, with the IR pipeline fixed: `-O1` differs from `-O2` only by
  the post-RA scheduler, which X86 enables for some CPUs. On znver5 `-O1` gives
  the same code as `-post-RA-scheduler=false` and is what every level above
  `None` uses. Against `-O3` it cuts compile instructions by 5.5% and spills
  by 0.4% for 0.3% more JIT size and flat runtime (instructions -0.1%, cycles
  +1.6% on a loaded machine, then +0.1% and -0.1%), but the JIT size of
  push0_proxy grows 6.4%. On graniterapids, the CI runner, `-O1` and `-O2`
  give identical assembly. `-O2` and `-O3` produce the same code to 0.1% in
  JIT size and compile instructions, so `-O3` buys nothing. `-O0` grows JIT
  size by 71%, spills ×27, and runtime instructions by 16%, for 41% fewer
  compile instructions.
- `-enable-ext-tsp-block-placement`: JIT size -0.3% for 56% more llc time.
  `-combiner-global-alias-analysis`: llc +3.4% for no smaller code.
- Turning off `machine-cse`, `branch-fold`, block placement, `machine-cp`, the
  scheduling model, or post-RA machine sinking saves up to 1.7% of llc
  instructions but grows code by up to 0.6%; turning off `machine-sink`,
  `peephole`, stack slot coloring, or early tail duplication costs both, and
  turning off CodeGenPrepare nearly doubles spills.
  `-x86-machine-combiner=false` raises reloads by 8%.
- Flags that move spills and reloads both ways across contracts with little
  change in JIT size: `-split-spill-mode=size`,
  `-twoaddr-reschedule=false`, `-misched-prera-direction=bidirectional`,
  `-combiner-topological-sorting`, `-min-jump-table-entries=8` (JIT size
  -1.4% from fewer jump tables, reloads +0.9%), `-tail-dup-size`,
  `-tail-merge-size=1`, `-tail-dup-placement=false`,
  `-machine-sink-split=false`, `-x86-br-merging-base-cost`, and the greedy
  eviction and hint-split limits.
- No change on any contract: `-disable-early-ifcvt`, `-x86-early-ifcvt`,
  `-disable-select-optimize`, `-disable-complex-addr-modes`,
  `-disable-cgp-branch-opts`, `-enable-local-reassign`,
  `-enable-split-loopiv-heuristic`, `-terminal-rule`, `-join-splitedges`,
  `-join-globalcopies`, `-enable-shrink-wrap=false`, `-enable-post-misched`,
  `-misched-cluster=false`, and the CodeGenPrepare type-promotion and
  address-sinking options.

## Jump analysis

Measured with `evm2 run --parse-only` on the 26 contracts of
`data/mainnet-25347446-25347455.bin.zst` that stay unresolved with the folded
recursion mode (2292 dynamic jumps), and on a census of six Solar `-Ogas`
contracts (338 jumps, packed u16 jump tables).

- Making a jump to `Top` (or to a range too wide to resolve) reach every
  JUMPDEST with its state, so a converged fixpoint needs no invalidation: 2292
  -> 2275 jumps on mainnet, 338 -> 302 on Solar. The full-context fixpoint ran
  out of parts on every jump-table contract: the jump's state enters internal
  blocks with too short a stack, their returns turn `Top` and spread again, and
  the insensitive fallback leaves every jump `Top`.
- Keying the coarse mode on the top 2, 4 or 8 jump targets instead of 1, with
  the full-context budget: 2292 -> 2292 / 2280 / 2280 jumps, analysis
  instructions on those 26 contracts 14.84G -> 17.11G / 17.58G / 17.29G. The
  contexts still split on stack height, so it explodes like the full-context
  mode.
- Folded mode: folding shared return stubs (forwarders) from their 5th copy, or
  never, instead of the 3rd changes no contract. Where folding converged before
  it now hits the per-block part limit or keeps the same `Top`.
- Folded mode: `MAX_SEGS_BELOW` 128 or 1024 instead of 64 keeps the `Top` return
  through a shared stub (segments keep growing: 154 and 300 in
  0x20ca56F1...), and with 1024 the folded attempt no longer converges.
- Multiplying every full-context and folded budget by 4, 16 or 64: 2292 -> 1787
  jumps in all three cases. That is the three contracts resolved by raising
  the full-context block partition limit to 8192, plus 12 jumps of 0x50d3865a...
  that only resolve with a larger total part budget. Analysis instructions on
  the 26 contracts go 14.84G -> 76.90G at 16x and 265.02G at 64x. The other big
  contracts keep failing: their call stacks are 100-300 deep and contexts keep
  growing.
