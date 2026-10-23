"""Deadlock-guard budgets for waits on completion the test cannot itself force.

A wait whose completion edge the test controls — an ``asyncio.Event`` a fake
sets, a task the test cancels, a pool thread that records its own start — is an
*ordering* gate: that edge is what the assertions read, and elapsed time plays
no part in them. The timeout wrapped around such a wait is **not** an
assertion. It exists only so a regression that never reaches the edge fails
fast with a named error instead of parking the worker until the suite timeout
fires.

The budgets below are therefore guards, not expectations. They sit far above
any plausible scheduling delay on a loaded runner (busy CI box, cold page
cache, a thread waiting its turn against the GIL). Tightening one converts a
correct-but-slow run into a clock-declared failure — exactly the flake this
module exists to prevent. Nothing here may sequence assertions: a test that
must prove "A happened before B" gates on an event it sets itself, never on a
duration.
"""

# In-process waits: a task or thread a correct run reaches within milliseconds.
DEADLOCK_GUARD_S = 30.0

# Waits on an external process (anvil boot, a mined receipt): process startup
# and RPC round-trips dominate, so the guard is wider.
EXTERNAL_PROCESS_GUARD_S = 60.0
