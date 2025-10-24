import degenbot_rs
import hypothesis
import hypothesis.strategies
import pytest
from eth_utils.address import to_checksum_address


@pytest.fixture(scope="session")
def addresses_to_test() -> list[bytes]:
    strategy = hypothesis.strategies.binary(min_size=20, max_size=20)
    return [strategy.example() for _ in range(10_000)]


def test_to_checksum_address(addresses_to_test: list[bytes]):
    for address in addresses_to_test:
        assert degenbot_rs.to_checksum_address(address) == to_checksum_address(address)


@pytest.mark.skip
def test_parallel_checksum_address(addresses_to_test: list[bytes]):
    degenbot_rs.to_checksum_addresses(addresses_to_test)


# @pytest.mark.skip
def test_benchmark_checksums_rs_loop(benchmark, addresses_to_test):
    def run_rs():
        func = degenbot_rs.to_checksum_address
        for address in addresses_to_test:
            func(address)

    benchmark(run_rs)


# @pytest.mark.skip
def test_benchmark_checksums_rs_sequential(benchmark, addresses_to_test):
    def run_rs():
        degenbot_rs.to_checksum_addresses_sequential(addresses_to_test)

    benchmark(run_rs)


# @pytest.mark.skip
def test_benchmark_checksums_rs_parallel(benchmark, addresses_to_test):
    def run_rs():
        degenbot_rs.to_checksum_addresses_parallel(addresses_to_test)

    benchmark(run_rs)


@pytest.mark.skip
def test_benchmark_checksums_py(benchmark, addresses_to_test):
    def run_py():
        func = to_checksum_address
        for address in addresses_to_test:
            func(address)

    benchmark(run_py)
