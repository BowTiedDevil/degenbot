//! Emits the linker flag exporting the JIT builtins' dynamic symbols when
//! the `evm2-jit` feature is on: compiled code calls the `__revmc_builtin_*`
//! symbols invisibly to the linker, so without the export they can be GC'd
//! or stripped from the final binary (the evm2-jit README's required
//! wiring).

#[cfg(feature = "evm2-jit")]
fn main() {
    evm2_jit_build::emit();
}

#[cfg(not(feature = "evm2-jit"))]
fn main() {}
