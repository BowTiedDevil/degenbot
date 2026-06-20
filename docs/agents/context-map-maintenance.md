# Context-map maintenance contract

Rules for the context-map corpus: `CONTEXT-MAP.md` (root) + module
`CONTEXT.md` files (`src/degenbot/<module>/CONTEXT.md`) + `rust/CONTEXT.md`.
These files are the project's ubiquitous-language ledger — vocabulary, not
history. Drift concentrates wherever a refactor's narrative is written into
them instead of its ADR; these rules keep the two concerns apart.

## What a context-map entry is

A definition of **what the term means now**, plus aliases to avoid, plus
(optional) one footgun note. It is **not** a changelog, design diary, or
status report. The corpus lives next to two history documents that already
own that job: ADRs (`docs/adr/`) record decisions + rationale; migration
guides (`docs/migration-guides/`) record completed refactors.

## Rules

1. **ADR, not vocabulary, for history.** When an ADR lands, the only
   permitted context-map edit is "term now means X; link to ADR-N."
   Implementation history — "formerly," "revised by ADR-N," "prior to
   ADR-N," "status: complete," "implemented in Plan N" — goes in the ADR,
   never in the vocabulary file.
2. **Status as a trailing tag, never prose.** Status, when needed, is
   `{Live | Removed}`, written as a trailing parenthetical:
   `**Foo** (Removed): ...`. No prose "Status: complete" paragraphs, no
   "Implementation status" blocks, no "deferred"/"RESOLVED" markers.
3. **Removed terms get a `## Removed terms` block.** One line each, naming
   the ADR/plan that removed them, e.g.
   `**`foo_registry`** (Removed under ADR-006 slice 8b): ...`.
   Do not scatter `*(removed)*` ghosts through the live term table.
4. **The `{Foo}` brace dialect is banned.** Use real markdown links
   (`[Term](#anchor)` or relative file paths) or plain `**Term**`.
5. **Definitions capped at ~4 sentences / ~80 words.** "Why" narratives
   belong in ADRs; a vocabulary entry states the current meaning and its
   aliases, nothing more.
6. **Cross-module relationships are seams only.** The root
   `CONTEXT-MAP.md` `## Cross-module relationships` section holds only
   relationships that span two or more modules — a bullet belongs there
   only if it names ≥2 module terms. Single-module relationships stay in
   that module's `CONTEXT.md` `## Relationships` section.

## Modules at target shape

These need no changes; new edits should imitate their size and shape
(vocabulary + aliases + ≤1 footgun, no history):

- `src/degenbot/erc20/`, `listener/`, `chainlink/`, `curve/`, `balancer/`,
  `aave/`, `uniswap/`, `types/`, `builders/`, `provider/`, `registry/`.
- `rust/CONTEXT.md` is the largest and most drift-prone; treat it as the
  exemplar of what to keep tight.

## Validation

A future `just lint-context-maps` target enforces rules 1–4 mechanically
(no brace dialect; no status-prose markers; no references to deleted
modules; relative links resolve). Rule 5 (word cap) and rule 6 (seam-only
cross-module bullets) are reviewer judgment — check them on edit.
