import pytest
import dotenv


@pytest.fixture(scope="session", autouse=True)
def load_env() -> dict:
    env_file = dotenv.find_dotenv("tests.env")
    return dotenv.dotenv_values(env_file)
