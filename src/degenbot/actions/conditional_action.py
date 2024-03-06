from typing import Any, Callable, Sequence


class ConditionalAction:
    def __init__(
        self,
        condition: Callable[[Any], bool],
        actions: Sequence[Callable[[Any], Any]],
    ):
        self.condition = condition
        self.actions = actions

    def check(self):
        if self.condition() is True:
            for action in self.actions:
                action()
