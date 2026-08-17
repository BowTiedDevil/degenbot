def deployer_for(chain_id: int, factory: str) -> str | None:
    """JSON-sourced effective CREATE2 deployer for a `(chain_id, factory)` pair.

    Returns `None` if the pair is not in the shipped JSON.
    """

def init_hash_for(chain_id: int, factory: str) -> str | None:
    """JSON-sourced CREATE2 init code hash for a `(chain_id, factory)` pair.

    Returns `None` if the pair is not in the shipped JSON.
    """

def resolve_deployer(chain_id: int, factory: str) -> str:
    """Resolve the effective CREATE2 deployer for a `(chain_id, factory)` pair.

    Applies the `None -> factory` convention; returns the factory itself for
    non-JSON pools (Fork A, P62DKO).
    """

def resolve_v2_init_hash(chain_id: int, factory: str) -> str:
    """Resolve the CREATE2 init code hash for a V2 `(chain_id, factory)` pair.

    Returns the JSON row's init hash when shipped, else the Uniswap V2
    mainnet fallback (Fork A, NSAZ4X).
    """

def resolve_v3_init_hash(chain_id: int, factory: str) -> str:
    """Resolve the CREATE2 init code hash for a V3 `(chain_id, factory)` pair.

    Returns the JSON row's init hash when shipped, else the Uniswap V3
    mainnet fallback (Fork A, P62DKO).
    """

__all__ = [
    "deployer_for",
    "init_hash_for",
    "resolve_deployer",
    "resolve_v2_init_hash",
    "resolve_v3_init_hash",
]
