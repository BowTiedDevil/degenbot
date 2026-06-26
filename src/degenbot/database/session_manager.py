"""Database session manager providing indirection over a scoped_session.

This allows the underlying scoped session to be swapped at runtime (e.g. for
test overrides with an in-memory database) without rebinding the module-level
``db_session`` name used as a default parameter across the codebase.
"""

from typing import Any, ClassVar

from sqlalchemy import Engine
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

    Every live instance is tracked in a process-wide list so :meth:`dispose_all`
    (used by the test-suite teardown fixture) can release any engine a test forgot
    to close — preventing the ``ResourceWarning: unclosed database`` that
    otherwise surfaces under GC. Tracking holds a *strong* reference deliberately:
    a weak ref would let the Bot (and its engine) be collected before the
    autouse teardown runs, which is precisely when the connection leaks.
    """

    _LIVE: ClassVar[list["DatabaseSessionManager"]] = []

    def __init__(self, session: scoped_session[Session]) -> None:
        """Initialize the instance."""
        self._session = session
        self._engine: Engine | None = self._extract_engine(session)
        DatabaseSessionManager._LIVE.append(self)

    @staticmethod
    def _extract_engine(session: scoped_session[Session]) -> Engine | None:
        """Pull the bound ``Engine`` (if any) out of the scoped session.

        The engine is bound on the session factory (``sessionmaker.kw["bind"]``).
        Retaining it lets :meth:`dispose` release the connection pool that holds
        the underlying ``sqlite3.Connection`` — without this, ``db.remove()`` only
        returns the thread-local Session and the engine leaks
        (``ResourceWarning: unclosed database``).

        Returns:
            The bound ``Engine``, or ``None`` when none is bound.

        """
        factory: Any = getattr(session, "session_factory", None)
        if factory is None:
            return None
        bind: Any = getattr(factory, "kw", {}).get("bind")
        return bind if isinstance(bind, Engine) else None

    def _reset(self, session: scoped_session[Session]) -> None:
        """Replace the underlying scoped session."""
        self._session = session
        self._engine = self._extract_engine(session)

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

    def dispose(self) -> None:
        """Dispose the bound SQLAlchemy ``Engine``, if any.

        Drops the Engine's connection pool, closing the underlying
        ``sqlite3.Connection`` that ``remove()`` alone leaves open. Idempotent
        and safe when no engine is bound (e.g. test stand-ins). Paired with
        :meth:`remove` in end-of-life teardown (:func:`degenbot.bot_lifecycle.close`).
        """
        engine = self._engine
        self._engine = None
        if engine is not None:
            engine.dispose()

    @classmethod
    def dispose_all(cls) -> None:
        """Dispose the engines of every still-live manager.

        Test-suite safety net: called by the autouse teardown fixture after each
        test so a Bot/Bot-db constructed inline (and never ``close()`` ed) does
        not leak its ``sqlite3.Connection``. Idempotent — managers already
        disposed have ``_engine = None`` and are skipped. Clears the registry so
        instances do not accumulate across tests (the strong refs would otherwise
        pin them for the whole process).
        """
        live, cls._LIVE[:] = cls._LIVE[:], []
        for manager in live:
            manager.dispose()

    def __getattr__(self, name: str) -> Any:  # noqa: ANN401
        """Proxy attribute access to the underlying scoped session.

        Note: ``_engine`` is set in ``__init__`` and must NOT fall through here,
        or Python calls this during unpickling/repr before ``__init__`` runs.

        Returns:
            The attribute looked up on the underlying scoped session.

        """
        return getattr(self._session, name)

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the object.

        """
        return f"DatabaseSessionManager({self._session!r})"
