"""Operator control surface for a live bot (NWTUM3).

A Unix-domain-socket command channel (":mod:`degenbot.operator.operator_channel`")
that lets an operator steer a running bot — add a specific path or trigger a
bounded on-demand discovery — without touching its process. The host runs an
:class:`~degenbot.operator.operator_channel.OperatorServer`; the
``degenbot path add`` / ``degenbot path discover`` CLI writes commands to it.
"""

from degenbot.operator.operator_channel import (
    OperatorHandler,
    OperatorServer,
    StepSpec,
    send_command,
    step_from_wire,
)

__all__ = ["OperatorHandler", "OperatorServer", "StepSpec", "send_command", "step_from_wire"]
