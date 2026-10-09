YDNH3P (C1.1): ADR-069 records the reopening of ADR-060 D4's preserved entry surface.

Created `docs/adr/ADR-069-prepared-hop-entry-surface.md` (status accepted 2026-10-09; predecessors ADR-060 D4, ADR-015, ADR-059 D6). Records the settled decision that the prepared hop becomes ONE value (tick-range sequence + Arc'd crossing table + word profiles + derivation fingerprint, folded once at derivation, carried inside `ResolvedHop`), the entries take one slice of a borrowed hop view (`V2(&IntHopState) | Cl(&PreparedClHop)`), the table gate is deleted, `walk_path_fingerprint` goes crate-private behind the value's `fingerprint()`, and the memo key derives from the same prepared values the walk consumes. Registered as row 069 in `docs/adr/index.md`.

Gates: `npx --yes markdownlint-cli2 docs/adr/ADR-069-prepared-hop-entry-surface.md docs/adr/index.md` -> 0 issues in 0 files. No cargo gates (docs-only chunk).
