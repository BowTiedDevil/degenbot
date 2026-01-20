import random

import pytest
from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address


@pytest.fixture(scope="module")
def random_addresses() -> list[bytes]:
    return [random.getrandbits(160).to_bytes(20, byteorder="big") for _ in range(10_000)]


@pytest.fixture(scope="module")
def checksummed_random_addresses(random_addresses) -> list[ChecksumAddress]:
    return [to_checksum_address(addr) for addr in random_addresses]
