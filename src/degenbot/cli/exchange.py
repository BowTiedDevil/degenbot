"""CLI commands for exchange activation/deactivation.

These commands are thin delegating shells. The Python here only orchestrates the user-facing
messages; the Rust core owns the database state.
"""

import click
import eth_typing

from degenbot import _ffi
from degenbot.bot import Bot
from degenbot.cli import cli
from degenbot.uniswap.deployments import (
    BaseAerodromeV2,
    BaseAerodromeV3,
    BasePancakeswapV2,
    BasePancakeswapV3,
    BaseSushiswapV2,
    BaseSushiswapV3,
    BaseSwapbasedV2,
    BaseUniswapV2,
    BaseUniswapV3,
    BaseUniswapV4,
    EthereumMainnetPancakeswapV2,
    EthereumMainnetPancakeswapV3,
    EthereumMainnetSushiswapV2,
    EthereumMainnetSushiswapV3,
    EthereumMainnetUniswapV2,
    EthereumMainnetUniswapV3,
    EthereumMainnetUniswapV4,
)


@cli.group()
def exchange() -> None:
    """Exchange commands."""


@exchange.group
def activate() -> None:
    """Activate the exchange.

    Liquidity pools for all activated exchanges are included when running
    "pool update".
    """


@exchange.group
def deactivate() -> None:
    """Deactivate the exchange.

    Liquidity pools for all deactivated exchanges are not included when
    running "pool update".
    """


# ═══════════════════════════════════════════════════════════════════════════
# Base mainnet — activate
# ═══════════════════════════════════════════════════════════════════════════


@activate.command("base_aerodrome_v2")
@click.pass_obj
def activate_base_aerodrome_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v2",
) -> None:
    """Activate Aerodrome V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BaseAerodromeV2.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Aerodrome V2 on Base (chain ID {chain_id}).")


@activate.command("base_aerodrome_v3")
@click.pass_obj
def activate_base_aerodrome_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v3",
) -> None:
    """Activate Aerodrome V3 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BaseAerodromeV3.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Aerodrome V3 on Base (chain ID {chain_id}).")


@activate.command("base_pancakeswap_v2")
@click.pass_obj
def activate_base_pancakeswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v2",
) -> None:
    """Activate Pancakeswap V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BasePancakeswapV2.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Pancakeswap V2 on Base (chain ID {chain_id}).")


@activate.command("base_pancakeswap_v3")
@click.pass_obj
def activate_base_pancakeswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v3",
) -> None:
    """Activate Pancakeswap V3 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BasePancakeswapV3.factory.address,
        deployer=BasePancakeswapV3.factory.deployer,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Pancakeswap V3 on Base (chain ID {chain_id}).")


@activate.command("base_swapbased_v2")
@click.pass_obj
def activate_base_swapbased_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "swapbased_v2",
) -> None:
    """Activate SwapBased V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BaseSwapbasedV2.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated SwapBased V2 on Base (chain ID {chain_id}).")


@activate.command("base_sushiswap_v2")
@click.pass_obj
def activate_base_sushiswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v2",
) -> None:
    """Activate Sushiswap V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BaseSushiswapV2.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Sushiswap V2 on Base (chain ID {chain_id}).")


@activate.command("base_sushiswap_v3")
@click.pass_obj
def activate_base_sushiswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v3",
) -> None:
    """Activate Sushiswap V3 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BaseSushiswapV3.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Sushiswap V3 on Base (chain ID {chain_id}).")


@activate.command("base_uniswap_v2")
@click.pass_obj
def activate_base_uniswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v2",
) -> None:
    """Activate Uniswap V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BaseUniswapV2.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Uniswap V2 on Base (chain ID {chain_id}).")


@activate.command("base_uniswap_v3")
@click.pass_obj
def activate_base_uniswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v3",
) -> None:
    """Activate Uniswap V3 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BaseUniswapV3.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Uniswap V3 on Base (chain ID {chain_id}).")


@activate.command("base_uniswap_v4")
@click.pass_obj
def activate_base_uniswap_v4(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v4",
) -> None:
    """Activate Uniswap V4 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=BaseUniswapV4.pool_manager.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    _ffi.db_upsert_pool_manager(
        database_path=database_path,
        address=BaseUniswapV4.pool_manager.address,
        chain=chain_id,
        kind="uniswap_v4",
        state_view=BaseUniswapV4.state_view.address,
        exchange_id=row.id,
    )
    click.echo(f"Activated Uniswap V4 on Base (chain ID {chain_id}).")


