# Database Agent Documentation

## Overview

SQLite database with Write-Ahead Logging (WAL) for persistence. SQLAlchemy ORM with Alembic migrations.

## Components

**Models** (`models/`)
- SQLAlchemy ORM models for liquidity pools, tokens, positions
- Type-annotated for mypy strict mode

**Operations**
- `operations.py` - Database queries, inserts, and transactions
- Scoped session management for thread safety

**Initialization**
- `__init__.py` - Version check and migration warnings
- Alembic integration for schema versioning

## Design Patterns

- Scoped sessions for thread-safe database access
- Alembic migrations track schema changes
- Version mismatch detection on module import
