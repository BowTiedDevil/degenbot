from typing import Any, Callable


class ConditionalAction:
    def __init__(
        self,
        condition: Callable[[Any], bool],
        action: Callable[[Any], Any],
    ):
        self.condition = condition
        self.action = action

    def check(self):
        if self.condition() is True:
            self.action()
