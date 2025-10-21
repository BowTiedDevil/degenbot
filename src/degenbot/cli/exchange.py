import click
import eth_typing
from sqlalchemy import select

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.config import CONFIG_FILE, settings
from degenbot.database import default_db_session
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.pools import PoolManagerTable


@cli.group()
def exchange() -> None:
    """
    Exchange commands
    """


@exchange.group
def activate() -> None:
    """
    Activate the exchange. Liquidity pools for all activated exchanges are included when running
    "pool update".
    """


@exchange.group
def deactivate() -> None:
    """
    Deactivate the exchange. Liquidity pools for all deactivated exchanges are not included when
    running "pool update".
    """


def _check_configured_rpc(chain_id: int) -> None:
    if chain_id not in settings.rpc:
        click.echo(
            f"An RPC for chain ID {chain_id} is not defined. Add one to {CONFIG_FILE} so that updates can be performed from the console."  # noqa: E501
        )


@activate.command("base_aerodrome_v2")
def activate_base_aerodrome_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v2",
) -> None:
    """
    Activate Aerodrome V2 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        default_db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=get_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da"),
            )
        )
        default_db_session.commit()

    click.echo(f"Activated Aerodrome V2 on Base (chain ID {chain_id}).")


@activate.command("base_aerodrome_v3")
def activate_base_aerodrome_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v3",
) -> None:
    """
    Activate Aerodrome V2 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        default_db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=get_checksum_address("0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"),
            )
        )
        default_db_session.commit()

    click.echo(f"Activated Aerodrome V3 on Base (chain ID {chain_id}).")


@activate.command("base_pancakeswap_v2")
def activate_base_pancakeswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v2",
) -> None:
    """
    Activate Pancakeswap V2 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        default_db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=get_checksum_address("0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E"),
            )
        )
        default_db_session.commit()

    click.echo(f"Activated Pancakeswap V2 on Base (chain ID {chain_id}).")


@activate.command("base_pancakeswap_v3")
def activate_base_pancakeswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v3",
) -> None:
    """
    Activate Pancakeswap V3 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        default_db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=get_checksum_address("0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
                deployer=get_checksum_address("0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"),
            )
        )
        default_db_session.commit()

    click.echo(f"Activated Pancakeswap V3 on Base (chain ID {chain_id}).")


@activate.command("base_swapbased_v2")
def activate_base_swapbased_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "swapbased_v2",
) -> None:
    """
    Activate SwapBased V2 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        default_db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=get_checksum_address("0x04C9f118d21e8B767D2e50C946f0cC9F6C367300"),
            )
        )
        default_db_session.commit()

    click.echo(f"Activated SwapBased V2 on Base (chain ID {chain_id}).")


@activate.command("base_sushiswap_v2")
def activate_base_sushiswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v2",
) -> None:
    """
    Activate Sushiswap V2 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        default_db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=get_checksum_address("0x71524B4f93c58fcbF659783284E38825f0622859"),
            )
        )
        default_db_session.commit()

    click.echo(f"Activated Sushiswap V2 on Base (chain ID {chain_id}).")


@activate.command("base_sushiswap_v3")
def activate_base_sushiswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v3",
) -> None:
    """
    Activate Sushiswap V3 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        default_db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=get_checksum_address("0xc35DADB65012eC5796536bD9864eD8773aBc74C4"),
            )
        )
        default_db_session.commit()

    click.echo(f"Activated Sushiswap V3 on Base (chain ID {chain_id}).")


@activate.command("base_uniswap_v2")
def activate_base_uniswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v2",
) -> None:
    """
    Activate Uniswap V2 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        default_db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=get_checksum_address("0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6"),
            )
        )
        default_db_session.commit()

    click.echo(f"Activated Uniswap V2 on Base (chain ID {chain_id}).")


@activate.command("base_uniswap_v3")
def activate_base_uniswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v3",
) -> None:
    """
    Activate Uniswap V3 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        default_db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=get_checksum_address("0x33128a8fC17869897dcE68Ed026d694621f6FDfD"),
            )
        )
        default_db_session.commit()

    click.echo(f"Activated Uniswap V3 on Base (chain ID {chain_id}).")


@activate.command("base_uniswap_v4")
def activate_base_uniswap_v4(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v4",
) -> None:
    """
    Activate Uniswap V4 on Base mainnet.
    """

    _check_configured_rpc(chain_id)

    pool_manager_address = get_checksum_address("0x498581fF718922c3f8e6A244956aF099B2652b2b")
    exchange_kind = "uniswap_v4"

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )
    if exchange is not None:
        if exchange.active:
            click.echo("Exchange is already activated.")
            return
        exchange.active = True
        default_db_session.commit()

    if exchange is None:
        exchange = ExchangeTable(
            chain_id=chain_id,
            name=exchange_name,
            active=True,
            factory=pool_manager_address,
        )
        default_db_session.add(exchange)
        default_db_session.flush()

        manager_in_db = default_db_session.scalar(
            select(PoolManagerTable).where(PoolManagerTable.address == pool_manager_address)
        )
        if manager_in_db is None:
            default_db_session.add(
                PoolManagerTable(
                    address=pool_manager_address,
                    chain=chain_id,
                    kind=exchange_kind,
                    exchange_id=exchange.id,
                )
            )

        default_db_session.commit()

    click.echo(f"Activated Uniswap V4 on Base (chain ID {chain_id}).")


@deactivate.command("base_aerodrome_v2")
def deactivate_base_aerodrome_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v2",
) -> None:
    """
    Deactivate Aerodrome V2 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Aerodrome V2 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated Aerodrome V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_aerodrome_v3")
def deactivate_base_aerodrome_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v3",
) -> None:
    """
    Deactivate Aerodrome V3 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Aerodrome V3 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated Aerodrome V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_pancakeswap_v2")
def deactivate_base_pancakeswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v2",
) -> None:
    """
    Deactivate Pancakeswap V2 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Pancakeswap V2 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated Pancakeswap V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_pancakeswap_v3")
def deactivate_base_pancakeswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v3",
) -> None:
    """
    Deactivate Pancakeswap V3 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Pancakeswap V3 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated Pancakeswap V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_sushiswap_v2")
def deactivate_base_sushiswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v2",
) -> None:
    """
    Deactivate Sushiswap V2 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Sushiswap V2 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated Sushiswap V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_sushiswap_v3")
def deactivate_base_sushiswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v3",
) -> None:
    """
    Deactivate Sushiswap V3 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Sushiswap V3 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated Sushiswap V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_swapbased_v2")
def deactivate_base_swapbased_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "swapbased_v2",
) -> None:
    """
    Deactivate SwapBased V2 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for SwapBased V2 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated SwapBased V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_uniswap_v2")
def deactivate_base_uniswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v2",
) -> None:
    """
    Deactivate Uniswap V2 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Uniswap V2 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated Uniswap V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_uniswap_v3")
def deactivate_base_uniswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v3",
) -> None:
    """
    Deactivate Uniswap V3 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Uniswap V3 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated Uniswap V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_uniswap_v4")
def deactivate_base_uniswap_v4(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v4",
) -> None:
    """
    Deactivate Uniswap V4 on Base mainnet.
    """

    exchange = default_db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Uniswap V4 on Base (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    default_db_session.commit()

    click.echo(f"Deactivated Uniswap V4 on Base (chain ID {chain_id}).")
