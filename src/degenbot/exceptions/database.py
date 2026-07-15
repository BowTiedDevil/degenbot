"""Rust-raised database-schema exception, re-exported under a companion name.

:class:`DatabaseSchemaStale` is raised by the Rust ``degenbot-db`` PyO3 seam
when a database's schema is older than the version the Rust core expects —
the caller must upgrade before the core can open it. It subclasses
:class:`ValueError` so broad ``except ValueError`` handlers still catch it.

.. note::

    This is a **direct alias re-export** of the Rust ``#[pyclass]`` type
    (``from degenbot._ffi import DatabaseSchemaStale``), not a Python
    subclass. The Rust engine *raises* the ``degenbot._ffi`` pyclass
    instance, so Python-side ``except DatabaseSchemaStale:`` matching must
    hit the exact type the Rust side raises. A subclass re-export would
    silently break that catch. The companion's only job is to give the type
    a stable home in ``degenbot.exceptions`` so CLI / driver code does not
    import ``degenbot._ffi``.
"""

from degenbot._ffi import DatabaseSchemaStale

__all__ = [
    "DatabaseSchemaStale",
]
