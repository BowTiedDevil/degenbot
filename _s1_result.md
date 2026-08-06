# S1 done (2ISTMX) — commit 2c33a9a1

Ship: deterministic, self-contained Vyper executor artifact + rebuild/verify
pipeline for the BHL2R2 tier-3b revm-replay oracle.

- `tier3-oracle/src-executor/` — self-contained cmd_executor.vy + interfaces/
  closure; compiles byte-identically to /workspaces/executor with vyper 0.5.0a3
  (determinism verified: creation+runtime byte-for-byte identical).
- `tier3-oracle/artifacts/executor/` — creation.hex, runtime.hex, abi,
  method_identifiers, error_map (42 PCs), immutables code_layout, sha256
  manifest, README (deploy contract + finding).
- `build-tier3-executor-harness.sh` (PUBLISH=0/1), `verify-tier3-executor-
  artifact.sh` (authoritative byte check; `just verify-tier3-executor-artifact`),
  and `tier3_executor_artifacts.rs` (toolchain-free guard, in default cargo
  test path). Guard RED-verified by tampering the source; green on restore.

## Critical finding (recorded on S2 4O7BPZ + README)
Vyper 0.5.0a3 emits only a COARSE whole-file-span source map — both venom
(experimental_codegen, default) and legacy `-f source_map` give ~15k segments
that all inherit a single span, and the structured pc_pos_map collapses to one
entry. There is NO per-instruction PC->line attribution. S2 cannot "source-map
the halt PC to a cmd_executor.vy line"; it must attribute via error_map +
method delegation + direct source inspection. S2's acceptance was revised
accordingly.

## Deploy contract (for S2)
Immutables (OWNER_ADDR, WETH_ADDR, POOL_MANAGER_ADDR, WETH_DELTA_SLOT,
NATIVE_DELTA_SLOT) are deploy-time constructor args appended to creation.hex in
code_layout order (160 bytes). Deploy = create2(salt, creation.hex ++ args).
