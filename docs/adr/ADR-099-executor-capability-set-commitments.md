# ADR-099: Executor capability-set commitments — one constant-size hash, one manifest list

**Status: accepted.** Refines [ADR-098](ADR-098-executor-agnostic-production-execution.md)
D5: versioning, capability declaration, and release manifests stay an *optional
built-in* policy, and this ADR changes how that policy publishes one of its four
metadata facts. Nothing here reaches the generic execution seam.

## Context

A `cmd_executor` deployment declares four facts through four pure getters:
`release_id()`, `protocol_major()`, `call_encoding_version()`, and
`capabilities()`. The first three are each one ABI word. The fourth is a
`DynArray[uint32, 32]` — the only bounded, only variable-length, and only
redundant part of the declaration:

- **The ceiling is compiled in.** `MAX_CAPABILITY_IDS` is a Vyper constant, so
  the bound is part of the deployed identity. Raising it is a contract
  redeploy, a rebaked artifact, and a new ABI return shape for every reader. The
  ceiling moves; it never disappears.
- **The answer is a dynamic ABI return.** Its payload grows by a 32-byte word
  per capability, both sides carry a dynamic-array encoder/decoder, and the
  decoder's strictness (canonical offset, length agreement, ascending uniqueness,
  the `>32` refusal) is machinery maintained to protect a list that is already
  published, complete, and reviewed off-chain.
- **The chain is answering a question the verifier already knows the full answer
  to.** The binder holds the release manifest, which states the complete
  capability list with names, descriptions, and the opcodes each one covers. What
  the binding step actually needs is not *detail from the chain* but a compact,
  whole-set statement it can compare against that trusted document.

The set itself is small (15 IDs), append-only (an ID is never reused, so a
removed capability leaves a gap), and cheap to enumerate off-chain. That shape —
a bounded registry with an unbounded off-chain description — is exactly where a
single commitment is strongest and per-element proofs are wasted.

Two facts keep the design honest, and both are already load-bearing elsewhere in
the codebase:

- **A declared set is not implemented behavior.** A contract can declare a
  capability its code does not implement, and no metadata scheme can detect that.
  Simulation remains the only behavioural evidence (ADR-098 D3).
- **A whole-runtime hash cannot be an identity** because Vyper immutables
  (`OWNER_ADDR`, `WETH_ADDR`, `POOL_MANAGER_ADDR`, `USDC_ADDR`, `USDT_ADDR`, and
  the two precomputed V4 delta slots) make two faithful deployments of one
  release differ byte-for-byte. A hash *can* be a valid identity only for a value
  that is the same in every deployment of a release — which a compile-time
  constant is, and a deployed runtime is not.

## Decision

### D1 — The capability surface is one `bytes32` commitment

The metadata ABI keeps four getters; the fourth changes shape:

```vyper
CAPABILITIES_HASH: constant(bytes32) = 0x9cce6a2d69dbea3b4023e117492ff11aadc2e3feadb28a83faa172fc0823bcbf

@external
@view
def capabilities_hash() -> bytes32:
    """The commitment to the declared capability ID list (see the release manifest)."""
    return CAPABILITIES_HASH
```

| Getter | Signature | Selector | Returns |
|--------|-----------|----------|---------|
| `release_id` | `release_id()` | `0x43643676` | one word, NUL-padded ASCII tag |
| `protocol_major` | `protocol_major()` | `0x07417356` | one word, `uint32` |
| `call_encoding_version` | `call_encoding_version()` | `0x76c4345b` | one word, `uint32` |
| `capabilities_hash` | `capabilities_hash()` | `0xb8ca459e` | one word, `bytes32` commitment |

