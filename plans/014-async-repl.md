# Plan 014: Async REPL — `python -m degenbot` with Top-Level `await`

## Status: NOT STARTED

## Summary

Provide an async REPL entry point so users can interactively use async degenbot APIs without wrapping every call in `asyncio.run()`. The REPL compiles input with
`ast.PyCF_ALLOW_TOP_LEVEL_AWAIT` and executes it inside a running event loop, matching the behaviour of `python -m asyncio` but with degenbot pre-imported and configured.

## Problem

When degenbot gains async APIs (per the async-first architecture discussed in the fetcher-closure analysis), REPL users face friction:

```python
>>> from degenbot import Bot
>>> bot = Bot.from_config_file()
>>> # Can't just: pool = await bot.build_v3_pool("0x...")
>>> # Must: 
>>> import asyncio
>>> pool = asyncio.run(bot.build_v3_pool("0x..."))
```

`asyncio.run()` creates and destroys the loop each call, losing state. Wrapping in a temporary coroutine is verbose. The `python -m asyncio` built-in solves this but doesn't pre-import degenbot or set up the session.

## Target State

```bash
$ python -m degenbot
degenbot async REPL (Python 3.12.0)
Top-level `await` is supported. Type `help(degenbot)` for info.
>>> from degenbot import Bot
>>> bot = Bot.from_config_file()
>>> pool = await bot.build_v3_pool("0x...")   # works directly
>>> 
```

Also works for sync APIs — no `await` needed for non-coroutine results:

```python
>>> pool.calculate_output(1000, 0)   # sync call, returns immediately
```

## Files Involved

- **New:** `src/degenbot/__main__.py` — async REPL entry point
- **New:** `src/degenbot/_async_repl.py` — `AsyncREPLConsole` implementation
- **Updated:** `pyproject.toml` — add `[project.scripts]` entry if CLI command desired

## Technical Design

### How `python -m asyncio` Works (CPython Reference)

