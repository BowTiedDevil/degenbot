from typing import Any, Callable, Set, Tuple

from ..baseclasses import BaseLiquidityPool


def _true(*_):
    return True


class LiquidityPoolObserver:
    def __init__(self, pool: BaseLiquidityPool):
        self.pool = pool
        self.callbacks: Set[
            Tuple[
                Callable[[Any], bool],  # callback
                Callable,  # conditional
            ]
        ] = set()
        self.subscribers = set()

    def add_callback(
        self,
        callback: Callable,
        condition: Callable[[Any], bool] | None = None,
    ):
        """
        Add a callable and an optional callable conditional to be evaluated on notifications.
        If omitted, the conditional is assumed to always be True.

        The conditional must accept the `self.pool` as an input and return a boolean.
        """

        self.callbacks.add(
            (
                callback,
                condition if condition is not None else _true,
            ),
        )

    def remove_callback(
        self,
        callback: Callable,
        condition: Callable[[Any], bool] | None = None,
    ):
        self.callbacks.remove(
            (
                callback,
                condition if condition is not None else _true,
            )
        )

    def notify(self, publisher):
        if not isinstance(publisher, BaseLiquidityPool):
            return

        for callback, condition in self.callbacks:
            if condition(self.pool) is True:
                callback()