The getter is a compile-time constant return: no storage slot, no constructor
argument, no proxy, no `DynArray` construction, no array output encoder on that
path, and no memory allocation. It is implementable in Vyper 0.5.0b2 with the
same `return CONSTANT` shape `release_id()` already uses. The retired getter's
dispatch arm and array return path leave the runtime with it, so the deployed
size falls by that much (measured at bake time, against the EIP-170 limit it was
already far under); the two precomputed delta slots and the 224-byte immutable
tail are untouched because no getter introduces an immutable.

`MAX_CAPABILITY_IDS` ceases to exist on both sides of the boundary: the contract
no longer has a list to bound, and the strict decoder no longer has an array to
bound. The registry becomes unbounded where it matters — the manifest — and
constant-size where it does not — the chain.

### D2 — The canonical preimage: a scheme tag, then the ABI encoding of the list

The commitment is one value with one documented derivation, so that a third party
can reproduce it with standard tools and no part of this repository:

```text
commitment = keccak256( ascii("degenbot/cmd_executor/capabilities/v1") ++ abi.encode(uint32[] ids) )
```

Rules that make the preimage canonical:

1. The scheme tag is the ASCII bytes of `degenbot/cmd_executor/capabilities/v1`
   (37 bytes), emitted verbatim, unpadded.
2. `ids` is the declared list in **ascending, unique, non-zero** order — the
   order the existing manifest validation already enforces, so a manifest whose
   array is unsorted or duplicated is refused rather than silently
   re-canonicalized.
3. `abi.encode(uint32[])` is the **standard canonical ABI encoding**: one offset
   word (`0x20`), one length word, then each ID as a 32-byte big-endian
   zero-padded word. The preimage is therefore `37 + 64 + 32 * n` bytes.
4. The list is never empty: a release declares at least one capability (today
   `address_table` and `execution_receipt` are unconditional requirements).

The chosen encoding is the exact byte sequence the retired `capabilities()`
getter returned, which makes the cutover auditable: for an unchanged capability
list, the new commitment is the keccak of bytes a v1 deployment already answered.
It is also self-delimiting (the length word makes the list boundary part of the
preimage) and reproducible from any ABI toolchain:

```bash
IDS='[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]'
PREIMAGE=$(cast concat-hex \
  0x646567656e626f742f636d645f6578656375746f722f6361706162696c69746965732f7631 \
  "$(cast abi-encode 'f(uint32[])' "$IDS")")
cast keccak "$PREIMAGE"
# 0x9cce6a2d69dbea3b4023e117492ff11aadc2e3feadb28a83faa172fc0823bcbf
```

```python
from eth_abi import encode
from eth_utils import keccak

keccak(b"degenbot/cmd_executor/capabilities/v1" + encode(["uint32[]"], [ids]))
```

Committed goldens, used by the tests below:

| ID list | Commitment |
|---------|-----------|
| `1..15` (the current release) | `0x9cce6a2d69dbea3b4023e117492ff11aadc2e3feadb28a83faa172fc0823bcbf` |
| `1..48` (the "more than 32" fixture) | `0xbd17dd15fa96f3e11b9115d7b2103cb5157d7f46937fd698225b2644e85ca12b` |
| `1..14` (one capability short) | `0x2bcf4f84999f182f49be5ae72cb0cb28adc521846426870a7e0de0587666d10c` |

Two properties the design depends on, stated explicitly:

- **The preimage is injective over the declared list.** The scheme tag fixes the
  type and the context, the ABI length word fixes the element count, and the
  fixed-width elements fix the boundaries, so two different ID lists cannot
  produce one preimage. Any change to the list — one added capability, one
  removed capability — changes the commitment (see the third golden).
- **The commitment binds the list and nothing else.** It does not cover the
  release tag, the protocol major, or the call-encoding version. Those are
  independent facts checked independently by the other three getters; folding
  them in would make tag-level and list-level disagreements indistinguishable and
  would prevent a v1 answer from being compared against a v2 manifest.

### D3 — The manifest owns the complete list; the commitment is validated, not trusted

The release manifest (`schema_version: 2`) replaces its flat `capabilities`
array with one block:

