# Cmd executor releases

The built-in `cmd_executor` is an immutable, non-upgradeable contract, so its
versioning has to be *declared*, not inferred. This page defines the declared
release identity, the complete call-encoding description that goes with it, and
the rules for deployments that predate the declaration.

Decision records:
[ADR-098](../adr/ADR-098-executor-agnostic-production-execution.md) D5 —
versioning is an optional built-in policy, and none of it is part of the generic
`ExecutionAdapter` seam — and
[ADR-099](../adr/ADR-099-executor-capability-set-commitments.md), which replaces
the bounded capability array with one constant-size commitment. A foreign
execution contract needs no metadata getters, no manifest, and no release policy
([execution adapters](../execution-adapters.md)).

Command streams, balance semantics, and the contract's full command set are in
[the user guide](user-guide.md); this page covers what a deployment *declares*
about itself, not how to encode for it.

## The current release

| Fact | Value | Declared by |
|------|-------|-------------|
| Release tag | `cmd_executor/2` | `release_id()` |
| Protocol major | `1` | `protocol_major()` |
| Call-encoding version | `1` | `call_encoding_version()` |
| Capability commitment | `0x9cce…bcbf` (see below) | `capabilities_hash()` |
| Off-chain manifest | `rust/crates/foundation/degenbot-executor/releases/cmd_executor.v2.json` | this repository |
| Artifact | `tier3-oracle/artifacts/executor/` | `artifact.*` in the manifest |

The tag moved from `cmd_executor/1` when the capability getter changed shape
(ADR-099 D5): a deployment's metadata ABI is part of what it declares. The
protocol major and the call-encoding version did **not** move, because the wire
did not — the command stream, the packed config, and the `execute` ABI with its
receipt are byte-identical, so no strategy, golden, or encoder changed.

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
so an ID always names the same encoding feature. The list itself lives in the
manifest (`capabilities.ids`, unbounded); the deployment answers one word, the
commitment to that whole list. A reader that does not know an ID still sees it in
the manifest rather than rejecting the deployment.

### The capability commitment

```text
commitment = keccak256( ascii("degenbot/cmd_executor/capabilities/v1")
                        ++ abi.encode(uint32[] ids) )
```

The preimage is a 37-byte scheme tag followed by the canonical ABI encoding of
a `uint32[]` (offset word `0x20`, length word, one zero-padded word per ID), so
it is self-delimiting and injective over the declared list: one ID added,
removed, or replaced changes the word. The encoding is exactly the bytes the
retired `capabilities()` getter answered, which is what made the cutover
auditable.

Reproducible with no part of this repository:

```bash
cast keccak "$(cast concat-hex 0x646567656e626f742f636d645f6578656375746f722f6361706162696c69746965732f7631 \
                             "$(cast abi-encode 'f(uint32[])' '[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]')")"
# 0x9cce6a2d69dbea3b4023e117492ff11aadc2e3feadb28a83faa172fc0823bcbf
```

Committed goldens:

| ID list | Commitment |
|---------|------------|
| `1..15` (the current release) | `0x9cce6a2d69dbea3b4023e117492ff11aadc2e3feadb28a83faa172fc0823bcbf` |
| `1..48` (the "more than 32" fixture) | `0xbd17dd15fa96f3e11b9115d7b2103cb5157d7f46937fd698225b2644e85ca12b` |
| `1..14` (one capability short) | `0x2bcf4f84999f182f49be5ae72cb0cb28adc521846426870a7e0de0587666d10c` |

**A matching commitment is a screen, never proof of implementation.** The
preimage is public and guessable, so forging a match costs nothing; the word
authenticates *agreement between a deployment's claim and a committed document*,
nothing about the code that answers it. A **mismatch is dispositive** — the
deployment is not the one the operator means — and a match only means the
deployment is worth verifying further (provenance, runtime bytes, and a
representative simulation; ADR-099 D7).

## On-chain getters

Four `@external @view` functions, each returning a compile-time constant. No
storage slot, no constructor argument, no proxy, no upgrade path:

| Getter | Signature | Selector | Returns |
|--------|-----------|----------|---------|
| `release_id` | `release_id()` | `0x43643676` | one word, NUL-padded ASCII tag |
| `protocol_major` | `protocol_major()` | `0x07417356` | one word, `uint32` |
| `call_encoding_version` | `call_encoding_version()` | `0x76c4345b` | one word, `uint32` |
| `capabilities_hash` | `capabilities_hash()` | `0xb8ca459e` | one word, `bytes32` commitment |

The values are constants in `contracts/vyper/cmd_executor.vy`, so a deployment's
identity is fixed at compile time and readable by anyone, forever. The
`CAPABILITY_*` ID constants stay in the source as its human-auditable
declaration of the set; they emit no code, and the gates below require them to
agree with the manifest and with `CAPABILITIES_HASH`.

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
| `capabilities_hash()` returning 31 or 33 bytes | `MetadataDecodeError::Length { getter: "capabilities_hash", expected: 32 }` |
| `capabilities_hash()` returning 32 zero bytes | `MetadataDecodeError::Zero { getter: "capabilities_hash" }` — the ID list is never empty, so no valid commitment is zero |
| only some getters answered | `MetadataDecodeError::PartialAnswers { answered, absent }` — `absent` names the silent getters |
| a well-formed word that names a different list | not a decode error: `ReleaseMismatch::CapabilityCommitment` at binding |

Every 32-byte word is syntactically a commitment, so there is no range check and
no high-byte check: the decoder separates **syntax** from **agreement**, and
agreement is the release's decision. A capability ID the build has never heard
of is **not** an error either — the manifest carries the list, and only
`ReleaseManifest::capability` fails to name an ID.

## The release manifest

`ReleaseManifest` (committed JSON, embedded with `include_str!`) binds the
declared identity to the whole call encoding. It is deserialized with
`deny_unknown_fields` and validated on every use, so a misspelled or invented key
is a hard error rather than a silently ignored line.

`schema_version` is `2` and nothing else: a manifest written against another
schema is `ManifestError::UnsupportedSchemaVersion`, not a best-effort read. The
capability block is what schema 2 replaced — one derivation, one word, one list:

```json
"capabilities": {
  "scheme": "degenbot/cmd_executor/capabilities/v1",
  "commitment": "0x9cce6a2d69dbea3b4023e117492ff11aadc2e3feadb28a83faa172fc0823bcbf",
  "ids": [
    {
      "id": 1,
      "name": "address_table",
      "description": "The preprocessing address table plus the protocol sentinel indices.",
      "provides": ["SET_ADDRESS"]
    }
  ]
}
```

`ids` is the complete list, and it carries **no length ceiling**. The retired
`MAX_CAPABILITY_IDS` bound of 32 is gone from both sides — the contract answers
one word, and a manifest may declare 15, 48 (the suite's above-the-old-bound
fixture), or more: nothing validates a maximum, while the deployment's metadata
surface stays one `bytes32`. Validation bounds *shape*, not size: IDs ascending,
unique, and non-zero, the list non-empty, and every `provides` entry a declared
opcode (`provides` may be omitted for a capability that is a config or receipt
semantic rather than an opcode).

| Section | Binds |
|---------|-------|
| `release_id`, `protocol` | the declared tag and bounded major/minor |
| `call_encoding.version` | the joint stream + config + execute-ABI version |
| `call_encoding.max_stream_length` | the `execute(commands)` ABI bound (288) |
| `call_encoding.address_resolution` | `SET_ADDRESS` table bound (32) and the sentinel index → role bindings |
| `call_encoding.command_stream.opcodes` | every opcode: byte, name, kind (`preprocess`/`execute`/`separator`), ordered field list with types, plus count-prefixed repeats and length-prefixed tails |
| `call_encoding.config.fields` | the packed `uint256` bit layout, gapless over bits 0–255, with allowed values and ranges |
| `call_encoding.execute_abi` | function, canonical signature, selector (checked against `keccak256(signature)`), state mutability, inputs, and the receipt fields |
| `capabilities` | the derivation scheme, the commitment word, and the complete ID list — ID → name/description, and the opcodes each capability covers |
| `artifact` | the ABI/creation/runtime/immutables/error-map/method-identifier files and the provenance manifest |

