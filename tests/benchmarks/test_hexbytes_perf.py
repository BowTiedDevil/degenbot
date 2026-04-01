"""
Benchmark tests comparing FastHexBytes (Rust) with HexBytes (Python).

Run with: uv run pytest tests/benchmarks/test_hexbytes_perf.py --benchmark-only -v

To save results: --benchmark-autosave
To compare: --benchmark-compare
"""

from hexbytes import HexBytes

from degenbot import FastHexBytes

# Test data sizes representing common EVM types
ADDRESS_HEX = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"  # 20 bytes (Ethereum address)
HASH_HEX = (
    "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"  # 32 bytes (tx hash)
)
SHORT_HEX = "0xdeadbeef"  # 4 bytes
EMPTY_HEX = "0x"  # 0 bytes

ADDRESS_BYTES = bytes.fromhex(ADDRESS_HEX[2:])
HASH_BYTES = bytes.fromhex(HASH_HEX[2:])
SHORT_BYTES = bytes.fromhex(SHORT_HEX[2:])


class TestConstructionFromHex:
    """Benchmark construction from hex strings."""

    def test_hexbytes_from_hex_address(self, benchmark):
        result = benchmark(HexBytes, ADDRESS_HEX)
        # HexBytes.hex() returns hex without 0x prefix
        assert result.hex() == ADDRESS_HEX[2:].lower()

    def test_fast_hexbytes_from_hex_address(self, benchmark):
        result = benchmark(FastHexBytes, ADDRESS_HEX)
        # FastHexBytes.hex() returns hex with 0x prefix
        assert result.hex() == ADDRESS_HEX.lower()

    def test_hexbytes_from_hex_hash(self, benchmark):
        result = benchmark(HexBytes, HASH_HEX)
        # HexBytes.hex() returns hex without 0x prefix
        assert result.hex() == HASH_HEX[2:].lower()

    def test_fast_hexbytes_from_hex_hash(self, benchmark):
        result = benchmark(FastHexBytes, HASH_HEX)
        assert result.hex() == HASH_HEX.lower()

    def test_hexbytes_from_hex_short(self, benchmark):
        result = benchmark(HexBytes, SHORT_HEX)
        # HexBytes.hex() returns hex without 0x prefix
        assert result.hex() == SHORT_HEX[2:].lower()

    def test_fast_hexbytes_from_hex_short(self, benchmark):
        result = benchmark(FastHexBytes, SHORT_HEX)
        assert result.hex() == SHORT_HEX.lower()


class TestConstructionFromBytes:
    """Benchmark construction from bytes."""

    def test_hexbytes_from_bytes_address(self, benchmark):
        result = benchmark(HexBytes, ADDRESS_BYTES)
        assert len(result) == 20

    def test_fast_hexbytes_from_bytes_address(self, benchmark):
        result = benchmark(FastHexBytes, ADDRESS_BYTES)
        assert len(result) == 20

    def test_hexbytes_from_bytes_hash(self, benchmark):
        result = benchmark(HexBytes, HASH_BYTES)
        assert len(result) == 32

    def test_fast_hexbytes_from_bytes_hash(self, benchmark):
        result = benchmark(FastHexBytes, HASH_BYTES)
        assert len(result) == 32


class TestHexAccess:
    """Benchmark hex string access - the key advantage of FastHexBytes."""

    def test_hexbytes_hex_address(self, benchmark):
        hb = HexBytes(ADDRESS_HEX)
        result = benchmark(hb.to_0x_hex)
        assert result == ADDRESS_HEX.lower()

    def test_fast_hexbytes_hex_address(self, benchmark):
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(fhb.hex)
        assert result == ADDRESS_HEX.lower()

    def test_hexbytes_hex_hash(self, benchmark):
        hb = HexBytes(HASH_HEX)
        result = benchmark(hb.to_0x_hex)
        assert result == HASH_HEX.lower()

    def test_fast_hexbytes_hex_hash(self, benchmark):
        fhb = FastHexBytes(HASH_HEX)
        result = benchmark(fhb.hex)
        assert result == HASH_HEX.lower()