# ═══════════════════════════════════════════════════════════════════════════
# Ethereum mainnet — activate
# ═══════════════════════════════════════════════════════════════════════════


@activate.command("ethereum_pancakeswap_v2")
@click.pass_obj
def activate_ethereum_pancakeswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "pancakeswap_v2",
) -> None:
    """Activate Pancakeswap V2 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=EthereumMainnetPancakeswapV2.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Pancakeswap V2 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_pancakeswap_v3")
@click.pass_obj
def activate_ethereum_pancakeswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "pancakeswap_v3",
) -> None:
    """Activate Pancakeswap V3 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=EthereumMainnetPancakeswapV3.factory.address,
        deployer=EthereumMainnetPancakeswapV3.factory.deployer,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Pancakeswap V3 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_sushiswap_v2")
@click.pass_obj
def activate_ethereum_sushiswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "sushiswap_v2",
) -> None:
    """Activate Sushiswap V2 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=EthereumMainnetSushiswapV2.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Sushiswap V2 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_sushiswap_v3")
@click.pass_obj
def activate_ethereum_sushiswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "sushiswap_v3",
) -> None:
    """Activate Sushiswap V3 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=EthereumMainnetSushiswapV3.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Sushiswap V3 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_uniswap_v2")
@click.pass_obj
def activate_ethereum_uniswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v2",
) -> None:
    """Activate Uniswap V2 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=EthereumMainnetUniswapV2.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Uniswap V2 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_uniswap_v3")
@click.pass_obj
def activate_ethereum_uniswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v3",
) -> None:
    """Activate Uniswap V3 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=EthereumMainnetUniswapV3.factory.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    click.echo(f"Activated Uniswap V3 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_uniswap_v4")