Validation cross-checks, so a contradictory manifest cannot pass as current:

- `schema_version` is exactly `2` (`ManifestError::UnsupportedSchemaVersion`);
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
- the capability block's `scheme` is the format's preimage prefix
  (`degenbot/cmd_executor/capabilities/v1`) and nothing else;
- capability IDs are unique, ascending, and non-zero, and the list is non-empty;
  every opcode a capability claims is declared; there is no upper bound on how
  many IDs the list holds;
- `capabilities.commitment` is exactly the commitment the manifest's own `ids`
  derive (`ManifestError::CapabilityCommitment { declared, derived }`), so the
  word and the list check each other and a hand edit of either one is refused;
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
| a capability commitment that disagrees with the manifest (including a superset declaration — one extra ID — and a subset) | `BindingError::Mismatch { mismatch: CapabilityCommitment }` |
| a protocol major or release tag that disagrees with the manifest | `BindingError::Mismatch { mismatch: ProtocolMajor / ReleaseId }` |
| partial, short, non-canonical, or malformed answers | `BindingError::Decode` |

A capability disagreement is one word comparison against the manifest's derived
commitment. It carries less forensic detail than the retired set diff — the
chain no longer answers a list — so the diagnostic names both words, the release
tag, and the manifest's `capabilities.ids` to diff against. A mismatch is
strictly dispositive: a **superset** declaration (an extra capability) is refused
exactly as a **subset** is.

### Offline and current-release consumers

`CmdExecutorBinding::new(...)` remains the explicit current-release mapping: the
session already names the release by construction, so `binding.adapter()` uses
this build's `cmd_executor/2` encoding and discovers nothing. It cannot silently
choose another release. A deployment whose release must be *validated* from its
own declaration binds through `BoundCmdExecutor`, whose adapter carries the
selected release and plans capabilities against it.

## The bounded release registry

`ReleaseRegistry` is the set of releases this build dispatches — the bound, made
explicit:

- `ReleaseRegistry::production()` holds exactly the releases compiled into this
  build. Today that is `cmd_executor/2` and nothing else. A production build
  does **not** invent a second protocol major, and it keeps no entry for the
  retired `cmd_executor/1` metadata ABI.
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
- **New capability** — a new unused ID, added to the contract's `CAPABILITY_*`
  constants (which changes `CAPABILITIES_HASH`, hence a redeploy and a release
  bump) and to the manifest's `capabilities.ids` with its name and description.
  Never reuse or renumber an ID. The stable ID → name catalog
  (`degenbot_executor::release::capability`) is what lets diagnostics name a
  capability a release does *not* declare.
- **The capability commitment itself** — one value with one documented
  derivation. Changing the scheme string is a new derivation and therefore a new
  format version; there is no second getter and no accepted alternative shape.
- **The metadata ABI** — replacing or adding a getter is a release bump, because
  the declared identity is part of the deployment. A metadata-only bump leaves
  `call_encoding_version()` alone: it claims a wire change that did not
  happen.
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

## Migrating a deployment to `cmd_executor/2`

`capabilities()` → `capabilities_hash()` is a hard cutover, not an additive
change (ADR-099 D5). ADR-099 D2 explains why one word replaces the list and D7
explains why a match is a screen rather than proof; this section is the
operational form — what an operator runs, ordered so the checks metadata cannot
fake come first.

It applies to a built-in `cmd_executor` deployment only. A custom execution
contract needs none of it — no getters, no manifest, no commitment, and no
release bump ([execution adapters](../execution-adapters.md)).

### The migration, step by step

**1. Pin the build.** Record the commit, then confirm the artifacts are the ones
that commit ships: `just verify-tier3-executor-artifact` recompiles the tracked
`contracts/vyper/cmd_executor.vy` with the pinned Vyper 0.5.0b2 into a throwaway
directory and byte-compares the result against the committed artifacts, while
`tier3-oracle/artifacts/executor/manifest.json` pins each artifact's sha256 to
its tracked source. Nothing in this step reads the chain.

