# pragma: no cover

import pytest
import dotenv
import web3


@pytest.fixture(scope="session")
def load_env() -> dict:
    env_file = dotenv.find_dotenv("tests.env")
    return dotenv.dotenv_values(env_file)


# Set up a web3 connection to Ankr endpoint
@pytest.fixture(scope="session")
def ankr_archive_web3(load_env) -> web3.Web3:
    try:
        ANKR_URL = f"https://rpc.ankr.com/eth/{load_env['ANKR_API_KEY']}"
    except KeyError:
        ANKR_URL = "https://rpc.ankr.com/eth/"

    w3 = web3.Web3(web3.HTTPProvider(ANKR_URL))
    return w3


# Set up a web3 connection to local geth node
@pytest.fixture(scope="session")
def local_web3() -> web3.Web3:
    w3 = web3.Web3(web3.HTTPProvider("http://localhost:8545"))
    return w3


# Provide a default Web3 object for degenbot
@pytest.fixture(scope="session", autouse=True)
def setup_degenbot_web3(local_web3: web3.Web3) -> None:
    import degenbot

    degenbot.set_web3(local_web3)
