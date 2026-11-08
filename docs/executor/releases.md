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

## Binding a deployment: discovery, validation, refusal

A binding is the deployment's identity plus the release this build will encode
against. It is resolved once per deployment, before anything is encoded:

```rust
use degenbot_arbitrage::{BoundCmdExecutor, CmdExecutorBinding, read_metadata};
use degenbot_executor::release::LegacyReleaseMapping;
use degenbot_executor::release_registry::ReleaseRegistry;

let registry = ReleaseRegistry::production()?;                 // the bounded set
let binding = CmdExecutorBinding::new(owner, executor, weth, pool_manager, opts);
let bound = BoundCmdExecutor::discover(                            // over RPC
    binding,
    &provider,
    Some(&LegacyReleaseMapping::new(&legacy_entries)),             // optional
    &registry,
)
.await?;
let adapter = bound.adapter();                                     // the release's encoding
```

`BoundCmdExecutor::bind` is the same step from already-read answers, so an
offline consumer (a replay, a capture, a fixture) binds without a provider.
`read_metadata` is the only part that talks to a node.

### What a failed read means

The classification is deliberate, because it decides whether a deployment
enters the legacy path:

| `eth_call` outcome | classified as | why |
|--------------------|---------------|-----|
| return data | an answer | the deployment declared something |
| empty return data (`0x`) | **absent getter** | no code, or a fallback that returns nothing, declares nothing |
| EVM revert (`execution reverted`) | **absent getter** | an unknown selector on a contract without a fallback reverts |
| transport / node failure | **binding error** (`BindingError::Node`) | a timeout, a dropped connection, a rate limit, or a node that does not serve `eth_call` proves nothing about the deployment |

A node failure is **never** folded into "getterless": doing so would let a
broken endpoint push a metadata-declaring deployment into the legacy path,
where only an operator mapping (or a refusal) is left.

### What binding refuses

Nothing defaults to the newest release this build knows. Every one of these is a
typed error, raised at binding, before any encoding:

| Situation | Error |
|-----------|-------|
| no getter answered and no legacy mapping names the address | `BindingError::Unknown` |
| an unknown `release_id()` tag | `BindingError::UnknownRelease` (the refusal lists the tags this build dispatches) |
| a protocol major outside the registry's bounded set | `BindingError::Mismatch { mismatch: UnsupportedProtocolMajor }` |
| `call_encoding_version()` disagreements with the manifest | `BindingError::Mismatch { mismatch: CallEncodingVersion }` |
| a capability set that disagrees with the manifest (including a capability ID the release does not declare) | `BindingError::Mismatch { mismatch: Capabilities }` |
| a protocol major or release tag that disagrees with the manifest | `BindingError::Mismatch { mismatch: ProtocolMajor / ReleaseId }` |
| partial, short, non-canonical, or malformed answers | `BindingError::Decode` |

An unknown capability ID is **preserved** by `CapabilitySet` (it is never
rejected for being unfamiliar) but it is **not** support: a deployment declaring
an ID its release does not declare fails the equality check above.

### Offline and current-release consumers

`CmdExecutorBinding::new(...)` remains the explicit current-release mapping: the
session already names the release by construction, so `binding.adapter()` uses
this build's `cmd_executor/1` encoding and discovers nothing. It cannot silently
choose another release. A deployment whose release must be *validated* from its
own declaration binds through `BoundCmdExecutor`, whose adapter carries the
selected release and plans capabilities against it.

## The bounded release registry

`ReleaseRegistry` is the set of releases this build dispatches — the bound, made
explicit:

- `ReleaseRegistry::production()` holds exactly the releases compiled into this
  build. Today that is `cmd_executor/1` and nothing else. A production build
  does **not** invent a second protocol major.
- `ReleaseRegistry::with_releases(...)` is the explicit extension point: another
  major under development, or a test that certifies selection. It refuses two
  releases claiming one tag (`ReleaseError::DuplicateRelease`) and a release
  whose encoding names a different tag than its manifest
  (`ReleaseError::EncodingReleaseId`).
