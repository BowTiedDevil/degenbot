import pytest
import dotenv
import web3


@pytest.fixture(scope="session")
def load_env() -> dict:
    env_file = dotenv.find_dotenv("tests.env")
    return dotenv.dotenv_values(env_file)


# Set up a web3 connection to Ankr endpoint
@pytest.fixture(scope="session")
def ankr_archive_web3() -> web3.Web3:
    w3 = web3.Web3(web3.HTTPProvider("https://rpc.ankr.com/eth"))
    return w3
