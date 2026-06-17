{
  "id": "3eecfb45",
  "title": "Diagnose commitlint CI failure on dev merge",
  "tags": [],
  "status": "done",
  "created_at": "2026-06-17T10:06:02.953Z"
}

Follow-up error after force-pushing the reworded dev: `Invalid revision range c4c82d39..HEAD`. Root cause: the force push orphaned the OLD dev tip `c4c82d39` (no longer reachable from any ref); `actions/checkout@v6` with `fetch-depth:0` only fetches reachable objects, so `c4c82d39` is absent on the runner → commitlint's `--from=$before` can't resolve it → throw `Invalid revision range`. Reproduced locally: `git log c4c82d39..HEAD` emits the byte-identical `fatal: Invalid revision range c4c82d39.....HEAD`.

Fix (durable): hardened `.github/workflows/ci.yml` commitlint step — when the computed `before` SHA doesn't resolve, fall back to a guaranteed-present range `HEAD~min(200,count-1)..HEAD` (HEAD history is fully fetched) that still covers the rewritten commits; plus an empty-range guard. Normal fast-forward pushes and PR behavior unchanged (guard only trips on a missing SHA). Committed as `bbc87383 fix: harden commitlint against force-pushed before SHA` (commitlint exit 0). Verified the shell logic via 3 scenarios under `bash -eo pipefail`. Push is a fast-forward (parent = origin/dev = 5ed87580); pushing it makes the next run lint only `bbc87383` (before=5ed87580 present) → green, and hardens CI against future force pushes.
