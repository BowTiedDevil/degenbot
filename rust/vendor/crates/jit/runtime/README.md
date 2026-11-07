# evm2-jit-runtime

Runtime JIT/AOT backend for evm2-jit.

This crate owns the runtime compilation infrastructure: compiled-program lookup, resident-code management, artifact storage, background worker scheduling, AOT preloading, and evm2 execution wrappers.

It builds on `evm2-jit-codegen` for compilation and is re-exported by the umbrella `evm2-jit` crate.

## Custom hosts

Set `RuntimeConfig::host` to `HostConfig::for_types::<T>()` before creating a backend for a custom `EvmTypesHost`. This selects the concrete message layout and monomorphized host builtins. A runner checks the host type, the active gas schedule and the opcode table before executing compiled code; a mismatch falls back to the interpreter. `for_types` expects the base opcode table.

A host with its own opcode table uses `HostConfig::for_config::<T>(&execution_config)`. Compiled code then charges the table's static gas and runs the host's own instruction for overridden `SLOAD`, `SSTORE`, `TLOAD`, `TSTORE`, `BALANCE`, `EXTCODESIZE`, `EXTCODECOPY`, `EXTCODEHASH`, `BLOCKHASH`, `SELFBALANCE` and `LOG0` to `LOG4`. An override must keep the opcode's stack effect, must not shrink memory and must not change the program counter; compiled code halts with a fatal error if it does. A contract that uses an opcode the host adds, or overrides any other opcode, stays on the interpreter. The backend serves only frames whose execution config has the same opcode table.

Chains with custom opcode semantics can replace the builtin resolver with unsafe `HostConfig::with_resolver`. The resolver must preserve each builtin's C ABI and use the same host type as the configuration. Configure matching gas parameters as well. Custom hosts support in-process and out-of-process JIT, without persistent artifact storage or AOT: function pointers and custom layouts are process-local. An out-of-process helper compiles against the host's layout and opcode table, which the runtime sends at startup, and the runtime links the compiled object's builtins to the addresses of the host type.