class TestLength:
    """Benchmark len() calls."""

    def test_hexbytes_len(self, benchmark):
        hb = HexBytes(HASH_HEX)
        result = benchmark(len, hb)
        assert result == 32

    def test_fast_hexbytes_len(self, benchmark):
        fhb = FastHexBytes(HASH_HEX)
        result = benchmark(len, fhb)
        assert result == 32


class TestSlicing:
    """Benchmark slice operations."""

    def test_hexbytes_slice(self, benchmark):
        hb = HexBytes(HASH_HEX)
        result = benchmark(lambda: hb[0:20])
        assert len(result) == 20

    def test_fast_hexbytes_slice(self, benchmark):
        fhb = FastHexBytes(HASH_HEX)
        result = benchmark(lambda: fhb[0:20])
        assert len(result) == 20

    def test_hexbytes_slice_with_step(self, benchmark):
        hb = HexBytes(HASH_HEX)
        result = benchmark(lambda: hb[::2])
        assert len(result) == 16

    def test_fast_hexbytes_slice_with_step(self, benchmark):
        fhb = FastHexBytes(HASH_HEX)
        result = benchmark(lambda: fhb[::2])
        assert len(result) == 16


class TestIteration:
    """Benchmark iteration over bytes."""

    def test_hexbytes_iteration(self, benchmark):
        hb = HexBytes(HASH_HEX)
        result = benchmark(list, hb)
        assert len(result) == 32

    def test_fast_hexbytes_iteration(self, benchmark):
        fhb = FastHexBytes(HASH_HEX)
        result = benchmark(list, fhb)
        assert len(result) == 32


class TestEquality:
    """Benchmark equality comparisons."""

    def test_hexbytes_eq_bytes(self, benchmark):
        hb = HexBytes(ADDRESS_HEX)
        result = benchmark(lambda: hb == ADDRESS_BYTES)
        assert result is True

    def test_fast_hexbytes_eq_bytes(self, benchmark):
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(lambda: fhb == ADDRESS_BYTES)
        assert result is True

    def test_hexbytes_eq_hexbytes(self, benchmark):
        hb1 = HexBytes(ADDRESS_HEX)
        hb2 = HexBytes(ADDRESS_HEX)
        result = benchmark(lambda: hb1 == hb2)
        assert result is True

    def test_fast_hexbytes_eq_fast_hexbytes(self, benchmark):
        fhb1 = FastHexBytes(ADDRESS_HEX)
        fhb2 = FastHexBytes(ADDRESS_HEX)
        result = benchmark(lambda: fhb1 == fhb2)
        assert result is True

    def test_hexbytes_eq_hex_string(self, benchmark):
        hb = HexBytes(ADDRESS_HEX)
        # HexBytes does NOT compare equal to strings (only bytes)
        result = benchmark(lambda: hb == ADDRESS_HEX)
        assert result is False

    def test_fast_hexbytes_eq_hex_string(self, benchmark):
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(lambda: fhb == ADDRESS_HEX)
        assert result is True

    def test_fast_hexbytes_eq_hex_string_no_prefix(self, benchmark):
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(lambda: fhb == ADDRESS_HEX[2:])
        assert result is True

    def test_fast_hexbytes_eq_hex_string_uppercase(self, benchmark):
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(lambda: fhb == ADDRESS_HEX.upper())
        assert result is True


class TestHashing:
    """Benchmark hash computation for use as dict keys."""

    def test_hexbytes_hash(self, benchmark):
        hb = HexBytes(ADDRESS_HEX)
        result = benchmark(hash, hb)
        assert isinstance(result, int)

    def test_fast_hexbytes_hash(self, benchmark):
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(hash, fhb)
        assert isinstance(result, int)


