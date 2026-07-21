"""Stub for the dynamically-created ``degenbot._ffi.cancel`` submodule.

Created at runtime by ``register_cancel`` in the PyO3 wrapper crate
(``degenbot-python/src/cancel.rs``). Holds the shared cooperative cancel
flag for the pool + aave updater loops.
"""

class CancelHandle:
    """Cooperative cancel flag for the updater loops.

    Pass an instance to ``run_pool_update`` / ``run_aave_update``; set
    ``cancel()`` to request a clean stop at the next chunk boundary.
    """

    def __init__(self) -> None: ...
    def cancel(self) -> None: ...
    def is_cancelled(self) -> bool: ...

__all__ = ["CancelHandle"]
