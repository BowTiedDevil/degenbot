"""Database session manager providing indirection over a scoped_session.

This allows the underlying scoped session to be swapped at runtime (e.g. for
test overrides with an in-memory database) without rebinding the module-level
``db_session`` name used as a default parameter across the codebase.
"""

from typing import Any

from sqlalchemy.engine import Connection
from sqlalchemy.orm import Session, scoped_session


class DatabaseSessionManager:
    """Callable proxy over a :class:`scoped_session`.

    Delegates ``__call__``, ``connection``, ``remove``, and any attribute
    access to the underlying scoped session so that existing usage patterns
    continue to work:

    * ``with db_session() as session:``  (context-manager)
    * ``db_session.scalar(...)``          (proxied method)
    * ``db_session.connection()``         (proxied method)
    * ``db_session.remove()``             (proxied method)
    * ``session: scoped_session = db_session``  (default parameter)

    Call :meth:`_reset` to replace the underlying scoped session (e.g. in a
    test fixture).
    """

    def __init__(self, session: scoped_session[Session]) -> None:
        """Initialize the instance."""
        self._session = session

    def _reset(self, session: scoped_session[Session]) -> None:
        """Replace the underlying scoped session."""
        self._session = session

    def __call__(self, **kwargs: Any) -> Session:
        """Call  .

        Returns:
            The computed value.

        """
        return self._session(**kwargs)

    def connection(self, **kwargs: Any) -> Connection:
        """Return a database connection.

        Returns:
            The computed value.

        """
        return self._session.connection(**kwargs)

    def remove(self) -> None:
        """Perform remove."""
        self._session.remove()

    def __getattr__(self, name: str) -> Any:  # noqa: ANN401
        """Exit the runtime context.

        Returns:
            The computed value.

        """
        return getattr(self._session, name)

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the object.

        """
        return f"DatabaseSessionManager({self._session!r})"
