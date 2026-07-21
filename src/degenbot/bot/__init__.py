"""Bot: central session manager for pool/token construction and registries.

Barrier module (ADR-013: the Pydantic barrier): bridges the Rust ``PyBot`` /
``PyBotIo`` pyclasses from ``_ffi`` and re-exports the deep ``Bot`` lifecycle
logic from :mod:`._bot`. Importers should use::

    from degenbot.bot import Bot, PyBot, PyBotIo

rather than reaching into ``degenbot._ffi`` directly.
"""

from degenbot._ffi import PyBot, PyBotIo

from ._bot import Bot

__all__ = ["Bot", "PyBot", "PyBotIo"]
