from abc import ABC, abstractmethod


class BaseCondition(ABC):
    # Derived classes must implement a `__call__` method so the condition can be evaluated as a
    # callable.
    @abstractmethod
    def __call__(self) -> bool: ...