@click.pass_obj
def activate_ethereum_uniswap_v4(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v4",
) -> None:
    """Activate Uniswap V4 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_upsert_exchange(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
        factory=EthereumMainnetUniswapV4.pool_manager.address,
        deployer=None,
    )
    if row.active:
        click.echo("Exchange is already activated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=True,
    )
    _ffi.db_upsert_pool_manager(
        database_path=database_path,
        address=EthereumMainnetUniswapV4.pool_manager.address,
        chain=chain_id,
        kind="uniswap_v4",
        state_view=EthereumMainnetUniswapV4.state_view.address,
        exchange_id=row.id,
    )
    click.echo(f"Activated Uniswap V4 on Ethereum (chain ID {chain_id}).")


# ═══════════════════════════════════════════════════════════════════════════
# Base mainnet — deactivate
# ═══════════════════════════════════════════════════════════════════════════


@deactivate.command("base_aerodrome_v2")
@click.pass_obj
def deactivate_base_aerodrome_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v2",
) -> None:
    """Deactivate Aerodrome V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(f"The database has no entry for Aerodrome V2 on Base (chain ID {chain_id}).")
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Aerodrome V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_aerodrome_v3")
@click.pass_obj
def deactivate_base_aerodrome_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v3",
) -> None:
    """Deactivate Aerodrome V3 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(f"The database has no entry for Aerodrome V3 on Base (chain ID {chain_id}).")
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Aerodrome V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_pancakeswap_v2")
@click.pass_obj
def deactivate_base_pancakeswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v2",
) -> None:
    """Deactivate Pancakeswap V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(
            f"The database has no entry for Pancakeswap V2 on Base (chain ID {chain_id}).",
        )
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Pancakeswap V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_pancakeswap_v3")
@click.pass_obj
def deactivate_base_pancakeswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v3",
) -> None:
    """Deactivate Pancakeswap V3 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(
            f"The database has no entry for Pancakeswap V3 on Base (chain ID {chain_id}).",
        )
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Pancakeswap V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_sushiswap_v2")
@click.pass_obj
def deactivate_base_sushiswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v2",
) -> None:
    """Deactivate Sushiswap V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(f"The database has no entry for Sushiswap V2 on Base (chain ID {chain_id}).")
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Sushiswap V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_sushiswap_v3")
@click.pass_obj
def deactivate_base_sushiswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v3",
) -> None:
    """Deactivate Sushiswap V3 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(f"The database has no entry for Sushiswap V3 on Base (chain ID {chain_id}).")
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Sushiswap V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_swapbased_v2")
@click.pass_obj
def deactivate_base_swapbased_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "swapbased_v2",
) -> None:
    """Deactivate SwapBased V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(f"The database has no entry for SwapBased V2 on Base (chain ID {chain_id}).")
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated SwapBased V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_uniswap_v2")
@click.pass_obj
def deactivate_base_uniswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v2",
) -> None:
    """Deactivate Uniswap V2 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(f"The database has no entry for Uniswap V2 on Base (chain ID {chain_id}).")
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Uniswap V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_uniswap_v3")
@click.pass_obj
def deactivate_base_uniswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v3",
) -> None:
    """Deactivate Uniswap V3 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(f"The database has no entry for Uniswap V3 on Base (chain ID {chain_id}).")
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Uniswap V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_uniswap_v4")
@click.pass_obj
def deactivate_base_uniswap_v4(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v4",
) -> None:
    """Deactivate Uniswap V4 on Base mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(f"The database has no entry for Uniswap V4 on Base (chain ID {chain_id}).")
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Uniswap V4 on Base (chain ID {chain_id}).")


# ═══════════════════════════════════════════════════════════════════════════
# Ethereum mainnet — deactivate
# ═══════════════════════════════════════════════════════════════════════════


@deactivate.command("ethereum_pancakeswap_v2")
@click.pass_obj
def deactivate_ethereum_pancakeswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "pancakeswap_v2",
) -> None:
    """Deactivate Pancakeswap V2 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(
            f"The database has no entry for Pancakeswap V2 on Ethereum (chain ID {chain_id}).",
        )
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Pancakeswap V2 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_pancakeswap_v3")
@click.pass_obj
def deactivate_ethereum_pancakeswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "pancakeswap_v3",
) -> None:
    """Deactivate Pancakeswap V3 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(
            f"The database has no entry for Pancakeswap V3 on Ethereum (chain ID {chain_id}).",
        )
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Pancakeswap V3 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_sushiswap_v2")
@click.pass_obj
def deactivate_ethereum_sushiswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "sushiswap_v2",
) -> None:
    """Deactivate Sushiswap V2 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(
            f"The database has no entry for Sushiswap V2 on Ethereum (chain ID {chain_id}).",
        )
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Sushiswap V2 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_sushiswap_v3")
@click.pass_obj
def deactivate_ethereum_sushiswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "sushiswap_v3",
) -> None:
    """Deactivate Sushiswap V3 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(f"The database has no entry for Sushiswap V3 on Base (chain ID {chain_id}).")
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Sushiswap V3 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_uniswap_v2")
@click.pass_obj
def deactivate_ethereum_uniswap_v2(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v2",
) -> None:
    """Deactivate Uniswap V2 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(
            f"The database has no entry for Uniswap V2 on Ethereum (chain ID {chain_id}).",
        )
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Uniswap V2 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_uniswap_v3")
@click.pass_obj
def deactivate_ethereum_uniswap_v3(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v3",
) -> None:
    """Deactivate Uniswap V3 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(
            f"The database has no entry for Uniswap V3 on Ethereum (chain ID {chain_id}).",
        )
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Uniswap V3 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_uniswap_v4")
@click.pass_obj
def deactivate_ethereum_uniswap_v4(
    bot: Bot,
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v4",
) -> None:
    """Deactivate Uniswap V4 on Ethereum mainnet."""
    database_path = str(bot.config.database.path)
    row = _ffi.db_fetch_exchange_by_name(
        database_path=database_path,
        chain_id=chain_id,
        name=exchange_name,
    )
    if row is None:
        click.echo(
            f"The database has no entry for Uniswap V4 on Ethereum (chain ID {chain_id}).",
        )
        return
    if not row.active:
        click.echo("Exchange is already deactivated.")
        return
    _ffi.db_set_exchange_active(
        database_path=database_path,
        exchange_id=row.id,
        active=False,
    )
    click.echo(f"Deactivated Uniswap V4 on Ethereum (chain ID {chain_id}).")