```json
"capabilities": {
  "scheme": "degenbot/cmd_executor/capabilities/v1",
  "commitment": "0x9cce6a2d69dbea3b4023e117492ff11aadc2e3feadb28a83faa172fc0823bcbf",
  "ids": [
    { "id": 1, "name": "address_table", "description": "...", "provides": ["..."] }
  ]
}
```

- `scheme` is the preimage prefix verbatim, so the manifest states the derivation
  it was built with. Validation refuses any other string: the scheme is a
  constant of the format, not a free-text note.
- `commitment` is the expected on-chain word, written out so an operator can
  compare a deployment against the document without running this build. It is
  *derived-regenerable*: `ReleaseManifest::validate` recomputes the commitment
  from `ids` and refuses a manifest that disagrees
  (`ManifestError::CapabilityCommitment { declared, derived }`), so the two
  halves check each other and a hand edit of either one — the word or the ID
  list — is refused instead of shipped.
- `ids` keeps the existing per-capability `deny_unknown_fields` declarations
  (id, name, description, `provides`) and the existing ascending/unique/non-zero
  rule (`ManifestError::UnorderedCapabilities`), plus a new non-empty rule.

The list is unbounded. `MAX_CAPABILITY_IDS` retires with
`MetadataDecodeError::TooManyCapabilities`; a manifest declaring 48 or 4,096
capabilities validates, and the on-chain surface stays one word.

On the Rust side, the on-chain declaration type changes and the loss is bounded:

| Today | After |
|-------|-------|
| `ExecutorMetadata.capabilities: CapabilitySet` (decoded from chain) | `ExecutorMetadata.capabilities_commitment: CapabilityCommitment` |
| `CapabilitySet`, `decode_capabilities`, `MAX_CAPABILITY_IDS` | deleted |
| `Release::declared_capabilities()` (from manifest) | unchanged |
| `required_capabilities`, `missing_capabilities`, `describe_capabilities` | unchanged |

`CapabilityCommitment` is a `bytes32` newtype owned by `degenbot-executor`, with
hex `Display` and no arithmetic, no ordering, and no `Default`. Capability-aware
planning never reads it: planning already compares the *manifest's* declared set
with the encoder's requirement set, and binding already proved the deployment
agrees with that manifest. The commitment makes that proof cheaper; it does not
become a planning input.

### D4 — Decode, classify, and what an RPC failure means

The existing discovery classification is unchanged in kind, because it is about
`eth_call` outcomes rather than payload shape:

| `eth_call` outcome | classification | why |
|--------------------|----------------|-----|
| return data, non-empty | `GetterOutcome::Answered` | the deployment declared something |
| return data, empty (`0x`) | `GetterOutcome::Absent` | no code, or a fallback returning nothing, declares nothing |
| EVM revert (`execution reverted`) | `GetterOutcome::Absent` | an unknown selector on a contract without a fallback reverts |
| transport / node failure | `DiscoveryError::Node` | a timeout, a rate limit, or a dead endpoint proves nothing about the deployment |

The decoder is exact and separates **syntax** from **agreement** — the split that
keeps a wrong declaration a refusal rather than a crash:

| `capabilities_hash()` return data | result |
|---|---|
| exactly 32 bytes, non-zero | `CapabilityCommitment` (a syntactically valid claim) |
| 0 bytes | `GetterOutcome::Absent` → governed by the classification table |
| 31 or 33 bytes | `MetadataDecodeError::Length { getter: "capabilities_hash", expected: 32 }` |
| 32 zero bytes | `MetadataDecodeError::Zero { getter: "capabilities_hash" }` — the ID list is never empty, so no valid commitment is zero |
| 32 bytes that name a different list | **not a decode error**: the word is well formed, so the refusal is `ReleaseMismatch::CapabilityCommitment` at binding |

Every 32-byte word is syntactically a commitment, so there is no range check and
no high-byte check. Uniformity with the other three getters is preserved:
`GetterObservations` still carries four outcomes and `answered_count()` still
counts four.