**2. Redeploy the immutable code.** The constructor is unchanged —
`__init__(weth, pool_manager, usdc, usdt)`, four role addresses, 128 bytes of
deploy arguments appended to creation code that is identical for every
deployment of the release:

```bash
DEPLOY_INPUT="$(cat tier3-oracle/artifacts/executor/cmd_executor.creation.hex)$(cast abi-encode 'f(address,address,address,address)' "$WETH" "$POOL_MANAGER" "$USDC" "$USDT")"
```

There is no proxy, no upgrade, and no migration call: the new artifact is a new
address, and the session names it with a new `DeploymentId`.

**3. Verify deploy-time provenance.** The deployment transaction's `input` must
equal `$DEPLOY_INPUT` byte for byte, and its receipt's `contractAddress` must be
the address you intend to drive. This step depends on no metadata getter and no
post-deploy state.

**4. Reconcile the runtime bytes.** `eth_getCode(address, block)` must equal the
compiler artifact runtime — code plus CBOR metadata, 18,524 B, no immutables —
followed by the 224-byte immutable tail, one 32-byte word each, in the layout
`tier3-oracle/artifacts/executor/cmd_executor.immutables.json` declares. (The
deployment artifact `contracts/cmd_executor_runtime_bytecode.txt` is those
18,524 bytes with one concrete deployment's tail already appended, 18,748 B
total; only the tail differs between two deployments of the release.)

| Offset | Word | Value |
|--------|------|-------|
| `0x00` | `OWNER_ADDR` | the deployer |
| `0x20` | `WETH_ADDR` | deploy argument 1 |
| `0x40` | `POOL_MANAGER_ADDR` | deploy argument 2 |
| `0x60` | `USDC_ADDR` | deploy argument 3 |
| `0x80` | `USDT_ADDR` | deploy argument 4 |
| `0xa0` | `WETH_DELTA_SLOT` | `keccak256(abi.encodePacked(executor, weth))` |
| `0xc0` | `NATIVE_DELTA_SLOT` | `keccak256(abi.encodePacked(executor, address(0)))` |

Addresses are left-padded to a word. A mismatch here is a different
implementation, not a different deployment argument — and no getter can settle
it, because a getter only answers what the code at that address chooses to say.

**5. Read the four getters.**

```bash
cast call "$EXECUTOR" 'release_id()(bytes32)'            # cmd_executor/2, NUL-padded
cast call "$EXECUTOR" 'protocol_major()(uint32)'         # 1
cast call "$EXECUTOR" 'call_encoding_version()(uint32)'  # 1
cast call "$EXECUTOR" 'capabilities_hash()(bytes32)'     # 0x9cce…bcbf
```

**6. Recompute and compare the commitment.** Derive the word from the manifest's
`capabilities.ids` with the recipe above; the manifest performs the same
recomputation at validation, so its `ids` and `commitment` cannot disagree
silently. A **mismatch is dispositive** — this is not the deployment the
document describes — while a **match is a screen**: it says the deployment
agrees with a public, guessable preimage, and nothing about the code behind it.

**7. Bind.** `BoundCmdExecutor::discover` resolves the tag against this build's
registry, then checks the declaration against that release's manifest. Every
refusal in the table above is typed, and none of them defaults to the newest
release:

```rust
let registry = ReleaseRegistry::production()?;
let binding = CmdExecutorBinding::new(owner, executor, weth, pool_manager, EncodeOptions::default());
let bound = BoundCmdExecutor::discover(binding, &provider, None, &registry).await?;
```

**8. Prove behavior.** Run one exact-block representative simulation of the
deployment's own address through the production path:

```rust
let deployment = bound.deployment(DeploymentId::new(1));   // deployed code, no injected build
```

The adapter encodes a representative path to exact `to`/`data`/`value`, the
engine executes the code the chain holds at that address, and Assess decodes the
`ExecutionReceipt`. Steps 1–4 verify artifacts and deployed bytes, steps 5–6
verify a *claim*, and this step is the only one that observes what the code
does. It is mandatory for every new deployment.

