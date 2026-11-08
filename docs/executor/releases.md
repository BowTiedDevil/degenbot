# Cmd executor releases

The built-in `cmd_executor` is an immutable, non-upgradeable contract, so its
versioning has to be *declared*, not inferred. This page defines the declared
release identity, the complete call-encoding description that goes with it, and
the rules for deployments that predate the declaration.

Decision record: [ADR-098](../adr/ADR-098-executor-agnostic-production-execution.md)
D5 — versioning is an optional built-in policy, and none of it is part of the
generic `ExecutionAdapter` seam. A foreign execution contract needs no metadata
getters, no manifest, and no release policy.

## The current release

| Fact | Value | Declared by |
|------|-------|-------------|
| Release tag | `cmd_executor/1` | `release_id()` |
| Protocol major | `1` | `protocol_major()` |
| Call-encoding version | `1` | `call_encoding_version()` |
| Capability IDs | `1..15` (see below) | `capabilities()` |
| Off-chain manifest | `rust/crates/foundation/degenbot-executor/releases/cmd_executor.v1.json` | this repository |
| Artifact | `tier3-oracle/artifacts/executor/` | `artifact.*` in the manifest |

### Capability IDs

| ID | Name | Covers |
|----|------|--------|
| 1 | `address_table` | `SET_ADDRESS` preprocessing + sentinel indices |
| 2 | `v2_swaps` | `V2_SWAP_COMPACT`, `V2_SWAP_DIRECT` |
| 3 | `v2_swap_calc` | `V2_SWAP_CALC` |
| 4 | `v3_swaps` | `V3_SWAP_COMPACT`, `V3_SWAP_DELTA`, `V3_SWAP_ONE_FOR_ZERO`, `V3_SWAP_ZERO_FOR_ONE` |
| 5 | `v3_dynamic_amounts` | `V3_SWAP_DELTA` |
| 6 | `v4_swaps` | `V4_SWAP_COMPACT`, `V4_SWAP_DYNAMIC` |
| 7 | `v4_batch` | `V4_BATCH` |
| 8 | `v4_batch_open_weth` | `V4_BATCH_OPEN_WETH` |
| 9 | `v4_dynamic_deltas` | `V4_TAKE_DELTA`, `V4_SETTLE_DELTA`, `V4_SETTLE_ALL` |
| 10 | `erc6909_claims` | `V4_MINT_COMPACT`, `V4_BURN_COMPACT` |
| 11 | `weth_wrap_unwrap` | `WETH_DEPOSIT`, `WETH_WITHDRAW`, `WETH_DEPOSIT_ALL`, `WETH_WITHDRAW_ALL` |
| 12 | `native_transfers` | `SEND_ETH`, `SEND_ETH_ALL` |
| 13 | `config_bribe` | packed-config bribe rate + recipient |
| 14 | `config_min_profit` | packed-config gross-profit floor |
| 15 | `execution_receipt` | `execute()` returns `ExecutionReceipt{gross_profit,bribe_paid}` |

IDs are **stable and never reused**. A capability that disappears leaves a gap,
so an ID always names the same encoding feature. The on-chain getter returns IDs
only; the names live in the manifest, which means a reader that does not know an
ID keeps it verbatim instead of rejecting the deployment.

## On-chain getters

Four `@external @view` functions, each returning a compile-time constant. No
storage slot, no constructor argument, no proxy, no upgrade path:

```vyper
def release_id() -> bytes32              # NUL-padded ASCII tag, e.g. "cmd_executor/1"
def protocol_major() -> uint32           # bounded dispatch unit
def call_encoding_version() -> uint32    # versions stream + config + execute ABI together
def capabilities() -> DynArray[uint32, 32]
```

The values are constants in `contracts/vyper/cmd_executor.vy`, so a deployment's
identity is fixed at compile time and readable by anyone, forever.

### Strict decoding

`degenbot_executor::release` owns the decoders. They are exact, and every
rejection is an error value — never a default, never a panic:

| Input | Result |
|-------|--------|
| empty return data (absent getter) | `MetadataDecodeError::PartialAnswers` |
| one ABI word ≠ 32 bytes | `MetadataDecodeError::Length { expected, actual }` |
| `uint32` word with non-zero high bytes | `MetadataDecodeError::OutOfRange` |
| zero major or zero encoding version | `MetadataDecodeError::Zero` |
| `release_id` word that is not a NUL-padded printable-ASCII tag | `MetadataDecodeError::MalformedReleaseId` |
| capability array with a non-canonical offset, trailing bytes, or a length that disagrees with the payload | `MetadataDecodeError::Length` / `OutOfRange` |
| more than 32 capability IDs, or a zero ID | `MetadataDecodeError::TooManyCapabilities` / `Zero` |
| duplicate or descending capability IDs | `MetadataDecodeError::UnorderedCapabilities` |
| only some getters answered | `MetadataDecodeError::PartialAnswers { answered }` |

A capability ID the build has never heard of is **not** an error:
`CapabilitySet` preserves it, and only `ReleaseManifest::capability` fails to
name it.

## The release manifest

`ReleaseManifest` (committed JSON, embedded with `include_str!`) binds the
declared identity to the whole call encoding. It is deserialized with
`deny_unknown_fields` and validated on every use, so a misspelled or invented key
is a hard error rather than a silently ignored line.

| Section | Binds |
|---------|-------|
| `release_id`, `protocol` | the declared tag and bounded major/minor |
| `call_encoding.version` | the joint stream + config + execute-ABI version |
| `call_encoding.max_stream_length` | the `execute(commands)` ABI bound (288) |
| `call_encoding.address_resolution` | `SET_ADDRESS` table bound (32) and the sentinel index → role bindings |
| `call_encoding.command_stream.opcodes` | every opcode: byte, name, kind (`preprocess`/`execute`/`separator`), ordered field list with types, plus count-prefixed repeats and length-prefixed tails |
| `call_encoding.config.fields` | the packed `uint256` bit layout, gapless over bits 0–255, with allowed values and ranges |
| `call_encoding.execute_abi` | function, canonical signature, selector (checked against `keccak256(signature)`), state mutability, inputs, and the receipt fields |
| `capabilities` | ID → name/description, and the opcodes each capability covers |
| `artifact` | the ABI/creation/runtime/immutables/error-map/method-identifier files and the provenance manifest |

Validation cross-checks, so a contradictory manifest cannot pass as current:

- the `release_id` is a NUL-padded printable-ASCII tag;
- `protocol.major` is in this build's **bounded** set `SUPPORTED_PROTOCOL_MAJORS`
  (`[1]`), so an unknown major is a refusal rather than an implicit upgrade;
- `call_encoding.version` is non-zero;
- opcodes are unique and ascending, and each opcode's field widths sum to its
  command size;
- every variable tail names a declared `uint8` length field and a declared
  `bytes` data field, and every repeat names a declared `uint8` count field;
- config fields tile bits 0–255 exactly, with no hole and no overlap, and no
  declared value exceeds its width;
- the execute selector equals `keccak256(signature)[0..4]`;
- capability IDs are unique, ascending, and non-zero; every opcode a capability
  claims is declared;
- every artifact path is repository-relative, and the provenance file is not
  the runtime file.

`declared_opcode_sizes_match_the_encoders` keeps the manifest honest against the
Rust encoder surface: every declared opcode's declared size must equal the byte
length the corresponding `enc_*` builder produces. The two opcodes with no Rust
builder (`V3_SWAP_ONE_FOR_ZERO`, `V3_SWAP_ZERO_FOR_ONE`) are contract-only, so
their declared sizes are cross-checked against the Python encoding helpers in
`tests/executor/test_release_metadata.py` instead.

### Release identity is not artifact integrity

A release is named by its declared tag, **never** by a whole-bytecode hash.
Vyper immutables (`OWNER_ADDR`, `WETH_ADDR`, `POOL_MANAGER_ADDR`, `USDC_ADDR`,
`USDT_ADDR`, and the two precomputed V4 delta slots) are baked into the deployed
runtime, so two faithful deployments of the same release have different runtime
code. A whole-runtime hash would therefore make identity deployment-specific.

