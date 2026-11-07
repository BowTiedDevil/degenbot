# evm2-jit

Experimental [JIT] and [AOT] compiler for the [Ethereum Virtual Machine][EVM].

The compiler implementation is abstracted over an intermediate representation backend and integrates with evm2's interpreter runner.

![image](https://github.com/user-attachments/assets/96adf64b-8513-469d-925d-4f8d902e4e0a)

The compiler backend is abstracted behind a trait ([`evm2-jit-backend`]), with an [LLVM] implementation ([`evm2-jit-llvm`]) providing full test coverage.

[JIT]: https://en.wikipedia.org/wiki/Just-in-time_compilation
[AOT]: https://en.wikipedia.org/wiki/Ahead-of-time_compilation
[EVM]: https://ethereum.org/en/developers/docs/evm/
[LLVM]: https://llvm.org/
[`evm2-jit-backend`]: /crates/evm2-jit-backend
[`evm2-jit-llvm`]: /crates/evm2-jit-llvm

## Requirements

- Latest stable Rust version

### LLVM backend

- Linux or macOS, Windows is not supported
- LLVM 23
  - On Debian-based Linux distros: see [apt.llvm.org](https://apt.llvm.org/)
  - On Arch-based Linux distros: `pacman -S llvm`
  - On macOS: `brew install llvm@23`
  - The following environment variables may be required:
    ```bash
    prefix=$(llvm-config --prefix)
    # or
    #prefix=$(llvm-config-23 --prefix)
    # on macOS:
    #prefix=$(brew --prefix llvm@23)
    export LLVM_SYS_231_PREFIX=$prefix
    ```

## Usage

The compiler is implemented as a library and can be used as such through the `evm2-jit` crate.

A minimal runtime is required to run AOT-compiled bytecodes. A default runtime implementation is
provided through symbols exported in the `evm2-jit-builtins` crate and must be exported in the final
binary. This can be achieved with the following build script:
```rust,ignore
fn main() {
    evm2_jit_build::emit();
}
```

## Testing

```bash
cargo test -p evm2-jit-runtime
cargo cli
cargo st
```

The full EEST runner lives in `evm2-eest`. See the repository-level `AGENTS.md`
for fixture setup and the `cargo nextest run -p evm2-eest --test eest --ignore-default-filter`
command.

## Credits

- [`paradigmxyz/jitevm`](https://github.com/paradigmxyz/jitevm) for inspiring the initial compiler implementation.
- [gigahorse-toolchain](https://github.com/nevillegrech/gigahorse-toolchain) for static analysis ideas.
- [Solidity](https://github.com/ethereum/solidity) and [Vyper](https://github.com/vyperlang/vyper) compilers for opcode semantics and optimization references.