`MetadataDecodeError::PartialAnswers { answered }` gains the absent getter names
so a partially declaring deployment is diagnosable (`capabilities_hash()` absent
is the pre-cutover signature — see D5). The arm itself is unchanged: one to three
answers is an error, never a fallback, and never a legacy-mapping rescue.

### D5 — Hard cutover: `cmd_executor/2`, one metadata ABI, no dual getter

Replacing a getter changes a deployment's declared identity, so it is a release
change:

| Fact | `cmd_executor/1` | `cmd_executor/2` |
|------|------------------|------------------|
| `release_id()` | `cmd_executor/1` | `cmd_executor/2` |
| `protocol_major()` | `1` | `1` — unchanged |
| `call_encoding_version()` | `1` | `1` — unchanged |
| capability getter | `capabilities()` → `uint32[]` | `capabilities_hash()` → `bytes32` |
| Encoder bytes | one release encoding | the *same* encoding |

The tag bumps because the metadata ABI is part of what a deployment declares: two
deployments answering `cmd_executor/1` with different getter sets would make the
tag useless as a dispatch key. The protocol major and the call-encoding version
stay put because the **wire did not move** — the command stream, the packed
config layout, and the `execute` ABI including the receipt are byte-identical, so
no strategy, golden, or encoder changes. A metadata-only bump that dragged the
encoding version along would claim a wire change that did not happen.

The cutover is total; there is no compatibility layer:

- The contract source deletes `capabilities()` and `MAX_CAPABILITY_IDS` and adds
  `capabilities_hash()` and `CAPABILITIES_HASH`. The named `CAPABILITY_*` ID
  constants stay: they are the source's human-auditable declaration of the set,
  compile-time values that emit no code, and the gates in the acceptance list
  require them to agree with the manifest and with the hash constant.
- `releases/cmd_executor.v1.json` is deleted and `releases/cmd_executor.v2.json`
  is added; `RELEASE_ID_TAG` moves to `cmd_executor/2`, `RELEASE_MANIFEST_V1`
  becomes `RELEASE_MANIFEST_V2`, and the production registry holds exactly one
  release. `SUPPORTED_PROTOCOL_MAJORS` is untouched (`[1]`).
- No dual getter, no "try the hash, fall back to the array", no decoder that
  accepts either answer shape, no second registry entry kept for old deployments.
  Two metadata shapes in one build is the compatibility layer this project
  refuses: it makes every reader's correctness depend on which shape it happened
  to see.
