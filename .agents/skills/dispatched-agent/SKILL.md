---
name: dispatched-agent
description: Rules for workers dispatched into the degenbot workspace by a project manager (one-shot agents, actors). Load before making any edit or running any gate when you arrived via dispatch. Covers commit restrictions, scoped test gates, foreground command discipline, and shared-workspace tree hygiene.
---

# Dispatched-Agent Lane Rules

You arrived by dispatch and work under a project manager. Your manager owns commits, the whole-workspace gates, the task lifecycle, and the definition of done. Your job is one scoped chunk of implementation work.

## Never commit or push

Commits and staging belong to the manager, who commits only in the post-settle quiet window. Leave the tree dirty for review, and never stage files yourself — a partial stage by you plus the pre-commit stash dance plus the manager's own edits aborts the commit after mutating the tree.

## Run only scoped gates

Your scoped gates are named in the **dispatch brief**, and the **current justfile** is the authority for how to run them: its recipes carry the full contract (locking, the Python libdir export, feature aliases, `RUSTFLAGS`, the nextest profile, pytest marker filters), and a hand-typed `cargo test`, `cargo build`, `maturin develop`, or `uv run pytest` misses it. Run gates through their recipe — narrowing inside a tight red-green loop is fine (`just test-rust-nextest -p <crate>`, `-E 'test(name)'`) — then run the brief's scoped gates whole before signing off. Whatever the brief names for lint/format, run it: the commit hook formats every staged file, so a format miss blocks the manager's commit.

Two conditional extras, when your change triggers them:

- `just lint-rust-check` when your change alters a public type's size or shape (enum variant payloads, `Result` error types, struct fields crossing crates) — clippy's type-size/`result_large_err` lints only fire on the full-feature lane, so the scoped gate stays green while the workspace lint lane breaks
- `just gen-stubs` when degenbot_rs or a doc-comment feeding the stubs changed — the stub-drift gate fails closed on any FFI-crate doc/signature touch; run it before signing off

Not yours (the dispatcher runs these across all workers):

- `just test-rust-nextest`, `just test-python`, `just test-rust` and any other whole-workspace ladder
- whole-tree lint/format sweeps

Scoped narrowing never proves workspace-wide health — say what you ran and what you did not.

**Final scoped gates run after your final edit.** Any gate result that predates the last edit is stale; re-run after the last change and report the fresh result.

## Foreground commands only

Run every load-bearing command in the foreground. If a command detaches past the harness bound (~120s), do not wait on the background task: mark its result **PENDING** in your final sign-off and end your run. The manager re-runs every PENDING gate at review. A promised "I'll resume on completion" never fires for a settled one-shot.

## Shared-workspace tree hygiene

Other workers may share this workspace. Before editing:

1. Re-verify the tree matches your brief (the files and state you were told to expect).
2. Leave unfamiliar files and modifications completely alone — never revert, fix, or "clean up" work that is not yours.
3. Name any foreign edit you observe in your sign-off. You are the manager's eyes on the tree; the manager relays facts between workers.

## Scope guard

Your brief is one design problem and one behavior cluster. If the work reaches an independent design problem or behavior cluster beyond it, stop at a **coherent partial completion** — the part you finished stands on its own — and report the concrete continuation scope: what remains, which files it touches, and what the next slice must decide. Never narrow acceptance criteria silently to fit what you finished, and never report partial completion as complete; name every unmet acceptance item explicitly.

## Sign-off shape

End with a compact statement of: what changed (files + intent), which gates you ran after your final edit and their results, anything marked PENDING, any unfinished acceptance items with their continuation scope, and any foreign tree activity you noticed. The manager reviews your **diff**, not your story — never summarize around a failure.
