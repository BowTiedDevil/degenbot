import pathlib

from degenbot.exceptions.base import DegenbotError


class BackupExists(DegenbotError):
    """
    The rate of exchange for the path is below the minimum.
    """

    def __init__(self, path: pathlib.Path) -> None:
        self.path = path
        super().__init__(message=f"A backup at {path} already exists.")