- Artifacts rebake in the same change: `tier3-oracle/artifacts/executor/*` (ABI,
  creation, runtime, error map, method identifiers, and the provenance
  manifest's sha256 pins), `contracts/cmd_executor_abi.json`,
  `contracts/cmd_executor_bytecode.txt`,
  `contracts/cmd_executor_runtime_bytecode.txt`. `cmd_executor.immutables.json`
  keeps its `code_layout`: the getters introduce no immutable.

**Migration, for a deployment already on chain.** The encoder did not change, so
the operator action is a redeploy from the new artifact — same constructor,
128 bytes of deploy arguments, same command stream, same receipt — plus a new
`DeploymentId` at the new address. A `cmd_executor/1` deployment answers three of
four getters under this build, which is
`MetadataDecodeError::PartialAnswers { answered: 3, absent: ["capabilities_hash"] }`
→ `BindingError::Decode`: it is refused loudly at binding, before anything is
encoded, with the absent getter named. That refusal is the migration path's
boundary, not a compatibility gap: an operator who must keep driving a v1
deployment keeps the build that declares it, and this build never guesses
(ADR-087: deleting the reader is the retirement).

**Legacy mapping behavior is unchanged and unreachable from a declaration.** The
mapping names *getterless* deployments only: zero of four getters answered plus
an operator entry yields `DeploymentRelease::Legacy { release }`; zero answers
and no entry yields `Unknown`; a mapping entry never overrides a declaration
(four answers always classify `Declared`), and it never *rescues* a partial one.
A v1 deployment is therefore not addressable through the mapping — three answers
is a contradiction to investigate, not silence to fill in.

### D6 — Binding refuses an unknown release or a disagreeing commitment

Binding keeps its manifest-first order: `release_id()` selects a registry entry
(an unknown tag is `BindingError::UnknownRelease`, listing the tags this build
dispatches), then the selected release checks the declaration. The capability
check becomes a word comparison:

```rust
ReleaseMismatch::CapabilityCommitment {
    release: String,
    declared: CapabilityCommitment,   // derived from the manifest
    observed: CapabilityCommitment,   // read from the deployment
}
```

The diagnostic carries both words and the release tag; the manifest path and its
ID list are named in the message, because the chain no longer offers a list to
diff. A hash mismatch is strictly less informative than the retired set diff and
strictly safer to produce: it needs no array decode, cannot be short-circuited by
an unfamiliar ID, and detects a **superset** declaration (an extra capability)
exactly as it detects a subset.

Every other refusal stands unchanged: unsupported protocol major,
tag/major/encoding-version disagreement, malformed answers, and a node failure
that is never folded into absence. All of them fire at binding, before an encoder
is chosen and before any calldata exists.

### D7 — Operator verification: the hash is a screen, never implementation proof

**A matching commitment is not proof of implementation**, and the asymmetry is
the whole point:

- The preimage is a small, public, guessable set. Forging a match costs nothing:
  a contract that returns the correct constant and implements a drainer in
  `execute()` matches perfectly. There is no secret, no signature, and no proof
  of possession anywhere in this scheme.
- The commitment authenticates *agreement between a deployment's claim and a
  committed document*. It says nothing about the code that answers it.
- Immutability removes the usual escape hatch: there is no proxy slot, no
  implementation pointer, and no admin to ask. The bytes at the address are the
  authority (ADR-098 D3), so verification has to reach the bytes.
- Therefore **a mismatch is dispositive and a match is a screen**. A wrong word
  proves the deployment is not the one the operator means, in one `eth_call`. A
  right word proves only that the deployment is *worth verifying further*.

The operator workflow, ordered so the checks metadata cannot fake come first:

1. **Pin the build.** Record the git commit and confirm artifact integrity:
   `just verify-tier3-executor-artifact` recompiles the git-tracked
   `contracts/vyper/cmd_executor.vy` with the pinned Vyper 0.5.0b2 into a
   throwaway directory and byte-compares the result against the committed
   artifacts, while `tier3-oracle/artifacts/executor/manifest.json` pins each
   artifact's sha256 to its tracked source. Nothing here reads the chain.
2. **Verify deploy-time provenance.** Fetch the deployment transaction: `input`
   must equal the artifact's **creation code** (identical for every deployment,
   because constructor arguments are appended rather than compiled in) followed
   by `abi.encode(weth, pool_manager, usdc, usdt)` — 128 bytes — and the
   receipt's `contractAddress` must be the deployment address. This step depends
   on no metadata getter and on no post-deploy state.
3. **Reconcile the runtime bytes.** `eth_getCode(address, block)` must equal the
   artifact runtime (code section + CBOR metadata, which pins the compiler
   version and metadata hash) **followed by** the 224-byte immutable tail:
   `OWNER_ADDR` (the deployer), the four role addresses in declared offset order,
   and the two delta slots, recomputed as
   `keccak256(abi.encodePacked(executor_address, currency))` for WETH and for
   native ETH. Offsets come from `cmd_executor.immutables.json`; a mismatch here
   is a different implementation, not a different deployment argument.
4. **Screen with metadata.** `capabilities_hash()` must equal the manifest's
   derived commitment, and `release_id()`, `protocol_major()`, and
   `call_encoding_version()` must equal the manifest's declarations. This is the
   cheap address-typo and wrong-artifact filter — and it tells the operator
   *which* document to compare against when step 3 already disagrees.
