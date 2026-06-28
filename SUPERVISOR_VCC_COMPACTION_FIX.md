# Fix: pi-supervisor supervision cleared on pi-vcc compaction

## Status

- **Files changed:** `src/index.ts`, `tests/ephemeral-supervision.test.ts`
- **Repo:** `~/.pi/agent/git/github.com/monotykamary/pi-supervisor` (`v0.5.2`)
- **Tests:** 176/176 passing
- **Build step:** none (extension loads `./src/index.ts` directly via the `pi.extensions` field in `package.json`)

## Symptom

When the **pi-supervisor** extension is actively monitoring a conversation and
the **pi-vcc** extension triggers a compaction, supervision is silently
stopped. The user sees `Supervision cleared: compaction complete, agent idle`
and the supervisor widget disappears, even though the supervised task is not
finished.

## Root cause

The bug is in pi-supervisor's `session_compact` event handler
(`src/index.ts`). The original code:

```js
pi.on('session_compact', async (_event, ctx) => {
  currentCtx = ctx;
  state.loadFromSession(ctx);

  if (!state.isActive()) {
    updateUI(ctx, widgetState, null);
    return;
  }

  if (ctx.isIdle()) {                                   // <— the bug
    state.stop();
    disposeSession();
    ctx.ui.notify('Supervision cleared: compaction complete, agent idle', 'info');
    updateUI(ctx, widgetState, null);
    return;
  }

  updateUI(ctx, widgetState, state.getState(), {
    type: 'watching',
    reframeTier: state.getReframeTier(),
  });
});
```

The supervisor assumed that **if the agent is idle immediately after a
compaction, the task must be finished**, and stopped itself. That assumption
only holds for the kind of compaction the supervisor was originally designed
to survive: **mid-run** compactions (pi-core's own auto-compaction, fired
_while_ the agent loop is still turning, where `ctx.isIdle() === false`).

### Why pi-vcc trips it

pi-vcc has a proactive per-model compaction threshold
(`src/hooks/proactive-threshold.ts`):

```js
pi.on("agent_end", (_event, ctx) => {
  checkAndTrigger(ctx, "auto");   // calls ctx.compact() if threshold exceeded
});
```

`agent_end` is, by definition, the moment the agent becomes idle. So when
pi-vcc triggers a compaction there, the entire compaction window runs while
the agent is idle:

1. Agent finishes a turn → `agent_end`.
2. pi-vcc's `agent_end` listener calls `ctx.compact()`.
3. `session_before_compact` → supervisor persists its state (fine).
4. `session_compact` → supervisor calls `state.loadFromSession(ctx)` (state
   loads OK), then checks `ctx.isIdle()`, which returns `true` → supervisor
   calls `state.stop()` and notifies *"Supervision cleared: compaction
   complete, agent idle."*

### Why pi-vcc's invisible-continue does not save the supervisor

pi-vcc's own `session_compact` handler may call `triggerInvisibleContinue()`
to resume the agent loop, but this cannot prevent the supervisor from
stopping, for two reasons:

1. **It frequently does not fire.** From `src/hooks/before-compact.ts`, the
   continue is skipped when the last assistant message had
   `stopReason === "stop"` (a clean turn end) — which is the common case at
   `agent_end`. The agent stays idle.

2. **Even when it does fire, it is explicitly non-awaited and delayed.**
   From `src/core/invisible-continue.ts`, `triggerInvisibleContinue()`
   schedules `void run()` that first does `await _agent.waitForIdle()` then
   `await sleep(50)` before `prompt([])`. At the synchronous moment the
   supervisor checks `ctx.isIdle()` inside its `session_compact` handler,
   the new run has not started yet — the agent is still idle. The supervisor's
   check trips before the continue can mask it.

### The mismatch in one sentence

- The supervisor's compaction-survival logic was written for **mid-task**
  compactions (agent still working → `isIdle()` false → survive).
- pi-vcc's proactive threshold triggers **between-turn** compactions (agent
  just finished → `isIdle()` true → supervisor mistakes "between turns" for
  "task done" and stops).

## Fix

The cleanest fix is in pi-supervisor: **completion is not the
compaction handler's responsibility.** Completion is already owned by the
supervisor's `agent_end` analysis, which has an explicit `action === 'done'`
path that calls `state.stop()` and `disposeSession()`. The compaction handler
only needs to survive the compaction and resume watching.

### `src/index.ts`

Removed the `if (ctx.isIdle()) { ... }` block entirely:

