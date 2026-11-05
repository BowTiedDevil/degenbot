---
name: adversarial-review-committee
description: Adversarial review committee — stress-test a plan, spec, or design with three persistent adversarial reviewers (correctness, sequencing, scope) and iterate rounds until every verdict is satisfied. Use when the user asks for an adversarial or red-team review of a plan, wants a plan "worked out" with persistent reviewers before implementation, or names the committee / adv-* reviewers.
---

# Adversarial review committee

Three persistent actors attack a plan artifact round after round; the PM dispositions every
finding, revises the plan, and loops until all three return `VERDICT: SATISFIED` on the same
revision. Proven shape: 3 axes × N rounds, findings in a fixed format, nothing accepted without
evidence.

## Steps

### 1. Freeze the plan into an artifact

Write the plan under review to `.scratch/<topic>-plan.md` (self-contained: claims, file/line
evidence pointers, scope per item, gates, and an explicit "not proposed" section). Reviewers
attack a file, never chat memory.

**Done when:** the file exists and every factual claim in it is checkable against the tree.

### 2. Create the three reviewers

Persistent actors via `agents.create` — one per axis:

| name | axis | what it attacks |
|---|---|---|
| `adv-correctness` | technical truth | factual claims vs the tree; would the change work; contradictions with AGENTS.md/ADRs; silent behavior changes; debt classes reintroduced |
| `adv-sequencing` | order & dependency | chunk order; shared files / FFI stubs / census pins / generated artifacts; whether one chunk invalidates another's evidence; atomic-vs-split; migration order inside items |
| `adv-scope` | scope & priority | missing work; churn dressed as depth; wrong payoff ranking; better-as-disposition items; the "explicitly not proposed" section hardest |

Give each a DIFFERENT model family when the user does not name models (same-family reviewers
miss the same things); use the user's models verbatim when named. Mailbox delivery, one shared
topic, plain-text responses:

```python
await agents.create(name="adv-correctness", instructions=<axis + protocol>,
                    topics=["team.<topic>-review"], delivery="mailbox",
                    responseMode="text", model="<family-a>")
```

**Done when:** `agents.create` returns three actor ids.

### 3. Round loop

Ask all three the same round prompt in parallel (`agents.ask(id=…, message="ROUND <n>. …")`,
wrap each in try/except before `asyncio.gather`, generous `timeout_ms`):

1. **Collect** — each returns numbered findings + `VERDICT: SATISFIED` / `VERDICT: NOT SATISFIED`.
2. **Disposition** — for EVERY finding: verify the evidence yourself against the tree (a finding
   whose evidence does not check out is rejected with the counter-evidence; "unverifiable" is a
   rejection, not a maybe). Record `accepted / rejected / deferred` + reason.
3. **Revise** — the plan file changes: fix accepted findings, add missing items, re-rank, and
   strike claims the tree disproved. Keep a short round log in the plan or alongside it.
4. **Re-ask** — reviewers attack the REVISED plan from scratch ("verify the revisions land your
   findings; find what the revision broke"). Never let a reviewer patch its own findings in place.

**Done when:** a full round ends with all three `VERDICT: SATISFIED` on the same plan revision —
or the user stops the process with the disposition log intact.

### 4. Close out

Save each round's full responses to `.scratch/adv-review-r<n>-<name>.md` (they are the audit
trail for the plan's contested points). Stop the actors (`agents.stop`), then carry accepted
findings into the implementation plan / backlog verbatim — a disposition the implementers cannot
see is a wasted round.

**Done when:** transcripts saved, actors stopped, accepted findings reflected in the work items.

## Reviewer instruction template

Send as the actor's `instructions` (axis paragraph first), then the round prompt per ask:

> PROTOCOL:
> - Return ONLY numbered findings, exact shape:
>   `FINDING [blocker|major|minor|nit] <one-line claim>` / `evidence: <file:line, command output, or reasoning>` / `fix: <concrete change>`
> - No praise, no plan summary. If nothing survives your attack: `VERDICT: SATISFIED`.
> - Otherwise end `VERDICT: NOT SATISFIED`.
> - Every claim checkable; cite the line or the command you ran. Unverifiable claims are rejected in disposition.
> - Attack content: factual errors, wrong sequencing, missing work, overreach, contradictions with repo policy, wrong payoff ranking. Not style, not wording.

## Reference

**Severity ladder** — `blocker` = the plan cannot proceed as written (wrong type location, a
dependency cycle, a false invariant); `major` = the item fails its goal or breaks something;
`minor` = gap or stale fact; `nit` = precision. Blockers are resolved before any implementation
starts; they are never argued down without tree evidence.

**Disposition rules** — the PM owns judgment; a reviewer cannot accept its own fix. Dispositions
are written down even when rejected ("rejected: evidence X disproves the claim"). When two
reviewers contradict each other, the tree decides.

**API mechanics** (fabric_exec):
- `agents.ask(id, message)` waits for the actor's next response — parallelize with guarded
  `asyncio.gather` (an unguarded exception discards sibling results).
- `agents.messages(id, limit)` reads bounded inbox/outbox history when a response is truncated.
- `tools.list` pages (default 100) and never proves absence — `tools.describe(ref=…)` on a
  candidate ref instead.
- Long rounds: set `timeout_ms` on `fabric_exec` (minutes, not seconds).
- `delivery="mailbox"` keeps the actor addressable after each round; `topics` let the three see
  each other if the user wants cross-talk.

**What the axes catch in practice** (why three): correctness finds invariant gaps and false
claims ("the constructor omits the zero-gamma bound"; "the 'empty shell' has live importers");
sequencing finds placement and dependency blockers ("the type cannot live in that crate — the
dependency edge runs the other way"; "the gate is conditional and the condition is undecided");
scope finds ranking inversions and unscheduled leftovers ("this is readability churn at rank 2
while an open defect-class question sits at rank 3"; "the survey prescribed this deletion and no
item covers it"). Any single axis misses a third of these.

**Pitfalls** — reviewers re-attacking incrementally instead of the whole revision; the PM
accepting a plausible claim without running the cited command; verdicts given as prose ("looks
good") instead of the exact token (reject them); scope creep in rounds (the artifact under review
is the plan, not the codebase's every wart).