5. **Prove behaviour.** Run one exact-block **representative simulation** of the
   deployment's own address through the production path: the built-in adapter
   encodes a representative path to exact `to`/`data`/`value`, the engine
   executes the deployed code (no injected build), and Assess decodes the
   `ExecutionReceipt`. This is the only step that observes what the code does. It
   is mandatory for every release certification and every new deployment, because
   steps 1–4 verify *artifacts and claims*, and behaviour lives in neither.

Steps 1–3 can only be satisfied by code; step 4 is what the commitment actually
earns; step 5 cannot be replaced by any of them.

### D8 — What the commitment does not establish, and who is excluded

- **Declared capability is not implemented capability.** A release can declare
  `v4_batch` and ship a broken `V4_BATCH` handler; the commitment, the manifest,
  and binding all agree, and the path still reverts. Metadata verifies the
  *declaration*; only simulation and audit verify the contract behind it. This
  ADR changes how a declaration is published, never how it is trusted.
- **The generic seam is untouched.** `degenbot-execution` gains no commitment
  type, no metadata-getter requirement, and no release field.
  `ExecutionAdapter` still carries `to` + `data` + `value` plus Assess (ADR-098
  D2/D4), and `degenbot-execution` still must not depend on `degenbot-executor`.
  `CapabilityCommitment` lives in `degenbot-executor` beside `ReleaseId`.
- **A metadata-free custom contract stays first class.** A foreign executor
  exposes no getters and takes part in no manifest, no commitment, no release
  registry, and no capability policy; it binds through the same production path
  it always did. The foreign-target certification (a contract with no
  `release_id()`, `protocol_major()`, `call_encoding_version()`,
  `capabilities_hash()`, WETH, or PoolManager) is part of the acceptance set, and
  it is the proof that this policy did not leak into the generic layer.
- **No proxy, upgradeability, or migration assumption** enters the scheme: the
  commitment describes one immutable deployment and nothing about its future.

## Consequences

- Metadata reads drop from one dynamic-array answer to one word, and the on-chain
  surface stops having a capability ceiling. The runtime loses the retired
  getter's array encoder.
- The release list becomes unbounded in the only place that must be: the
  manifest.
- Binding refuses with less forensic detail on a capability disagreement: both
  words, the release tag, and the manifest path, instead of two ID lists. The
  operator diffs against the manifest and can screen candidate lists (a missing
  ID, an extra ID) by recomputing the commitment with the D2 recipe.
- One new release (`cmd_executor/2`) with an unchanged wire, one rebake, one
  deleted manifest, one deleted decoder, and one deleted contract constant.
- A deployment still running `cmd_executor/1` is refused at binding by this
  build. That refusal is the intended, legible boundary of a hard cutover.
- The verifier's trust anchor stays where it was: the committed manifest and the
  committed artifacts, both hash-pinned, reviewed, and reproduced by the pinned
  toolchain — not the chain's claim about itself.

## Alternatives considered

