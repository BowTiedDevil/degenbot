"""FF-T5 (NT7HJC): the runtime fleet status - budget, plan, census.

"degenbot.runtime_status()" answers the operator's first three questions
about a live process: what did the fleet boot as (the plan: binding,
oversubscription, the tier refusal it fell from), what does it run (the
projected budget: seats and shares), and who is executing (the worker
census rows, with the lane-to-thread binding per resource).
"""

from degenbot._ffi import runtime_status

__all__ = ["runtime_status"]
