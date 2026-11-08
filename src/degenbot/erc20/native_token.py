"""NativeToken: the chain's conceptual native asset (ADR-097)."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Self

if TYPE_CHECKING:
    from degenbot._ffi import NativeToken as _NativeTokenHandle
    from degenbot.types.aliases import ChainId


class NativeToken:
    """The conceptual native asset of one chain (Ether/ETH on Ethereum).

    Exactly one exists per chain. It has **no protocol address of its own** —
    there is deliberately no ``address`` property — and its metadata is the
    core's intrinsic chain-facts manifest (name/symbol/decimals per chain), not
    a contract read and not operator config (ADR-097 D2).

    Protocol sentinels (the Curve all-Es coin, the V4 ``address(0)`` currency)
    are translated only inside those protocols' call sites; a raw sentinel
    handed to a generic entry point fails loudly instead of resolving here.

    Direct construction is forbidden: use ``Bot.get_native_token()`` (or a
    protocol's own construction, which returns this type for its native leg).
    """

    # Instance attributes set in `from_handle` (the only construction seam).
    # Declared at class scope so the type checker tracks them without inline
    # annotations on the classmethod body.
    _py_native: _NativeTokenHandle

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        Raises:
            TypeError: Always. Use ``Bot.get_native_token()``.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.get_native_token(chain_id) to obtain the session's "
            "native token handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def from_handle(cls, py_native: _NativeTokenHandle) -> Self:
        """Wrap a Rust-owned ``NativeToken`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). Rust
        owns the chain-facts metadata; this companion reads it through
        ``self._py_native`` on every access and holds no metadata copy.

        Returns:
            A ``cls`` instance wrapping ``py_native``.

        """
        self = cls.__new__(cls)
        self._py_native = py_native
        return self

    @property
    def chain_id(self) -> ChainId:
        """Chain whose native asset this is."""
        return self._py_native.chain_id

    @property
    def name(self) -> str:
        """Native asset name (chain facts; e.g. ``"Ether"``)."""
        return self._py_native.name

    @property
    def symbol(self) -> str:
        """Native asset symbol (chain facts; e.g. ``"ETH"``)."""
        return self._py_native.symbol

    @property
    def decimals(self) -> int:
        """Native asset decimals (chain facts; e.g. ``18``)."""
        return self._py_native.decimals

    def __repr__(self) -> str:
        """Return a representation naming the chain, not an address.

        Returns:
            A representation naming the symbol and chain ID.

        """
        return f"{type(self).__name__}(symbol={self.symbol!r}, chain_id={self.chain_id!r})"

    def __eq__(self, other: object) -> bool:
        """One conceptual native token per chain: equality is chain identity.

        Returns:
            Whether `other` is this chain's conceptual native token.

        """
        return type(self) is type(other) and self.chain_id == other.chain_id  # type: ignore[attr-defined]

    def __hash__(self) -> int:
        """Hash on the class and chain, the same identity :meth:`__eq__` uses.

        Returns:
            The hash of this token's class and chain identity.

        """
        return hash((type(self), self.chain_id))
