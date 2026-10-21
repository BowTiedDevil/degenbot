# Release runbook: one tag, three registries

A release is a tag, cut from `main`, that publishes the same version to three
surfaces in a fixed order. This page is the operational half of that pipeline:
what to do before tagging, what a healthy run looks like, and how to finish a run
that died. The pipeline itself lives in `.github/workflows/release.yml`.

## The contract

| Order | Surface | Job | Owns |
| --- | --- | --- | --- |
| 1 | — | `validate-branch` | the tag is an ancestor of `main` |
| 2 | built artifacts | `build-wheels`, `build-sdist` | 4 platform wheels + sdist, Rust pinned to `rust-toolchain.toml` |
| 3 | crates.io | `publish-crates-io` | 31 publishable members, dependency order |
| 4 | PyPI | `publish-to-pypi` | the artifacts from step 2 |
| 5 | GitHub Releases | `github-release` | artifacts, Sigstore bundles, the "release completed" signal |

`test-*` tags stop after step 2 and publish only to Test PyPI.

Two policies bind every release:

- **One version literal** — `[workspace.package] version` in `rust/Cargo.toml` is the
  only version in the tree (ADR-009). Tag, workspace literal, crates.io SemVer, and
  the wheel's PEP 440 form all derive from it. The crates job fails closed before any
  upload when the tag and the literal disagree, or when a `[workspace.dependencies]`
  sibling literal has drifted.
- **No release without verify** — every crate is packaged and built from its tarball
  before upload (ADR-020). The verify pass covers the members still pending, never the
  already-published ones.

Tags are PEP 440 (`v0.6.0a15`); crates.io stores SemVer (`0.6.0-alpha.15`). Post
releases (`v0.6.0a1.post1`) are PyPI-only and fail closed at the crates leg.

## Cutting a release

```bash
git switch main && git pull
just bump-version 0.6.0-alpha.16      # lockstep: workspace literal + Cargo.lock
just crates-io-pending --version 0.6.0-alpha.16   # must list ALL 31 members
just test                             # or at minimum `just pre-push` before pushing
git tag v0.6.0a16 && git push origin v0.6.0a16
```

The audit line before tagging is the cheap guard against re-releasing a version the
registry already holds: members it does *not* print are live, and live members cannot
be uploaded again.

If the release carries a Rust schema change, do the schema ritual first
(`RUST_SCHEMA_VERSION` → `RUST_MIGRATIONS` → fixture matrix; see the `bump-version`
section of the `justfile`).

## What a healthy run looks like

| Step | Expect |
| --- | --- |
| `Audit the live set` | `live=0 pending=31` for a fresh version; no failures |
| `Pre-publish verification` | `verifying 31 pending member(s); 0 already-live member(s) excluded`, then a ~5–10 min build |
| `Publish` | `=== round 1: 31 pending at <ver> — … ===` then `=== publish complete at <ver> ===` |

A version-bump release is a PublishUpdate to crates.io (1 publish/min, burst 30), so
with 31 members one round may hit the bucket:

```
rate limited (retry-after Tue, 21 Oct 2026 18:24:00 GMT) — sleeping 240s, then resuming 1 member(s)
```

That is the loop working, not a failure. A brand-new crate *name* moves at 1 per
10 minutes instead, which is why the loop can wait for hours.

## Resuming a run that died

Publishing is a sequence of one-way registry writes. An attempt that dies partway
leaves a prefix of the workspace permanently live at that version, and no re-tag can
undo that — the registry rejects the second upload of a version it already holds.

**The rule: resume the tag, never move it.**

```bash
gh workflow run release.yml --ref main -f tag=v0.6.0a15
```

A dispatch runs the workflow file from the *dispatched* ref against the *tagged*
tree, so a fixed pipeline can finish a tag whose own commit carries a broken job.
The crates leg re-reads the live set from the registry at the top of every round and
publishes only what is missing; the PyPI and GitHub Release legs skip or overwrite
what an earlier attempt already accepted (`skip-existing`, `--clobber`,
create-or-reuse).

Per-leg playbooks, keyed by the message the run prints:

| Message | Meaning | Action |
| --- | --- | --- |
| `round N: crates.io already holds part of this version — resuming at the remaining members` | a previous attempt published a prefix | nothing — the next round audits and continues |
| `rate limited (retry-after …) — sleeping …` | crates.io bucket exhausted | nothing — wait |
| `::error::crates.io asks for a Ns wait, past the 10800s publish budget — re-run this workflow to resume at: …` | the limit outlasted the job budget | re-dispatch the same tag |
| `::error::the crates.io index could not be read — refusing to guess which members are live` | index outage | retry later; the leg never guesses |
| `::error::the index never reflected the last uploads (pending set unchanged across 3 audits)` | the audit disagrees with the registry | inspect the log tail; do not re-tag |
| `::error::origin/main…` / `neither the tagged tree nor main carries scripts/crates_io_pending.sh` | the dispatched ref predates the release tooling | merge the tooling, then re-dispatch |
| PyPI `HTTP 400 File already exists` | an upload raced the audit | the `skip-existing` guard covers a re-run; upload the missing files by hand only if the run cannot |
| `release vX already exists — reusing it` | a previous attempt created the Release | nothing |

Budget and wait knobs live on the publish step: `CRATES_PUBLISH_BUDGET_SECONDS`
(default 10800), `CRATES_PUBLISH_RETRY_BUFFER_SECONDS` (default 120),
`CRATES_PUBLISH_RETRY_FLOOR_SECONDS` (default 60).

## Verifying a release

```bash
# 1. every member live at the version (silence = complete)
just crates-io-pending --version 0.6.0-alpha.15

# 2. the Python surface
curl -s https://pypi.org/pypi/degenbot/json \
  | python3 -c 'import json,sys; print(sorted(f["filename"] for f in json.load(sys.stdin)["releases"]["0.6.0a15"]))'

# 3. the release page and its assets (5 artifacts + 5 sigstore bundles)
gh release view v0.6.0a15 --json assets --jq '.assets[].name'

# 4. the two consumers
cargo add degenbot@0.6.0-alpha.15          # resolves every sibling at the same version
uv pip install 'degenbot==0.6.0a15'        # wheels + abi3 extension import
```

Step 4 is the one that catches a *mixed* release. A caret requirement such as
`^0.6.0-alpha.14` accepts any later pre-release of `0.6.0`, so a version published to
the internal crates but not the facade makes `cargo add degenbot` resolve an old
facade against new internals — a graph built from two different commits. That is why a
half-published version is worse than an unpublished one, and why the crates leg audits
before it uploads instead of trusting its own bookkeeping.

`uv`'s `exclude-newer` supply-chain cutoff hides fresher releases; override it once
with `UV_EXCLUDE_NEWER=<date>` when smoke-testing a release you just cut.

## Local tooling

```bash
just crates-io-pending                     # members NOT yet live at the workspace version
just crates-io-pending --version X --live  # members already live
just crates-io-pending --classify          # "<live|pending> <member>" per line
just publish-dry-run                       # the same dry-run verify CI runs (~20–40 min cold)
```

`scripts/crates_io_pending.sh` reads the crates.io **sparse index** — the source
`cargo publish` itself gates on. It is not `cargo search`: that answers from a separate
search backend which lags publication and rate-limits per IP, so a member published
minutes ago can read as absent there. The script exits non-zero (2) when the index
cannot be read, and every caller treats that as "unknown", never as "not published".
`CRATES_IO_INDEX_URL` points it at a mirror or a local fixture.

## Dispatch permissions

`gh workflow run` needs **Actions: Read and write** on a fine-grained token. The
similarly named **Workflows** permission only governs editing files under
`.github/workflows`, and a token carrying just that returns
`HTTP 403: Resource not accessible by personal access token`. A classic token needs
`repo` (plus organization SSO authorization where that applies). The same event from
the Actions tab of the repository needs no token at all.

## Do not

- Re-tag a version that is already published. Resume it with a dispatch.
- Move or delete a release tag to "start over"; the tag is the provenance of what was
  published.
- Publish a version twice. Registries refuse it, and the refusal aborts the batch.
- Publish with `--no-verify`, or hand-publish a crate outside this pipeline.
- Assume yanking cleans up. Yanking hides a version from new resolutions; it does not
  free the number, and the crates remain.