class TestConcatenation:
    """Benchmark concatenation operations."""

    def test_hexbytes_concat_bytes(self, benchmark):
        hb = HexBytes(SHORT_HEX)
        result = benchmark(lambda: hb + SHORT_BYTES)
        assert len(result) == 8

    def test_fast_hexbytes_concat_bytes(self, benchmark):
        fhb = FastHexBytes(SHORT_HEX)
        result = benchmark(lambda: fhb + SHORT_BYTES)
        assert len(result) == 8

    def test_hexbytes_concat_hexbytes(self, benchmark):
        hb1 = HexBytes(SHORT_HEX)
        hb2 = HexBytes(SHORT_HEX)
        result = benchmark(lambda: hb1 + hb2)
        assert len(result) == 8

    def test_fast_hexbytes_concat_fast_hexbytes(self, benchmark):
        fhb1 = FastHexBytes(SHORT_HEX)
        fhb2 = FastHexBytes(SHORT_HEX)
        result = benchmark(lambda: fhb1 + fhb2)
        assert len(result) == 8


class TestBytesConversion:
    """Benchmark conversion to bytes."""

    def test_hexbytes_to_bytes(self, benchmark):
        hb = HexBytes(HASH_HEX)
        result = benchmark(bytes, hb)
        assert len(result) == 32

    def test_fast_hexbytes_to_bytes(self, benchmark):
        fhb = FastHexBytes(HASH_HEX)
        result = benchmark(bytes, fhb)
        assert len(result) == 32


class TestRepr:
    """Benchmark repr/str conversion."""

    def test_hexbytes_repr(self, benchmark):
        hb = HexBytes(ADDRESS_HEX)
        result = benchmark(repr, hb)
        assert ADDRESS_HEX.lower()[2:] in result.lower()

    def test_fast_hexbytes_repr(self, benchmark):
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(repr, fhb)
        assert ADDRESS_HEX.lower()[2:] in result.lower()

    def test_hexbytes_str(self, benchmark):
        hb = HexBytes(ADDRESS_HEX)
        result = benchmark(str, hb)
        # HexBytes str() returns bytes repr, not hex
        assert result.startswith("b'")

    def test_fast_hexbytes_str(self, benchmark):
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(str, fhb)
        # FastHexBytes str() returns hex string with 0x prefix
        assert result == ADDRESS_HEX.lower()


class TestIndexing:
    """Benchmark single-byte indexing."""

    def test_hexbytes_index(self, benchmark):
        hb = HexBytes(HASH_HEX)
        result = benchmark(lambda: hb[0])
        assert isinstance(result, int)

    def test_fast_hexbytes_index(self, benchmark):
        fhb = FastHexBytes(HASH_HEX)
        result = benchmark(lambda: fhb[0])
        assert isinstance(result, int)


class TestRoundtrip:
    """Benchmark roundtrip operations common in EVM code."""

    def test_hexbytes_hexbytes_roundtrip(self, benchmark):
        """Create HexBytes, get hex, create new HexBytes from that hex."""
        hb = HexBytes(ADDRESS_HEX)
        result = benchmark(lambda: HexBytes(hb.hex()))
        assert len(result) == 20

    def test_fast_hexbytes_hexbytes_roundtrip(self, benchmark):
        """Create FastHexBytes, get hex, create new FastHexBytes from that hex."""
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(lambda: FastHexBytes(fhb.hex()))
        assert len(result) == 20

    def test_hexbytes_bytes_hex_roundtrip(self, benchmark):
        """Create HexBytes, convert to bytes, get hex."""
        hb = HexBytes(ADDRESS_HEX)
        result = benchmark(lambda: bytes(hb).hex())
        assert result == ADDRESS_HEX[2:].lower()

    def test_fast_hexbytes_bytes_hex_roundtrip(self, benchmark):
        """Create FastHexBytes, convert to bytes, get hex."""
        fhb = FastHexBytes(ADDRESS_HEX)
        result = benchmark(lambda: bytes(fhb).hex())
        assert result == ADDRESS_HEX[2:].lower()
