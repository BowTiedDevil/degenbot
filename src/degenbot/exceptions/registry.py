from degenbot.exceptions.base import DegenbotError

"""
Exceptions defined here are raised by classes and functions in the `registry` module.
"""


class RegistryError(DegenbotError):
    """
    Exception raised inside registries.
    """


class RegistryAlreadyInitialized(RegistryError):
    """
    Raised by a singleton registry if a caller attempts to recreate it.
    """