| Alternative | Why it does not fit as well |
|---|---|
| **Raise `MAX_CAPABILITY_IDS`** (32 → 64, 128, …) | Moves the ceiling instead of removing it: every raise is a redeploy, a rebaked artifact, a new ABI return shape, and a new decoder bound. It also keeps the growing dynamic return and its strict-decoder machinery. |
| **Pagination** (`capabilities_length() -> uint32` plus `capabilities_at(uint32) -> uint32`) | Constant size per read and unbounded in principle, but verification becomes `1 + n` dependent calls whose *whole-set* property is assembled client-side. Extra capabilities beyond the count are invisible, an interleaved `eth_call` against a different block or endpoint is undetectable, and every verifier re-implements an accumulator. It reintroduces the dynamic-array decoder it was meant to delete, as a loop. |
| **Indexed membership** (`has_capability(uint32) -> bool`) | Answers only the questions asked. Capability-aware planning's check is set equality against the manifest precisely because an *undeclared extra* capability is a mismatch worth refusing; membership queries cannot observe extras, so the check silently weakens to "every ID I know about is present". It also needs one call per candidate ID and cannot be answered offline. |
| **Bitmap pages** (`capability_bitmap(uint256) -> bytes32`) | The closest competitor: O(1) per read, enumerable, sparse-friendly, and one word for today's 15 IDs. But the page index is a new ceiling (256-bit pages mean a page bound, which is pagination again), sparse IDs waste reads, and a verifier must still assemble the whole set to detect extras. It buys enumeration nobody needs at the cost of a second ABI convention. |
| **Merkle root + per-ID proofs** (`capabilities_root()` plus `capability_proof(uint32)`) | The only variant that makes per-ID membership provable from the chain alone. It is also unnecessary here: the whole set is authenticated by one word against a document the verifier already trusts, and the verifier *has* the set. The cost is a proof verifier in the client and a tree shape in the contract's build, for a trust property nobody is missing. |
| **Hashed per-feature IDs** (each capability named by `keccak256(name)` instead of a `uint32`) | Solves a naming problem the manifest already solves, and changes the encoder's requirement vocabulary and diagnostics (`CapabilityId`) for no verification gain. The stable integer ID is what lets a build name a capability a release does *not* declare, and it keeps preimages short. |
| **No on-chain capability metadata at all** (the manifest or config supplies the set) | Cheapest surface, and `release_id()` would still select an encoder — but binding would then validate nothing about the set, and a wrong deployment address (or a stale artifact at the right address) would encode silently. The commitment is the one word that turns "we believe" into "the deployment agrees". |
| **Dual getters** (`capabilities()` kept beside `capabilities_hash()`) | A compatibility layer by another name: two true sources, readers branching on payload shape, and a decoder that must stay correct for a shape nothing should produce. This project designs standalone changes without backwards-compatibility layers; the cutover deletes the old surface in the same change that adds the new one. |

## Acceptance

The design is implemented when these pass. Each name is an acceptance
criterion, and each one fails on the defect it describes.

**Manifest and commitment (`degenbot-executor` suite)**

1. `the_manifest_commitment_is_derived_from_the_declared_ids` — the committed
   `cmd_executor.v2.json` derives exactly `0x9cce…bcbf`, pinned as a golden.
2. `a_manifest_commitment_that_disagrees_with_its_ids_is_refused` —
   `ManifestError::CapabilityCommitment` for a hand-edited word.
3. `a_capability_list_above_the_retired_bound_validates` — a 48-ID manifest
   validates and derives `0xbd17…a12b`, proving the ceiling is gone off-chain and
   that no decoder kept the bound.
4. `the_commitment_is_order_sensitive_and_refuses_an_unsorted_declaration` —
   canonicalization: an unsorted or duplicated manifest array is
   `ManifestError::UnorderedCapabilities`, never silently sorted; adding,
   removing, or replacing one ID changes the commitment (goldens 1/2/3).
5. `the_capability_scheme_is_the_declared_preimage_prefix` — a manifest naming
   any scheme other than `degenbot/cmd_executor/capabilities/v1` is refused.

**Decoding and classification**

6. `decode_capabilities_hash_rejects_short_long_and_zero_words` — 31 and 33
   bytes are `Length`, 32 zero bytes are `Zero`, exactly 32 bytes decode
   verbatim.
7. `a_well_formed_but_wrong_commitment_is_a_mismatch_not_a_decode_error` — the
   D4 split: agreement is the release's check, not the decoder's.
8. `the_getter_classification_keeps_absence_and_failure_apart` — extended to the
   new getter: empty return and revert are `Absent`, transport failure is
   `DiscoveryError::Node`.
