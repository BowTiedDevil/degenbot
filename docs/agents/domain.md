# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Layout: Multi-context

This repo has a `CONTEXT-MAP.md` at the root pointing to per-module `CONTEXT.md` files under `src/degenbot/<module>/`. Each module context defines its own domain terms, relationships, and ambiguity rulings. The root map holds cross-module relationships and cross-module ambiguity rulings.

```
/
├── CONTEXT-MAP.md                     ← module index + cross-cutting content
├── docs/
│   ├── adr/                           ← system-wide decisions (not yet created)
│   └── agents/
├── src/
│   └── degenbot/
│       ├── types/CONTEXT.md
│       ├── erc20/CONTEXT.md
│       ├── registry/CONTEXT.md
│       ├── arbitrage/CONTEXT.md
│       ├── aave/CONTEXT.md
│       └── connection/CONTEXT.md
└── UBIQUITOUS_LANGUAGE.md             ← legacy (replaced by CONTEXT-MAP.md)
```

## Before exploring, read these

1. **`CONTEXT-MAP.md`** at the repo root — read it first for the module index and cross-cutting content.
2. **Per-module `CONTEXT.md`** — read each one relevant to the area you're about to work in. The map's bullet summaries help you identify which module(s) to read.
3. **`docs/adr/`** — read ADRs that touch the area you're about to work in. If per-context ADR directories appear under `src/<module>/docs/adr/`, check those too.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The producer skill (`/grill-with-docs`) creates them lazily when terms or decisions actually get resolved.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in the relevant `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/grill-with-docs`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0007 (event-sourced orders) — but worth reopening because…_
