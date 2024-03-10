from typing import Any, Callable, Sequence


class ConditionalAction:
    def __init__(
        self,
        condition: Callable[[], bool],
        actions: Sequence[Callable[[], Any]],
    ):
        self.condition = condition
        self.actions = actions

    def check(self) -> None:
        if self.condition() is True:
            for action in self.actions:
                action()
