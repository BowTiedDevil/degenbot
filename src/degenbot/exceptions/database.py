import pathlib

from degenbot.exceptions.base import DegenbotError


class BackupExists(DegenbotError):
    """
    Raised by `degenbot database backup` if a file exists at the target path.
    """

    def __init__(self, path: pathlib.Path) -> None:
        self.path = path
        super().__init__(message=f"A backup at {path} already exists.")