Source: `Lib/asyncio/__main__.py` (bpo-37028, PR #13472 by @1st1)

1. An `AsyncIOInteractiveConsole(code.InteractiveConsole)` is created
2. It overrides `compile()` to set `ast.PyCF_ALLOW_TOP_LEVEL_AWAIT` on compiler flags
3. An event loop is started and `console.interact()` runs inside it
4. `runcode()` executes compiled code on the loop via `loop.call_soon_threadsafe()`
5. If the result is a coroutine, it's scheduled with `asyncio.ensure_future()`
6. A `concurrent.futures.Future` bridges the async execution back to the synchronous
   `interact()` loop so the prompt blocks until execution completes

### Why We Can't Just `import` Our Way In

- The standard REPL's `InteractiveConsole.compile()` doesn't set `ALLOW_TOP_LEVEL_AWAIT`
- An `import` can't retroactively change the compiler flags of the enclosing REPL
- Top-level `await` also requires a running event loop; `import` can't inject one

### Why Not Use `asyncio.AsyncIOInteractiveConsole` Directly?

It's defined in `Lib/asyncio/__main__.py`, not exported from the `asyncio` package. It's an
internal implementation detail, not a public API. We replicate the pattern.

### Architecture

```
┌─────────────────────────────────────────────────┐
│  python -m degenbot                             │
│  ┌───────────────────────────────────────────┐  │
│  │  __main__.py                              │  │
│  │  1. Create event loop                     │  │
│  │  2. Pre-import degenbot into locals       │  │
│  │  3. Create AsyncREPLConsole               │  │
│  │  4. console.interact() inside loop        │  │
│  └───────────────────────────────────────────┘  │
│                      │                          │
│         ┌────────────▼────────────┐             │
│         │  _async_repl.py          │             │
│         │  AsyncREPLConsole        │             │
│         │  ├─ compile() → +AWAIT   │             │
│         │  ├─ runcode() → eval on  │             │
│         │  │   loop, auto-await    │             │
│         │  │   coroutines          │             │
│         │  └─ close() → stop loop  │             │
│         └─────────────────────────┘             │
└─────────────────────────────────────────────────┘
```

## Implementation Steps

### Phase 1: `AsyncREPLConsole` Core

Create `src/degenbot/_async_repl.py`:

```python
"""Interactive console with top-level await support.

Replicates the pattern from CPython's Lib/asyncio/__main__.py
without depending on its internal API.
"""

import ast
import asyncio
import code
import concurrent.futures
import sys
import threading


class AsyncREPLConsole(code.InteractiveConsole):
    """An InteractiveConsole that compiles with ALLOW_TOP_LEVEL_AWAIT
    and executes code on a running event loop, auto-awaiting coroutines."""

    def __init__(self, locals: dict | None = None) -> None:
        super().__init__(locals)
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(
            target=self._loop.run_forever,
            daemon=True,
        )
        self._thread.start()

    def compile(self, source: str, filename: str = "<async-repl>", symbol: str = "single"):
        return compile(
            source,
            filename,
            symbol,
            flags=ast.PyCF_ALLOW_TOP_LEVEL_AWAIT,
            dont_inherit=True,
        )

    def runcode(self, code_obj) -> None:
        fut = concurrent.futures.Future()

        def _run() -> None:
            try:
                coro = eval(code_obj, self.locals)  # noqa: S307
                if asyncio.iscoroutine(coro):
                    # Schedule the coroutine and chain its result/exception
                    # back to the bridging Future
                    async def _wrap():
                        try:
                            result = await coro
                            fut.set_result(result)
                        except BaseException as exc:
                            fut.set_exception(exc)

                    asyncio.ensure_future(_wrap(), loop=self._loop)
                else:
                    fut.set_result(coro)
            except SystemExit:
                self._loop.call_soon_threadsafe(self._loop.stop)
                raise
            except BaseException as exc:
                fut.set_exception(exc)

        self._loop.call_soon_threadsafe(_run)

        try:
            return fut.result()
        except SystemExit:
            raise
        except BaseException:
            self.showtraceback()

    def close(self) -> None:
        """Stop the event loop and join the background thread."""
        self._loop.call_soon_threadsafe(self._loop.stop)
        self._thread.join(timeout=5)
        self._loop.close()
```

**Key design decisions:**

- `eval()` (not `exec()`) so we capture the return value — a coroutine object for top-level
  `await` expressions, or `None`/value for regular statements
- `_wrap()` coroutine ensures exceptions from `await`-ed code propagate correctly through the
  bridging `Future`
- `daemon=True` on the thread so a hung loop doesn't block process exit
- `SystemExit` stops the loop before raising — clean shutdown on `exit()` or Ctrl+D

### Phase 2: `__main__.py` Entry Point

Create `src/degenbot/__main__.py`:

```python
"""Async REPL entry point: python -m degenbot"""

import sys

from degenbot._async_repl import AsyncREPLConsole


def main() -> None:
    console = AsyncREPLConsole(locals={})

    # Pre-import degenbot into the REPL namespace
    try:
        import degenbot

        console.locals["degenbot"] = degenbot
        # Flatten top-level names for convenience (optional)
        # e.g. console.locals["Bot"] = degenbot.Bot
    except ImportError:
        pass  # degenbot not installed? unlikely, but don't crash

    py_version = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    banner = f"degenbot async REPL (Python {py_version})\nTop-level `await` is supported.\n"

    try:
        console.interact(banner=banner, exitmsg="")
    finally:
        console.close()


if __name__ == "__main__":
    main()
```

### Phase 3: History & Readline Support

The base `code.InteractiveConsole.interact()` already integrates with `readline` when available.
However, the default prompt is `>>>` / `...`. We may want to customize it:

```python
sys.ps1 = "degenbot>>> "
sys.ps2 = "         ... "
```

This is cosmetic and can be deferred. The default `>>>` works fine.

If we want persistent history across sessions:

```python
import atexit
import os
import readline

histfile = os.path.expanduser("~/.cache/degenbot/repl_history")
os.makedirs(os.path.dirname(histfile), exist_ok=True)
readline.read_history_file(histfile)
atexit.register(readline.write_history_file, histfile)
```

### Phase 4: Convenience Namespace Injection

When degenbot has async APIs, users will commonly want `Bot`, `Erc20Token`, pool classes, etc.
available without `from degenbot import ...`. Options:

**Option A: Flat namespace** — inject commonly-used names directly:

```python
console.locals.update({
    "Bot": degenbot.Bot,
    "Erc20Token": degenbot.Erc20Token,
    # ... etc
})
```

**Option B: Module-only** — just `import degenbot`, users dot-access:

```python
console.locals["degenbot"] = degenbot
```

**Option C: Both** — module + selective flat names, with a banner listing them:

```python
console.locals["degenbot"] = degenbot
for name in ("Bot", "Erc20Token", "UniswapV3Pool"):
    console.locals[name] = getattr(degenbot, name)
```

**Recommendation:** Start with Option B (minimal, non-polluting). Add Option C later based on
user feedback.

### Phase 5: Integration with Bot Session

If the user wants to start a `Bot` session inside the REPL, they can:

```python
>>> bot = Bot.from_config_file()
>>> pool = await bot.build_v3_pool("0x...")
```

We could also support a `--config` CLI flag that auto-creates a `Bot`:

```bash
python -m degenbot --config ~/.config/degenbot/config.toml
```

Which would inject `bot = Bot.from_config_file(path)` into the namespace before the REPL
starts. This is a nice-to-have, not a blocker.

### Phase 6: Tests

Test the `AsyncREPLConsole` class directly:

1. **Compilation test**: Verify `compile()` produces code objects with `ALLOW_TOP_LEVEL_AWAIT`
2. **Sync execution test**: Verify `runcode()` runs non-async code correctly
3. **Async execution test**: Verify `runcode()` auto-awaits coroutines
4. **Exception propagation test**: Verify exceptions from `await`-ed code reach the caller
5. **SystemExit test**: Verify `exit()` / `SystemExit` cleanly shuts down
6. **Close test**: Verify `close()` stops the loop and joins the thread
7. **Integration test**: Run the `__main__.py` as a subprocess and verify the banner

```python
# Example test skeleton
import asyncio
from degenbot._async_repl import AsyncREPLConsole


def test_async_repl_auto_awaits():
    console = AsyncREPLConsole()
    code_obj = compile(
        "await asyncio.sleep(0); 42",
        "<test>",
        "eval",
        flags=ast.PyCF_ALLOW_TOP_LEVEL_AWAIT,
        dont_inherit=True,
    )
    result = console.runcode(code_obj)  # should not hang or raise
    console.close()
```

Note: testing an interactive REPL is inherently tricky. Focus tests on the
`compile()` + `runcode()` methods, not on `interact()` which requires
pseudo-terminal simulation.

## Alternatives Considered

### 1. `PYTHONSTARTUP` Hook

A user-level `~/.python_startup.py` that patches the REPL. **Rejected**: runs inside the
existing REPL — can't change its compiler flags or event loop from within.

### 2. Import-Time Magic (e.g., `__init__.py` that patches `sys.modules`)

**Rejected**: The enclosing REPL controls compilation. No amount of import-side trickery can
set `ALLOW_TOP_LEVEL_AWAIT` on the outer session.

### 3. IPython Dependency

Tell users to install IPython (7.0+ has native async support). **Partially accepted**: we
should document IPython as a supported alternative, but not add it as a dependency.

### 4. `python -m asyncio` + Manual Import

**Documented as fallback**: `python -m asyncio` then `import degenbot` works today with
zero code. Our `__main__.py` just adds convenience (pre-import, custom banner, optional
`--config` flag).

## Definition of Done

- [ ] `src/degenbot/_async_repl.py` implements `AsyncREPLConsole` with `compile()`,
      `runcode()`, and `close()`
- [ ] `src/degenbot/__main__.py` launches the async REPL with degenbot pre-imported
- [ ] `python -m degenbot` starts a REPL where top-level `await` works
- [ ] Sync calls (non-async) still work without `await`
- [ ] Exceptions from `await`-ed code propagate correctly (not swallowed)
- [ ] `exit()` / Ctrl+D cleanly shuts down the event loop
- [ ] Tests cover compilation, sync execution, async execution, exception propagation,
      and clean shutdown
- [ ] README or docs mention the async REPL as the recommended interactive workflow
- [ ] IPython compatibility is documented as a zero-config alternative

## Open Questions

1. **Flat namespace injection?** Which names (if any) to inject directly into the REPL
   namespace alongside `degenbot`? Defer to user feedback.
2. **`--config` CLI flag?** Whether to auto-create a `Bot` session from a config file.
   Nice-to-have, not blocking.
3. **Custom prompt?** `degenbot>>>` vs default `>>>`. Cosmetic, defer.
4. **Persistent history?** Readline history file at `~/.cache/degenbot/repl_history`.
   Low effort, nice UX — include in Phase 3 unless there's a reason not to.
