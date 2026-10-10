# ADR-002: Pool Type Registry as Module-Level Singleton

> **Direction note.** The singleton still exists in code, but the project's
> direction moves pool-type knowledge to the Rust core as data (ADR-059 D3's
> species manifest; ADR-064: the Python side is an adapter, not an identity
> owner). Treat the singleton as legacy-in-place, not a fixture to build on.

The Pool Type Registry (`pool_type_registry`) is a module-level singleton, while all other registries (Pool, Token, Managed Pool) are class instances owned by Bot. This is intentional: the (chain ID, factory address) → class + identity + deployment data mapping is global knowledge that does not vary between Bot instances. Making it Bot-owned would require every DEX module to accept a Bot parameter at import time to register its classes, coupling module initialization to session creation. The singleton pattern allows each DEX module to self-register at import time via `pool_type_registry.register()`, independently of any Bot instance.