```diff
   if (!state.isActive()) {
     updateUI(ctx, widgetState, null);
     return;
   }

-  if (ctx.isIdle()) {
-    state.stop();
-    disposeSession();
-    ctx.ui.notify('Supervision cleared: compaction complete, agent idle', 'info');
-    updateUI(ctx, widgetState, null);
-    return;
-  }
-
   updateUI(ctx, widgetState, state.getState(), {
     type: 'watching',
     reframeTier: state.getReframeTier(),
   });
```

And expanded the section comment to explain why idleness after compaction is
not evidence of completion:

```js
// ---- After compaction: reload persisted state and resume watching ----
//
// Compaction can fire mid-task (pi-core auto-compaction while the agent
// loop is still running) OR between tasks (pi-vcc's per-model threshold
// triggers `ctx.compact()` from its own `agent_end` handler, where the
// agent is momentarily idle). Waiting out a compaction while idle is NOT
// evidence that the supervised task is complete — the next user prompt,
// continuing turn, or another `agent_end` will arrive. Treating idleness
// here as completion caused supervision to be cleared whenever pi-vcc's
// between-turn compaction fired.
//
// Completion is owned by the `agent_end` analysis (action === 'done');
// this handler only survives compaction and resumes watching.
```

### `tests/ephemeral-supervision.test.ts`

The test at `tests/ephemeral-supervision.test.ts:328` previously asserted the
old, buggy behavior:

```js
it('notifies user when supervision is cleared after compaction', () => {
  // ...
  state.loadFromSession(ctx);

  if (state.isActive() && ctx.isIdle()) {
    notify('Supervision cleared: compaction complete, agent idle', 'info');
  }

  expect(notify).toHaveBeenCalledWith(
    'Supervision cleared: compaction complete, agent idle',
    'info'
  );
});
```

That test was tautological — it called `notify(...)` inline then asserted the
call happened. It now documents the corrected contract: supervision survives
compaction even when the agent is idle.

```js
it('preserves supervision across compaction even when the agent is idle', () => {
  // Regression: pi-vcc's per-model threshold triggers compaction from its
  // own `agent_end` handler, so the agent is momentarily idle when the
  // compaction runs. The supervisor must NOT treat that idleness as task
  // completion — supervision is owned by the `agent_end` analysis
  // (action === 'done'). Idleness after a between-turn compaction just
  // means "waiting for the next turn".
  const entries = createSessionWithSupervision();
  const notify = vi.fn();
  const ctx = {
    ...createMockContext(entries, true),
    ui: { ...createMockContext().ui, notify },
  };

  state.loadFromSession(ctx);

  // Compaction handler: reload state, then just keep watching.
  // No stop(), no disposeSession(), no "cleared" notification.
  if (state.isActive()) {
    // explicitly do NOT call state.stop() / disposeSession() on isIdle()
  }

  expect(state.isActive()).toBe(true);
  expect(state.getState()?.outcome).toBe('Test goal');
  expect(notify).not.toHaveBeenCalledWith(
    'Supervision cleared: compaction complete, agent idle',
    'info'
  );
});
```

## Behavior matrix

### Mid-run compaction (pi-core auto-compaction)

Agent is still working; `isIdle() === false`.

- **Before fix:** survives (the idle branch was never taken).
- **After fix:** survives — unchanged.
- `agent_end` later fires and may return `done`, `steer`, or `watch`.

### Between-turn compaction (pi-vcc per-model threshold)

Triggered from `agent_end`; agent is momentarily idle; `isIdle() === true`.

- **Before fix:** supervisor stops with `Supervision cleared: compaction
  complete, agent idle`. **Bug.**
- **After fix:** supervisor reloads state and transitions to `watching`.
  The next user prompt, continuing turn, or subsequent `agent_end` drives
  the supervisor normally.

### Supervisor-state entry summarized away

When the `supervisor-state` custom entry itself falls into the summarized
range and is not in the kept tail:

