def dex_identity(variant: str) -> DexIdentity | None:
    """Look up a DEX deployment-identity preset by kebab-case variant string.

    Case-insensitive. Returns `None` for an unrecognized variant.
    """

class DexIdentity:
    """Frozen Python view over a `DexIdentity` preset (ADR-005 slice 6).

    Read-only deployment identity (factory, deployer, init hash, default fees,
    reserve ABI shape, variant string). Constructable via `DexIdentity(...)`
    for custom identities (tests / ad-hoc deployments) or resolved via
    `dex_identity(variant)` for canonical presets.
    """

    def __init__(
        self,
        factory: str,
        init_hash: str,
        fee_token0: tuple[int, int],
        fee_token1: tuple[int, int],
        variant: str,
        reserves_abi: list[str] | None = None,
    ) -> None: ...
    @property
    def factory(self) -> str: ...
    @property
    def deployer(self) -> str: ...
    @property
    def init_hash(self) -> str: ...
    @property
    def fee_token0(self) -> tuple[int, int]: ...
    @property
    def fee_token1(self) -> tuple[int, int]: ...
    @property
    def reserves_abi(self) -> list[str]: ...
    @property
    def variant(self) -> str: ...

__all__ = ["DexIdentity", "dex_identity"]