- `ReleaseRegistry::majors()` is the bounded major set deployment metadata is
  validated against. A registered tag with an unregistered major is a refusal,
  not an upgrade.
- `Release::new(manifest, supported_majors, encoding, development_runtime)`
  validates the manifest against the majors the caller declares. `parse_within`
  is the same bound for a manifest read from JSON; `parse`/`validate` keep the
  production bound (`SUPPORTED_PROTOCOL_MAJORS`).

A release selects the COMPLETE call encoding — command stream, packed config
semantics, and the `execute` ABI with its receipt (`ReleaseEncoding::encode`,
`ReleaseEncoding::decode_receipt`). The three are versioned and selected
together, because a stream that grew a command means nothing to an encoder that
cannot pack the matching config.

Two deployments may run one release: a deployment and an implementation release
are different facts, and each deployment keeps its own `DeploymentId` (ADR-098
D4).

## Capability-aware planning

An encoder states the capability IDs its call needs (`ReleaseCall::requires`,
derived by `required_capabilities` from the path's pool families and the encode
axes); a release states the IDs it declares. Planning is the subtraction, and a
call that needs an undeclared capability is declined:

```rust
Err(ComposeError::Unsupported(
    "release test_executor/2 does not declare erc6909_claims (capability:10); this path needs it",
))
```

What the requirement set contains:

| Requirement | When |
|-------------|------|
| `address_table`, `execution_receipt` | every call (the stream opens with the table, and the receipt is the evidence) |
| `v2_swaps` / `v3_swaps` / `v4_swaps` | one per pool family present in the path |
| `v4_batch` | only a pure-V4 path with the `use_v4_batch` axis |
| `erc6909_claims` | an ERC6909 profit capture |
| `config_bribe` | the config carries a bribe |

A decline is **per candidate**: `ComposeError::Unsupported` is a routine refusal
that the first-compatible walk advances past, and in an all-target fan-out it is
that lane's own `encode-failed` bucket. One target's missing capability never
suppresses another target's execution. A hard `ComposeError::Encode` failure is
that candidate's own unrecoverable error — it is never reclassified as a
capability decline.

## Artifacts: deployed code, and an injected build

- **Production simulates deployed code.** `BoundCmdExecutor::deployment(id)`
  declares no code override: the engine executes what the chain holds at the
  deployment's address.
- **An injected build uses the release's own artifact.**
  `BoundCmdExecutor::injected(id)` installs the selected release's runtime
  artifact at the deployment's executor address (nowhere else — the adapter
  encodes to that address).
  `BoundCmdExecutor::injected_with_runtime(id, bytes)` accepts caller-supplied
  bytes only when they ARE that artifact; any other release's code is refused
  (`BoundRuntimeError::Mismatch`), because simulating another implementation's
  code under this deployment would produce evidence the deployment never gave.