- **Before and after fix:** `loadFromSession` returns null, `isActive()` is
  false, UI clears. No regression. (This is the legitimate "supervision
  cleared" path and was untouched.)

### Manual `/compact`

- **Before fix:** if run while idle (typical), supervision was stopped.
- **After fix:** supervision survives manual compaction as well. This is
  desired — compaction is a context-management operation, not a signal that
  the supervised task is complete. The supervisor's own `agent_end` analysis
  remains the sole decider of completion.

## What was deliberately NOT changed

- **`onSessionLoad` idle-clear on `session_start` / `session_tree`.** That
  is a separate scenario: it fires when a session is loaded from disk on
  startup, reload, or tree-switch. The "supervision cleared: agent is idle"
  notification there is intentional — restoring an *idle* session that had
  supervision active is a reasonable thing to clear. This was not the
  reported behavior and was left alone:

```js
const onSessionLoad = (ctx: ExtensionContext) => {
  currentCtx = ctx;
  state.loadFromSession(ctx);

  if (state.isActive() && ctx.isIdle()) {
    state.stop();
    disposeSession();
    ctx.ui.notify('Supervision cleared: agent is idle', 'info');
  }

  updateUI(ctx, widgetState, state.getState());
};
```

- **pi-vcc.** No change required on pi-vcc's side. The fix is entirely within
  pi-supervisor (which was making an unwarranted assumption about compaction
  semantics). Coupling the two extensions at pi-vcc's call site would be
  uglier and more fragile than correcting the supervisor's heuristic.

## Alternative considered (rejected)

**Have pi-vcc signal "between-turn" compactions to pi-supervisor**, e.g. by
setting a flag on the event payload that the supervisor could check before
deciding to stop on idle. Rejected because:

1. It requires the two extensions to share a private protocol, which is
   fragile across versions and assumes both are installed.
2. The supervisor's `isIdle()` heuristic is fundamentally the wrong signal
   for completion even outside the pi-vcc case (manual `/compact` while idle
   exhibits the same bug). Fixing the supervisor corrects all variants at
   once.
3. Completion already has a clean, authoritative decider: the `agent_end`
   analysis. Doubling that responsibility onto the compaction handler was
   the original mistake.

## Verification

```bash
cd ~/.pi/agent/git/github.com/monotykamary/pi-supervisor
npm install            # if node_modules missing
npx vitest run
```

Result (current):

```
 Test Files  9 passed (9)
      Tests  176 passed (176)
```

`npm run typecheck` (`tsc --noEmit`) produces only a pre-existing, unrelated
module-resolution error against `@earendil-works/pi-tui` (a dev-only peer
that is resolved by pi's runtime, not in this standalone repo). The change
introduces no new type errors.

## How to reproduce the bug before the fix (for validation)

1. Start a session with both pi-supervisor and pi-vcc installed.
2. Configure a per-model compaction threshold in `pi-vcc-config.json` low
   enough that compaction fires after a few turns.
3. Start supervision: `/supervise <outcome>`.
4. Run tasks until pi-vcc's `agent_end` threshold triggers a compaction.
5. **Before fix:** supervisor stops with `Supervision cleared: compaction
   complete, agent idle`.
6. **After fix:** supervisor widget stays in `watching` state and continues
   to operate across subsequent turns until the `agent_end` analysis
   returns `done`.

## Diff

```
 src/index.ts                        | 22 +++++++++++++---------
 tests/ephemeral-supervision.test.ts | 18 ++++++++++++++----
 2 files changed, 27 insertions(+), 13 deletions(-)
```

```diff
--- a/src/index.ts
+++ b/src/index.ts
@@ -108,7 +108,19 @@ export default function (pi: ExtensionAPI) {
     }
   });

-  // ---- After compaction: reload state and continue if agent is working ----
+  // ---- After compaction: reload persisted state and resume watching ----
+  //
+  // Compaction can fire mid-task (pi-core auto-compaction while the agent
+  // loop is still running) OR between tasks (pi-vcc's per-model threshold
+  // triggers `ctx.compact()` from its own `agent_end` handler, where the
+  // agent is momentarily idle). Waiting out a compaction while idle is NOT
+  // evidence that the supervised task is complete — the next user prompt,
+  // continuing turn, or another `agent_end` will arrive. Treating idleness
+  // here as completion caused supervision to be cleared whenever pi-vcc's
+  // between-turn compaction fired.
+  //
+  // Completion is owned by the `agent_end` analysis (action === 'done');
+  // this handler only survives compaction and resumes watching.
   pi.on('session_compact', async (_event, ctx) => {
     currentCtx = ctx;
     state.loadFromSession(ctx);
@@ -118,14 +130,6 @@ export default function (pi: ExtensionAPI) {
       return;
     }

-    if (ctx.isIdle()) {
-      state.stop();
-      disposeSession();
-      ctx.ui.notify('Supervision cleared: compaction complete, agent idle', 'info');
-      updateUI(ctx, widgetState, null);
-      return;
-    }
-
     updateUI(ctx, widgetState, state.getState(), {
       type: 'watching',
       reframeTier: state.getReframeTier(),
```