"""Bot: central session manager for pool/token construction and registries.

Re-exports the driver ``Bot`` session class from :mod:`._bot` plus the
``driver_boot`` seam (the explicit install of the shared runtime + telemetry
stack, called by ``Bot.__init__``) from ``degenbot._ffi``. The Rust engine
handles — the same-named ``degenbot._ffi.Bot`` / ``BotIo`` pyclasses — are
imported directly from ``degenbot._ffi`` by first-party code: the module path
disambiguates the driver class from the engine handle (ADR-032; the ADR-013
init-only rule exempts the engine-handle types).
"""

from degenbot._ffi import driver_boot

from ._bot import Bot

__all__ = ["Bot", "driver_boot"]