- **Where the bytes come from.** A release definition that ships its artifact in
  memory (an injected test release, an undeployed build) answers from memory.
  Otherwise the committed DEPLOYMENT artifact is read through the one
  executor-runtime precedence chain (`contracts/cmd_executor_runtime_bytecode.txt`,
  overridable by the operator's artifact keys). The manifest separately names the
  COMPILER artifact (`artifact.runtime_code`), which
  `just verify-tier3-executor-artifact` compiles and byte-compares; it is
  referenced there, never injected.

## Versioning policy

- **Additive getter** (a new `@external @view`) — additive ABI surface. Existing
  getters keep their selectors and values; nothing else moves.
- **New capability** — a new unused ID, appended to `capabilities()` and named in
  the manifest. Never reuse or renumber an ID. The stable ID → name catalog
  (`degenbot_executor::release::capability`) is what lets diagnostics name a
  capability a release does *not* declare.
- **Any change to the command stream, the packed config layout, or the execute
  ABI/receipt** — one release bump: a new `RELEASE_ID`, a new
  `CALL_ENCODING_VERSION`, a new manifest file, and (when the wire format is
  incompatible) a new `PROTOCOL_MAJOR`. The three parts are versioned together
  on purpose: a stream that grew a new command means nothing to an encoder that
  cannot pack the matching config.
- **Multi-major support** grows `SUPPORTED_PROTOCOL_MAJORS` and adds one manifest
  *plus one `ReleaseEncoding`* per major, registered in
  `ReleaseRegistry::production()`. The bounded set, the per-release manifests,
  and the per-release encodings are the only extension points; no generic crate
  changes (ADR-098 D4/D5).

## Adding a future major (runbook)

1. **Contract.** Change the Vyper source under a new `PROTOCOL_MAJOR` and bump
   `call_encoding_version()` in the same edit: they are one release.
2. **Capability IDs.** Add new IDs; never reuse or renumber an existing one.
   Add the ID to the stable catalog (`release::capability`, including its
   `name()` arm) — the catalog is release-independent.
3. **Manifest.** Add `rust/crates/foundation/degenbot-executor/releases/cmd_executor.vN.json`
   describing the whole call encoding. Insert it into the registry's production
   set; extend `SUPPORTED_PROTOCOL_MAJORS` in the same change.
4. **Encoding.** Implement `ReleaseEncoding` for the new release (stream, config,
   `execute` ABI, receipt) and pair it in `Release::new`. The registry refuses a
   mismatched pairing, so the two halves cannot drift.
5. **Artifacts.** Re-bake the tier3 artifacts so `artifact.*` paths are real, and
   pin them through the existing verification gate.
6. **Goldens.** Add the new release's per-release golden (exact `to`/`data`/
   `value`) and its receipt-shape test beside
   `degenbot-arbitrage/tests/release_binding.rs`. The existing release's goldens
   stay untouched: one release's bytes never move because another landed.
7. **Do not** add version, capability, or release fields to a generic crate. The
   generic seam stays `to` + `data` + `value` plus Assess (ADR-098 D2/D4).

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

### Operator runbook: naming a getterless deployment

1. Ask the deployment: if all four metadata calls revert or return no data, it
   is getterless. A node error is **not** an answer — retry against a healthy
   endpoint before concluding anything.
2. Decide which release the deployment actually runs. That decision is the
   operator's; nothing in this repository infers it, and a wrong guess would hand
   the deployment an encoder its contract cannot execute.
3. Declare it:

   ```rust
   let legacy = LegacyReleaseMapping::new(&[LegacyReleaseEntry {
       address: deployed_executor,
       release: ReleaseId::from_ascii_tag("cmd_executor/1")?,
   }]);
   let bound = BoundCmdExecutor::discover(binding, &provider, Some(&legacy), &registry).await?;
   ```

4. Binding refuses a mapping whose release tag this build does not dispatch
   (`BindingError::UnknownRelease`) and refuses a deployment that answers
   *partially* — a half-declaring deployment is neither declared nor getterless,
   so investigate rather than fall back.

## Gates

```bash
just test-executor-offline                     # contract tests (Ape, offline)
just verify-tier3-executor-artifact            # compile-vs-use, needs vyper + solc
just test-rust-nextest -p degenbot-executor    # strict decoders, manifest validation, the bounded registry
just test-rust-nextest -p degenbot-arbitrage   # validating binding + per-release goldens + capability planning
just lint-rust-check lint-comment-hygiene fmt-check lint-markdown-check
```

The certification suite is
`rust/crates/engine/degenbot-arbitrage/tests/release_binding.rs`: it binds two
DEFINED releases (production `cmd_executor/1` and an injected
`test_executor/2`) beside a metadata-free foreign contract, asserts each release's
exact `to`/`data`/`value` and receipt shape, and pins every fail-closed case
above. `rust/crates/foundation/degenbot-executor/tests/release_registry.rs` covers
the registry bound, the capability catalog, and the getter classification.