9. `partial_answers_name_the_getters_that_stayed_silent` — three of four (with
   `capabilities_hash` absent) reports the absent getter: the shape a pre-cutover
   deployment produces.
10. `the_metadata_getter_selectors_match_their_signatures` — the four-entry
    selector table equals `keccak256(signature)[0..4]`, including `0xb8ca459e`
    for `capabilities_hash()`.

**Binding and refusal (`degenbot-arbitrage` suite)**

11. `a_deployment_whose_commitment_disagrees_with_the_release_is_refused` —
    `ReleaseMismatch::CapabilityCommitment`, including a superset declaration
    (one extra ID) and a subset (one missing ID).
12. `an_unknown_release_tag_is_refused_without_trying_the_newest_encoder` —
    unchanged refusal, re-pinned against the v2 registry.
13. `the_metadata_cutover_moved_no_encoder_byte` — the per-release goldens (exact
    `to`/`data`/`value` and receipt shape) keep their committed constants under
    the new tag.
14. `a_metadata_free_foreign_contract_still_binds_and_encodes` — the generic-seam
    exclusion proof: no getters, no manifest, no commitment.
15. `a_getterless_deployment_is_named_only_by_the_operator_mapping` — the legacy
    arm unchanged: zero answers plus an entry is `Legacy`, zero answers alone is
    `Unknown`, and a mapping never overrides four answers.

**Contract (`tests/executor/test_release_metadata.py`, offline Ape suite)**

16. `test_capabilities_hash` — the deployed getter returns the manifest's
    commitment.
17. `test_capability_commitment_matches_the_source_declaration` — recomputes the
    commitment from the `CAPABILITY_*` constants in
    `contracts/vyper/cmd_executor.vy` and requires agreement with the source's
    `CAPABILITIES_HASH` constant and with the manifest's `ids` and `commitment`.
    This is the three-way drift gate that keeps the auditable source list honest.
18. `test_capabilities_hash_getter_is_a_zero_input_view_bytes32` — ABI shape in
    the committed artifact, not only in the source.
19. `test_manifest_agrees_with_the_deployed_contract` — extended to the
    commitment alongside the other three getters.

**Artifact and code verification**

20. `just verify-tier3-executor-artifact` byte-compares the rebaked artifact
    against a fresh compile of the tracked source with the pinned Vyper.
21. `tier3_executor_artifacts.rs` (toolchain-free) keeps the artifact ↔
    provenance sha256 pins and the immutable layout stable.
22. `an_artifact_from_another_release_is_refused_for_injection` — the D7
    asymmetry in test form: an injected build must be the release's own artifact,
    so a matching metadata declaration can never smuggle in different code for
    simulation.

## Implementation notes

- `contracts/vyper/cmd_executor.vy` — replace the getter and the hash constant;
  keep the named ID constants.
- `degenbot-executor/src/release.rs` — `CapabilityCommitment`, the
  `schema_version: 2` manifest block, commitment derivation, the new
  `ManifestError` arm, `decode_capabilities_hash`, `PartialAnswers` absent-getter
  names, `ReleaseMismatch::CapabilityCommitment`; delete `decode_capabilities`,
  `CapabilitySet`, `MAX_CAPABILITY_IDS`, `TooManyCapabilities`, and the decoder's
  `UnorderedCapabilities` arm; rename `RELEASE_MANIFEST_V1` to
  `RELEASE_MANIFEST_V2`.
- `degenbot-executor/src/discovery.rs` — the fourth `MetadataGetter` and the
  `capabilities_hash` answer field.
- `degenbot-executor/releases/cmd_executor.v1.json` deleted;
  `cmd_executor.v2.json` added.
- `degenbot-arbitrage/src/release_binding.rs` — the mismatch variant's
  construction and message.
- Artifacts and the committed ABI/runtime copies rebaked;
  `docs/executor/releases.md` and `docs/executor/README.md` updated (getter
  table, strict-decode table, refusal table, versioning policy, runbook).