Artifact integrity and provenance stay where they belong:
`tier3-oracle/artifacts/executor/manifest.json` pins each committed artifact to
the sha256 of its git-tracked source, `just verify-tier3-executor-artifact`
recompiles and byte-compares, and the toolchain-free
`tier3_executor_artifacts.rs` guard runs in the default cargo suite. The release
manifest only *names* those files (`artifact.provenance`).

## Getterless deployments and the legacy mapping

A deployment compiled before these getters existed answers nothing. That is
**absence of evidence**, not evidence of the newest release:

```rust
match classify_deployment(address, observations, legacy_mapping)? {
    DeploymentRelease::Declared(metadata) => { /* the deployment named itself */ }
    DeploymentRelease::Legacy { release }  => { /* the operator named it */ }
    DeploymentRelease::Unknown             => { /* nobody named it: refuse to guess */ }
}
```

- **Declared** — all four getters answered and decoded strictly.
- **Legacy** — no getter answered *and* the operator's
  `LegacyReleaseMapping` has an entry for that deployment address.
- **Unknown** — no getter answered and no mapping entry. Naming it "latest"
  would hand a stale encoder a contract it cannot drive, so the model has no
  path from silence to a release.
- **Partial answers** — an error (`PartialAnswers`). A half-declaring deployment
  is neither declared nor getterless, and a legacy entry does not rescue it.

## Versioning policy

- **Additive getter** (a new `@external @view`) — additive ABI surface. Existing
  getters keep their selectors and values; nothing else moves.
- **New capability** — a new unused ID, appended to `capabilities()` and named in
  the manifest. Never reuse or renumber an ID.
- **Any change to the command stream, the packed config layout, or the execute
  ABI/receipt** — one release bump: a new `RELEASE_ID`, a new
  `CALL_ENCODING_VERSION`, a new manifest file, and (when the wire format is
  incompatible) a new `PROTOCOL_MAJOR`. The three parts are versioned together
  on purpose: a stream that grew a new command means nothing to an encoder that
  cannot pack the matching config.
- **Multi-major support** grows `SUPPORTED_PROTOCOL_MAJORS` and adds one manifest
  per major. The bounded set and the per-release manifests are the only extension
  points; no generic crate changes (ADR-098 D4/D5), and runtime target selection
  is a separate concern.

## Migration notes: the metadata ABI addition

Adding the four getters is an **additive** ABI change with no deployment-ABI
disruption:

- **Constructor** is unchanged: `__init__(weth, pool_manager, usdc, usdt)`, four
  role addresses, 128 bytes of deploy arguments.
- **Storage** is unchanged: the getters are constant returns, no new slot, no
  initializer, no migration call.
- **Runtime bytecode** grew by the four getters' dispatch + return code
  (18,638 B code + CBOR, 18,862 B deployed with the 224-byte immutable tail —
  well under the 24,576-byte EIP-170 limit).
- **Artifacts re-baked**: `tier3-oracle/artifacts/executor/*` plus the
  `contracts/cmd_executor_*` copies. Both are reproduced by
  `tier3-oracle/build-tier3-executor-harness.sh` and `contracts/recompile.py`,
  and pinned by `just verify-tier3-executor-artifact`.
- **`ExecutionReceipt` semantics are unchanged.** `execute()` still returns
  `{gross_profit, bribe_paid}` with the same measurement and bribe rules; the
  manifest's `execute_abi.returns` declares those fields so a release cannot
  quietly change them.
- **Already-deployed executors** keep running. They are getterless from a new
  client's point of view, so naming them requires an explicit legacy mapping —
  which is exactly the point of the previous section.

## Gates

```bash
just test-executor-offline                     # contract tests (Ape, offline)
just verify-tier3-executor-artifact            # compile-vs-use, needs vyper + solc
just test-rust-nextest -p degenbot-executor    # strict decoders + manifest validation
just lint-rust-check lint-comment-hygiene fmt-check lint-markdown-check
```
