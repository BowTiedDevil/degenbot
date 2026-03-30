"""Multi-chain connection manager with automatic failover.

This module provides a Rust-based connection manager for multiple blockchain
networks with health monitoring and automatic failover between RPC endpoints.

Example:
    >>> from degenbot.connection.manager import ConnectionManager, ChainConfig
    >>> manager = ConnectionManager()
    >>> manager.register_chain(
    ...     ChainConfig(
    ...         chain_id=1,
    ...         rpc_urls=["https://eth1.example.com", "https://eth2.example.com"],
    ...     )
    ... )
    >>> metrics = manager.get_metrics(chain_id=1)
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Literal, Self, cast

from degenbot._rs import (
    ChainConfig as _ChainConfig,
)
from degenbot._rs import (
    ConnectionManager as _ConnectionManager,
)

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot.types.aliases import ChainId


@dataclass(frozen=True)
class EndpointMetrics:
    """
    Metrics for an RPC endpoint.

    Attributes:
        rpc_url: Endpoint URL
        status: Current health status ("healthy", "unhealthy", or "checking")
        success_count: Total successful requests
        failure_count: Total failed requests
        avg_latency_ms: Average latency in milliseconds
        is_healthy: Boolean health status
    """

    rpc_url: str
    status: Literal["healthy", "unhealthy", "checking"]
    success_count: int
    failure_count: int
    avg_latency_ms: float
    is_healthy: bool


@dataclass
class ChainConfig:
    """
    Configuration for a blockchain connection.

    Args:
        chain_id: Chain ID (e.g., 1 for Ethereum mainnet)
        rpc_urls: List of RPC URLs (primary first)
        max_connections: Maximum concurrent connections per endpoint (default: 10)
        timeout: Request timeout in seconds (default: 30.0)
        max_retries: Maximum retry attempts (default: 10)
        max_blocks_per_request: Maximum blocks per log request (default: 5000)

    Example:
        >>> config = ChainConfig(
        ...     chain_id=1,
        ...     rpc_urls=["https://eth1.example.com", "https://eth2.example.com"],
        ...     max_connections=20,
        ...     timeout=60.0,
        ... )
    """

    chain_id: ChainId
    rpc_urls: Sequence[str]
    max_connections: int = 10
    timeout: float = 30.0
    max_retries: int = 10
    max_blocks_per_request: int = 5000


class ConnectionManager:
    """
    Multi-chain connection manager with automatic failover.

    Replaces: ConnectionManager, AsyncConnectionManager

    Provides a registry of providers for multiple chains with automatic
    failover between RPC endpoints and health monitoring.

    Example:
        >>> manager = ConnectionManager()
        >>> manager.register_chain(
        ...     ChainConfig(
        ...         chain_id=1,
        ...         rpc_urls=["https://eth1.example.com", "https://eth2.example.com"],
        ...     )
        ... )
        >>> health = manager.health_check(chain_id=1)
        >>> metrics = manager.get_metrics(chain_id=1)
    """

    def __init__(self) -> None:
        """Create a new connection manager."""
        self._manager = _ConnectionManager()

    def register_chain(self, config: ChainConfig) -> None:
        """
        Register a new chain with RPC endpoints.

        Args:
            config: Chain configuration

        Raises:
            ValueError: If chain registration fails
        """
        rs_config = _ChainConfig(
            chain_id=config.chain_id,
            rpc_urls=list(config.rpc_urls),
            max_connections=config.max_connections,
            timeout=config.timeout,
            max_retries=config.max_retries,
            max_blocks_per_request=config.max_blocks_per_request,
        )
        self._manager.register_chain(rs_config)

    def set_default_chain(self, chain_id: ChainId) -> None:
        """
        Set the default chain.

        Args:
            chain_id: Chain ID to set as default

        Raises:
            ValueError: If chain is not registered
        """
        self._manager.set_default_chain(chain_id)

    @property
    def default_chain_id(self) -> ChainId:
        """
        Get the default chain ID.

        Returns:
            Default chain ID

        Raises:
            ValueError: If no default chain is set
        """
        return cast("ChainId", self._manager.get_default_chain_id())

    def health_check(self, chain_id: ChainId) -> dict[str, bool]:
        """
        Perform health check on all endpoints for a chain.

        Args:
            chain_id: Chain ID to check

        Returns:
            Dictionary mapping RPC URL to health status (True=healthy)

        Raises:
            ValueError: If health check fails
        """
        return dict(self._manager.health_check(chain_id))

    def get_metrics(self, chain_id: ChainId) -> list[EndpointMetrics]:
        """
        Get metrics for all endpoints of a chain.

        Args:
            chain_id: Chain ID to get metrics for

        Returns:
            List of EndpointMetrics objects with typed fields

        Raises:
            ValueError: If metrics retrieval fails
        """
        rs_metrics = self._manager.get_metrics(chain_id)
        return [
            EndpointMetrics(
                rpc_url=m.rpc_url,
                status=m.status,
                success_count=m.success_count,
                failure_count=m.failure_count,
                avg_latency_ms=m.avg_latency_ms,
                is_healthy=m.is_healthy,
            )
            for m in rs_metrics
        ]

    def is_healthy(self, chain_id: ChainId) -> bool:
        """
        Check if at least one endpoint is healthy for a chain.

        Args:
            chain_id: Chain ID to check

        Returns:
            True if at least one endpoint is healthy
        """
        results = self.health_check(chain_id)
        return any(results.values())

    def get_best_endpoint(self, chain_id: ChainId) -> str:
        """
        Get the endpoint with lowest latency for a chain.

        Args:
            chain_id: Chain ID to get endpoint for

        Returns:
            RPC URL of the best endpoint

        Raises:
            ValueError: If no healthy endpoints available
        """
        metrics = self.get_metrics(chain_id)
        healthy = [m for m in metrics if m.is_healthy]
        if not healthy:
            msg = f"No healthy endpoints for chain {chain_id}"
            raise ValueError(msg)
        return min(healthy, key=lambda m: m.avg_latency_ms).rpc_url

    def close(self) -> None:
        """Close all connections and release resources."""
        self._manager.close()

    def __enter__(self) -> Self:
        """Context manager entry."""
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: object,
    ) -> None:
        """Context manager exit."""
        self.close()


__all__ = [
    "ChainConfig",
    "ConnectionManager",
    "EndpointMetrics",
]
