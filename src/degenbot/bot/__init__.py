"""Bot: central session manager for pool/token construction and registries.

Barrier module (ADR-013: the Pydantic barrier): bridges the Rust ``RustBot`` /
``RustBotIo`` pyclasses from ``_ffi`` and re-exports the deep ``Bot`` lifecycle
logic from :mod:`._bot`. Importers should use::

    from degenbot.bot import Bot, RustBot, RustBotIo

rather than reaching into ``degenbot._ffi`` directly.
"""

from degenbot._ffi import RustBot, RustBotIo

from ._bot import Bot

__all__ = ["Bot", "RustBot", "RustBotIo"]
