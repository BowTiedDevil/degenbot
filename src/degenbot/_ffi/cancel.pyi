class CancelHandle:
    """Cooperative cancel flag for the updater loops.

    Pass an instance to ``run_pool_update`` / ``run_aave_update``; set
    ``cancel()`` to request a clean stop at the next chunk boundary.
    """

    def __init__(self) -> None: ...
    def cancel(self) -> None: ...
    def is_cancelled(self) -> bool: ...

__all__ = ["CancelHandle"]
