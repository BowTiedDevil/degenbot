import click
import eth_typing
from sqlalchemy import select

from degenbot.cli import cli
from degenbot.config import CONFIG_FILE, settings
from degenbot.database import db_session
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.pools import PoolManagerTable
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

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=BaseAerodromeV2.factory.address,
            )
        )
        db_session.commit()

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

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=BaseAerodromeV3.factory.address,
            )
        )
        db_session.commit()

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

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=BasePancakeswapV2.factory.address,
            )
        )
        db_session.commit()

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

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=BasePancakeswapV3.factory.address,
                deployer=BasePancakeswapV3.factory.deployer,
            )
        )
        db_session.commit()

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

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=BaseSwapbasedV2.factory.address,
            )
        )
        db_session.commit()

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

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=BaseSushiswapV2.factory.address,
            )
        )
        db_session.commit()

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

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=BaseSushiswapV3.factory.address,
            )
        )
        db_session.commit()

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

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=BaseUniswapV2.factory.address,
            )
        )
        db_session.commit()

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

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=BaseUniswapV3.factory.address,
            )
        )
        db_session.commit()

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

    exchange_kind = "uniswap_v4"

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        exchange = ExchangeTable(
            chain_id=chain_id,
            name=exchange_name,
            active=True,
            factory=BaseUniswapV4.pool_manager.address,
        )
        db_session.add(exchange)
        db_session.flush()

        manager_in_db = db_session.scalar(
            select(PoolManagerTable).where(
                PoolManagerTable.address == BaseUniswapV4.pool_manager.address,
                PoolManagerTable.chain == chain_id,
            )
        )
        if manager_in_db is None:
            db_session.add(
                PoolManagerTable(
                    address=BaseUniswapV4.pool_manager.address,
                    chain=chain_id,
                    kind=exchange_kind,
                    exchange_id=exchange.id,
                    state_view=BaseUniswapV4.state_view.address,
                )
            )

        db_session.commit()

    click.echo(f"Activated Uniswap V4 on Base (chain ID {chain_id}).")


@activate.command("ethereum_pancakeswap_v2")
def activate_ethereum_pancakeswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "pancakeswap_v2",
) -> None:
    """
    Activate Pancakeswap V2 on Ethereum mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=EthereumMainnetPancakeswapV2.factory.address,
            )
        )
        db_session.commit()

    click.echo(f"Activated Pancakeswap V2 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_pancakeswap_v3")
def activate_ethereum_pancakeswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "pancakeswap_v3",
) -> None:
    """
    Activate Pancakeswap V3 on Ethereum mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=EthereumMainnetPancakeswapV3.factory.address,
                deployer=EthereumMainnetPancakeswapV3.factory.deployer,
            )
        )
        db_session.commit()

    click.echo(f"Activated Pancakeswap V3 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_sushiswap_v2")
def activate_ethereum_sushiswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "sushiswap_v2",
) -> None:
    """
    Activate Sushiswap V2 on Ethereum mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=EthereumMainnetSushiswapV2.factory.address,
            )
        )
        db_session.commit()

    click.echo(f"Activated Sushiswap V2 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_sushiswap_v3")
def activate_ethereum_sushiswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "sushiswap_v3",
) -> None:
    """
    Activate Sushiswap V3 on Ethereum mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=EthereumMainnetSushiswapV3.factory.address,
            )
        )
        db_session.commit()

    click.echo(f"Activated Sushiswap V3 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_uniswap_v2")
def activate_ethereum_uniswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v2",
) -> None:
    """
    Activate Uniswap V2 on Ethereum mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=EthereumMainnetUniswapV2.factory.address,
            )
        )
        db_session.commit()

    click.echo(f"Activated Uniswap V2 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_uniswap_v3")
def activate_ethereum_uniswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v3",
) -> None:
    """
    Activate Uniswap V3 on Ethereum mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        db_session.add(
            ExchangeTable(
                chain_id=chain_id,
                name=exchange_name,
                active=True,
                factory=EthereumMainnetUniswapV3.factory.address,
            )
        )
        db_session.commit()

    click.echo(f"Activated Uniswap V3 on Ethereum (chain ID {chain_id}).")


@activate.command("ethereum_uniswap_v4")
def activate_ethereum_uniswap_v4(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v4",
) -> None:
    """
    Activate Uniswap V4 on Ethereum mainnet.
    """

    _check_configured_rpc(chain_id)

    exchange_kind = "uniswap_v4"

    exchange = db_session.scalar(
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
        db_session.commit()

    if exchange is None:
        exchange = ExchangeTable(
            chain_id=chain_id,
            name=exchange_name,
            active=True,
            factory=EthereumMainnetUniswapV4.pool_manager.address,
        )
        db_session.add(exchange)
        db_session.flush()

        manager_in_db = db_session.scalar(
            select(PoolManagerTable).where(
                PoolManagerTable.address == EthereumMainnetUniswapV4.pool_manager.address,
                PoolManagerTable.chain == chain_id,
            )
        )
        if manager_in_db is None:
            db_session.add(
                PoolManagerTable(
                    address=EthereumMainnetUniswapV4.pool_manager.address,
                    chain=chain_id,
                    kind=exchange_kind,
                    exchange_id=exchange.id,
                    state_view=EthereumMainnetUniswapV4.state_view.address,
                )
            )

        db_session.commit()

    click.echo(f"Activated Uniswap V4 on Ethereum (chain ID {chain_id}).")


@deactivate.command("base_aerodrome_v2")
def deactivate_base_aerodrome_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v2",
) -> None:
    """
    Deactivate Aerodrome V2 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Aerodrome V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_aerodrome_v3")
def deactivate_base_aerodrome_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "aerodrome_v3",
) -> None:
    """
    Deactivate Aerodrome V3 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Aerodrome V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_pancakeswap_v2")
