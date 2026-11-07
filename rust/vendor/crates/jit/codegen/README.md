# evm2-jit-codegen

EVM bytecode compiler frontend and code generation pipeline.

This crate contains the bytecode parser and analysis passes, the generic compiler driver, linker helpers, and test utilities for producing JIT and AOT artifacts through compiler backends such as `evm2-jit-llvm`.

For the runtime worker pool and hot-code lookup backend, see `evm2-jit-runtime`. For the umbrella crate, see `evm2-jit`.
