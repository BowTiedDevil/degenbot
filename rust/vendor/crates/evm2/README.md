# evm2

Fast, customizable EVM implementation in Rust.

## Highlights

- up to **2x faster than revm** from a tighter interpreter, static dispatch tables, and cheaper instruction plumbing.
- **Built for custom EVMs**: extend specs, opcodes, gas schedules, transactions, precompiles, environment data, and inspectors without reshaping the core.
- **Clean extension boundaries**: typed configuration keeps fork logic, transaction handling, host state, and opcode definitions separate while compiling down to a small execution path.

## Example

```rust,ignore
enum CustomSpecId {
    Custom,
    // ...
}

struct CustomTypes;

impl EvmTypes for CustomTypes {
    // ...
}

#[instruction(EvmTypes = CustomTypes)]
fn l1_blocknumber(cx: _) -> out {
    *out = Word::from(cx.state.host().block_env().ext.l1_block_number);
}

fn main() -> Result<()> {
    let spec_id = CustomSpecId::Custom;
    let mut evm = Evm::<CustomTypes>::new(spec_id, ..);
    let tx = CustomTx { .. };
    let executed = evm.transact(&tx)?;
    let result = executed.commit();
    // ...
    Ok(())
}
```

See [`crates/evm2/examples/custom_evm`](https://github.com/alloy-rs/evm2/tree/main/crates/evm2/examples/custom_evm) for the complete version.

## Feature flags

All features of the `evm2` crate are listed below. Use `default-features = false` to disable the default set.

| Feature | Default | Description |
| --- | --- | --- |
| `default` | Yes | Enables the features marked below. |
| `std` | Yes | Enables Rust standard library support. |
| `async` | No | Enables asynchronous host I/O through stackful coroutines; requires `std`. |
| `serde` | No | Enables serialization and deserialization with Serde. |
| `arbitrary` | No | Enables arbitrary test data generation in Alloy dependencies. |
| `account-ext` | No | Enables chain-specific account data. |
| `map-hashbrown` | No | Uses hashbrown for Alloy maps and sets. |
| `map-foldhash` | Yes | Uses foldhash as the default hasher for Alloy maps and sets. |
| `asm-keccak` | Yes | Uses the assembly Keccak implementation. |
| `sha3-keccak` | No | Enables the RustCrypto SHA-3 Keccak implementation. |
| `secp256k1` | Yes | Uses libsecp256k1 for the ECRECOVER precompile. |
| `gmp` | Yes | Uses GMP for the modular exponentiation precompile. |
| `bn` | No | Uses substrate-bn for BN254 precompiles when `bn254-mcl` is disabled. |
| `c-kzg` | No | Uses c-kzg for KZG point evaluation. |
| `blst` | Yes | Uses blst for BLS12-381 precompiles and KZG verification when `c-kzg` is disabled. |
| `bn254-mcl` | Yes | Uses MCL for BN254 precompiles; requires `std`. MCL is fastest on x86-64 CPUs with AVX, BMI2, and ADX. |
| `portable` | Yes | Enables portable builds of the enabled blst and c-kzg backends. |
| `p256-aws-lc-rs` | Yes | Uses AWS-LC for the P256VERIFY precompile. |
| `parse` | Yes | Enables parsing opcode names into `OpCode` values. |
| `nightly` | No | Enables nightly Rust optimizations. |
| `no-tco` | No | Disables automatic selection of the tail-call interpreter backend. |

## Supported Rust Versions (MSRV)

evm2 always aims to stay up-to-date with the latest stable Rust release.

The Minimum Supported Rust Version (MSRV) may be updated at any time, so we can take advantage of new features and improvements in Rust.

#### License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in these crates by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
</sub>