### What did not move

- **The protocol major and the call-encoding version.** `protocol_major()` is
  still `1` and `call_encoding_version()` is still `1`, because the wire did not
  move; a metadata-only bump that dragged the encoding version along would claim
  a wire change that did not happen.
- **The `execute()` calldata and the receipt.** Same `execute(commands, config)`
  bytes, same `ExecutionReceipt { gross_profit, bribe_paid }`; the per-release
  goldens in `degenbot-arbitrage/tests/release_binding.rs` pin both under the new
  tag.
- **Capability planning.** It still compares the manifest's declared ID list with
  the encoder's requirement set; the commitment is never a planning input.
- **The legacy mapping.** It still names only deployments that answered
  *nothing*, and still never overrides or rescues a declaration.
- **Read classification.** Empty return data and a revert are absent; a transport
  failure is `BindingError::Node` and is never folded into absence.
- **Constructor, deploy arguments, storage, and the immutable tail.** No new
  slot, no initializer, no new immutable; the two delta slots and the 224-byte
  tail are byte-identical. The runtime only lost the retired getter's dispatch
  arm and array return encoder (18,638 B → 18,524 B code + CBOR, well under the
  24,576-byte EIP-170 limit).
- **The generic seam.** `degenbot-execution` gained no commitment type, no
  metadata-getter requirement, and no release field (ADR-098 D2/D4).

### What refuses, and what cannot rescue it

- A deployment still answering `capabilities()` answers three of four getters, so
  binding is
  `MetadataDecodeError::PartialAnswers { answered: 3, absent: ["capabilities_hash"] }`
  → `BindingError::Decode`: refused loudly, before any encoder is chosen, with
  the silent getter named.
- The legacy mapping does **not** rescue it: an entry supplies a release for a
  deployment that answered nothing, and three answers is a contradiction to
  investigate, not silence to fill in. Binding also refuses a mapping whose tag
  this build does not dispatch (`BindingError::UnknownRelease`, listing the tags
  this build dispatches).
- There is no dual getter, no "try the hash, fall back to the array", no decoder
  that accepts either shape, and no registry entry for the retired
  `cmd_executor/1` metadata ABI. An operator who must keep driving a
  `cmd_executor/1` deployment keeps the build that declares it (ADR-087:
  deleting the reader is the retirement).

### Operator runbook: naming a getterless deployment

1. Ask the deployment: if all four metadata calls revert or return no data, it
   is getterless. A node error is **not** an answer — retry against a healthy
   endpoint before concluding anything. A *partial* answer set (three of four,
   typically `capabilities_hash` absent) is a pre-cutover deployment, not a
   getterless one, and the mapping below never rescues it.
2. Decide which release the deployment actually runs. That decision is the
   operator's; nothing in this repository infers it, and a wrong guess would hand
   the deployment an encoder its contract cannot execute.
3. Declare it:

   ```rust
   let legacy = LegacyReleaseMapping::new(&[LegacyReleaseEntry {
       address: deployed_executor,
       release: ReleaseId::from_ascii_tag("cmd_executor/2")?,
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
DEFINED releases (production `cmd_executor/2` and an injected `test_executor/2`)
beside a metadata-free foreign contract, asserts each release's exact
`to`/`data`/`value` and receipt shape, pins the production calldata golden so the
metadata cutover cannot move an encoder byte, and pins every fail-closed case
above — including a superset and a subset capability commitment.
`rust/crates/foundation/degenbot-executor/tests/release_registry.rs` covers the
registry bound, the capability catalog, the getter selectors, and the getter
classification; the `degenbot-executor` unit suite covers manifest validation and
commitment derivation (including a 48-ID list, above the retired bound).
`tests/executor/test_release_metadata.py` is the three-way drift gate: the
deployed getter, the source's `CAPABILITY_*` constants plus `CAPABILITIES_HASH`,
and the manifest's `ids` plus `commitment` must all agree.
