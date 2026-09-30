---
name: dispatched-agent
description: Rules for workers dispatched into the degenbot workspace by a project manager (one-shot agents, actors). Load before making any edit or running any gate when you arrived via dispatch. Covers commit restrictions, scoped test gates, foreground command discipline, and shared-workspace tree hygiene.
---

# Dispatched-Agent Lane Rules

You arrived by dispatch and work under a project manager. Your manager owns commits, the whole-workspace gates, and the definition of done. Your job is one scoped chunk of implementation work.

## Never commit or push

Commits happen only in the manager's post-settle quiet window, staged whole. Leave the tree dirty for review. A partial stage by you plus the pre-commit stash dance plus the manager's own edits aborts the commit after mutating the tree.

## Run only scoped gates

Yours:

- `cargo test -p <crate>` for crates you touched
- `uv run --no-sync pytest <the test files you touched>`

Not yours (the dispatcher runs these across all workers):

- `just test-rust-nextest`, `just test-python`, `just test-rust` and any other whole-workspace ladder
- whole-tree lint/format sweeps

Per-crate `cargo test -p <crate>` is fine inside a tight red-green loop, but do not declare workspace-wide health from it — say what you ran and what you did not.

## Foreground commands only

Run every load-bearing command in the foreground. If a command detaches past the harness bound (~120s), do not wait on the background task: mark its result **PENDING** in your final sign-off and end your run. The manager re-runs every PENDING gate at review. A promised "I'll resume on completion" never fires for a settled one-shot.

## Shared-workspace tree hygiene

Other workers may share this workspace. Before editing:

1. Re-verify the tree matches your brief (the files and state you were told to expect).
2. Leave unfamiliar files and modifications completely alone — never revert, fix, or "clean up" work that is not yours.
3. Name any foreign edit you observe in your sign-off. You are the manager's eyes on the tree; the manager relays facts between workers.

## Sign-off shape

End with a compact statement of: what changed (files + intent), which gates you ran and their results, anything marked PENDING, and any foreign tree activity you noticed. The manager reviews your **diff**, not your story — never summarize around a failure.