def deactivate_base_pancakeswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v2",
) -> None:
    """
    Deactivate Pancakeswap V2 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Pancakeswap V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_pancakeswap_v3")
def deactivate_base_pancakeswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "pancakeswap_v3",
) -> None:
    """
    Deactivate Pancakeswap V3 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Pancakeswap V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_sushiswap_v2")
def deactivate_base_sushiswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v2",
) -> None:
    """
    Deactivate Sushiswap V2 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Sushiswap V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_sushiswap_v3")
def deactivate_base_sushiswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "sushiswap_v3",
) -> None:
    """
    Deactivate Sushiswap V3 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Sushiswap V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_swapbased_v2")
def deactivate_base_swapbased_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "swapbased_v2",
) -> None:
    """
    Deactivate SwapBased V2 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated SwapBased V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_uniswap_v2")
def deactivate_base_uniswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v2",
) -> None:
    """
    Deactivate Uniswap V2 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Uniswap V2 on Base (chain ID {chain_id}).")


@deactivate.command("base_uniswap_v3")
def deactivate_base_uniswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v3",
) -> None:
    """
    Deactivate Uniswap V3 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Uniswap V3 on Base (chain ID {chain_id}).")


@deactivate.command("base_uniswap_v4")
def deactivate_base_uniswap_v4(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.BASE,
    exchange_name: str = "uniswap_v4",
) -> None:
    """
    Deactivate Uniswap V4 on Base mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Uniswap V4 on Base (chain ID {chain_id}).")


@deactivate.command("ethereum_pancakeswap_v2")
def deactivate_ethereum_pancakeswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "pancakeswap_v2",
) -> None:
    """
    Deactivate Pancakeswap V2 on Ethereum mainnet.
    """

    exchange = db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(
            f"The database has no entry for Pancakeswap V2 on Ethereum (chain ID {chain_id})."
        )
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    db_session.commit()

    click.echo(f"Deactivated Pancakeswap V2 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_pancakeswap_v3")
def deactivate_ethereum_pancakeswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "pancakeswap_v3",
) -> None:
    """
    Deactivate Pancakeswap V3 on Ethereum mainnet.
    """

    exchange = db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(
            f"The database has no entry for Pancakeswap V3 on Ethereum (chain ID {chain_id})."
        )
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    db_session.commit()

    click.echo(f"Deactivated Pancakeswap V3 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_sushiswap_v2")
def deactivate_ethereum_sushiswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "sushiswap_v2",
) -> None:
    """
    Deactivate Sushiswap V2 on Ethereum mainnet.
    """

    exchange = db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Sushiswap V2 on Ethereum (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    db_session.commit()

    click.echo(f"Deactivated Sushiswap V2 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_sushiswap_v3")
def deactivate_ethereum_sushiswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "sushiswap_v3",
) -> None:
    """
    Deactivate Sushiswap V3 on Ethereum mainnet.
    """

    exchange = db_session.scalar(
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
    db_session.commit()

    click.echo(f"Deactivated Sushiswap V3 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_uniswap_v2")
def deactivate_ethereum_uniswap_v2(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v2",
) -> None:
    """
    Deactivate Uniswap V2 on Ethereum mainnet.
    """

    exchange = db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Uniswap V2 on Ethereum (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    db_session.commit()

    click.echo(f"Deactivated Uniswap V2 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_uniswap_v3")
def deactivate_ethereum_uniswap_v3(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v3",
) -> None:
    """
    Deactivate Uniswap V3 on Ethereum mainnet.
    """

    exchange = db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Uniswap V3 on Ethereum (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    db_session.commit()

    click.echo(f"Deactivated Uniswap V3 on Ethereum (chain ID {chain_id}).")


@deactivate.command("ethereum_uniswap_v4")
def deactivate_ethereum_uniswap_v4(
    chain_id: eth_typing.ChainId = eth_typing.ChainId.ETH,
    exchange_name: str = "uniswap_v4",
) -> None:
    """
    Deactivate Uniswap V4 on Ethereum mainnet.
    """

    exchange = db_session.scalar(
        select(ExchangeTable).where(
            ExchangeTable.chain_id == chain_id,
            ExchangeTable.name == exchange_name,
        )
    )

    if exchange is None:
        click.echo(f"The database has no entry for Uniswap V4 on Ethereum (chain ID {chain_id}).")
        return

    if not exchange.active:
        click.echo("Exchange is already deactivated.")
        return
    exchange.active = False
    db_session.commit()

    click.echo(f"Deactivated Uniswap V4 on Ethereum (chain ID {chain_id}).")
