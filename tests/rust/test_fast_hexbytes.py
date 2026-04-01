"""
Property-based tests for FastHexBytes type.

Uses Hypothesis to generate random bytes and verify FastHexBytes behavior
matches expected Python bytes behavior.
"""

import array
import struct

import hypothesis
import hypothesis.strategies as st
import pytest

from degenbot import FastHexBytes

# Strategy for generating random bytes
bytes_strategy = st.binary(min_size=0, max_size=256)


class TestFastHexBytesBytesParity:
    """
    Test FastHexBytes matches Python bytes behavior.
    """

    @hypothesis.given(data=bytes_strategy)
    def test_bytes_conversion(self, data: bytes) -> None:
        """
        FastHexBytes converts to same bytes as input.
        """
        fhb = FastHexBytes(data)
        assert bytes(fhb) == data

    @hypothesis.given(data=bytes_strategy)
    def test_length(self, data: bytes) -> None:
        """
        FastHexBytes length matches input bytes length.
        """
        fhb = FastHexBytes(data)
        assert len(fhb) == len(data)

    @hypothesis.given(data=bytes_strategy)
    def test_hex_format(self, data: bytes) -> None:
        """
        FastHexBytes hex() returns 0x-prefixed lowercase hex string.
        """
        fhb = FastHexBytes(data)
        expected_hex = "0x" + data.hex()
        assert fhb.hex() == expected_hex
        assert fhb.to_0x_hex() == expected_hex

    @hypothesis.given(data=bytes_strategy)
    def test_str_representation(self, data: bytes) -> None:
        """
        FastHexBytes str() returns 0x-prefixed hex string.
        """
        fhb = FastHexBytes(data)
        assert str(fhb) == "0x" + data.hex()

    @hypothesis.given(data=bytes_strategy)
    def test_repr_format(self, data: bytes) -> None:
        """
        FastHexBytes repr() follows expected format.
        """
        fhb = FastHexBytes(data)
        expected = f"FastHexBytes('0x{data.hex()}')"
        assert repr(fhb) == expected

    @hypothesis.given(data=bytes_strategy)
    def test_iteration(self, data: bytes) -> None:
        """
        FastHexBytes iteration yields same values as bytes iteration.
        """
        fhb = FastHexBytes(data)
        assert list(fhb) == list(data)

    @hypothesis.given(data=bytes_strategy, index=st.integers(min_value=-256, max_value=256))
    def test_indexing(self, data: bytes, index: int) -> None:
        """
        FastHexBytes indexing matches bytes indexing.
        """
        if not data:
            # Empty bytes cannot be indexed
            return

        fhb = FastHexBytes(data)

        # Python bytes index error handling
        try:
            expected = data[index]
        except IndexError:
            with pytest.raises(IndexError):
                _ = fhb[index]
        else:
            assert fhb[index] == expected

    @hypothesis.given(data=bytes_strategy)
    def test_slicing(self, data: bytes) -> None:
        """
        FastHexBytes slicing returns same bytes as input slicing.
        """
        fhb = FastHexBytes(data)

        # Test various slices
        assert fhb[0:1] == data[0:1]
        assert fhb[1:3] == data[1:3]
        assert fhb[:] == data[:]
        assert fhb[::2] == data[::2]
        assert fhb[::-1] == data[::-1]

    @hypothesis.given(data=bytes_strategy)
    def test_equality_with_bytes(self, data: bytes) -> None:
        """
        FastHexBytes equals equivalent bytes.
        """
        fhb = FastHexBytes(data)
        assert fhb == data
        assert fhb == bytearray(data)

    @hypothesis.given(data=bytes_strategy)
    def test_equality_with_hex_string(self, data: bytes) -> None:
        """
        FastHexBytes equals its hex string representation.
        """
        fhb = FastHexBytes(data)
        hex_with_prefix = "0x" + data.hex()
        hex_without_prefix = data.hex()

        assert fhb == hex_with_prefix
        assert fhb == hex_without_prefix

    @hypothesis.given(data=bytes_strategy)
    def test_inequality_with_different_bytes(self, data: bytes) -> None:
        """
        FastHexBytes does not equal different bytes.
        """
        fhb = FastHexBytes(data)
        different = data + b"\x00" if data else b"\x01"
        assert fhb != different

    @hypothesis.given(data=bytes_strategy)
    def test_truthiness(self, data: bytes) -> None:
        """
        FastHexBytes truthiness matches bytes truthiness.
        """
        fhb = FastHexBytes(data)
        assert bool(fhb) == bool(data)


class TestFastHexBytesConstruction:
    """
    Test FastHexBytes construction from various types.
    """

    @hypothesis.given(data=bytes_strategy)
    def test_from_bytes(self, data: bytes) -> None:
        """
        FastHexBytes can be constructed from bytes.
        """
        fhb = FastHexBytes(data)
        assert bytes(fhb) == data

    @hypothesis.given(data=bytes_strategy)
    def test_from_hex_string_with_prefix(self, data: bytes) -> None:
        """
        FastHexBytes can be constructed from hex string with 0x prefix.
        """
        hex_str = "0x" + data.hex()
        fhb = FastHexBytes(hex_str)
        assert bytes(fhb) == data

    @hypothesis.given(data=bytes_strategy)
    def test_from_hex_string_without_prefix(self, data: bytes) -> None:
        """
        FastHexBytes can be constructed from hex string without 0x prefix.
        """
        hex_str = data.hex()
        fhb = FastHexBytes(hex_str)
        assert bytes(fhb) == data

    @hypothesis.given(data=bytes_strategy)
    def test_from_bytearray(self, data: bytes) -> None:
        """
        FastHexBytes can be constructed from bytearray.
        """
        ba = bytearray(data)
        fhb = FastHexBytes(ba)
        assert bytes(fhb) == data

    @hypothesis.given(data=bytes_strategy)
    def test_from_int(self, data: bytes) -> None:
        """
        FastHexBytes can be constructed from int (if data represents a valid int).
        """
        if len(data) <= 8:  # Only test reasonably sized integers
            n = int.from_bytes(data, "big") if data else 0
            fhb = FastHexBytes(n)
            # Int construction pads to even-length hex (e.g., 0x0 -> 0x00)
            expected_hex = f"0x{n:x}"
            if len(expected_hex) % 2 == 1:  # Odd length means odd number of hex digits, need to pad
                expected_hex = f"0x0{n:x}"
            assert fhb.hex() == expected_hex

    @hypothesis.given(data=bytes_strategy)
    def test_copy_constructor(self, data: bytes) -> None:
        """
        FastHexBytes can be constructed from another FastHexBytes.
        """
        fhb1 = FastHexBytes(data)
        fhb2 = FastHexBytes(fhb1)
        assert fhb1 == fhb2
        assert bytes(fhb1) == bytes(fhb2)


class TestFastHexBytesOperations:
    """
    Test FastHexBytes operations.
    """

    @hypothesis.given(data1=bytes_strategy, data2=bytes_strategy)
    def test_concatenation_with_bytes(self, data1: bytes, data2: bytes) -> None:
        """
        FastHexBytes + bytes returns FastHexBytes with concatenated data.
        """
        fhb = FastHexBytes(data1)
        result = fhb + data2

        assert isinstance(result, FastHexBytes)
        assert bytes(result) == data1 + data2

    @hypothesis.given(data1=bytes_strategy, data2=bytes_strategy)
    def test_concatenation_with_fast_hexbytes(self, data1: bytes, data2: bytes) -> None:
        """
        FastHexBytes + FastHexBytes returns FastHexBytes with concatenated data.
        """
        fhb1 = FastHexBytes(data1)
        fhb2 = FastHexBytes(data2)
        result = fhb1 + fhb2

        assert isinstance(result, FastHexBytes)
        assert bytes(result) == data1 + data2

    @hypothesis.given(data=bytes_strategy, n=st.integers(min_value=0, max_value=10))
    def test_multiplication(self, data: bytes, n: int) -> None:
        """
        FastHexBytes * n returns FastHexBytes repeated n times.
        """
        fhb = FastHexBytes(data)
        result = fhb * n

        assert isinstance(result, FastHexBytes)
        assert bytes(result) == data * n

    @hypothesis.given(data=bytes_strategy, n=st.integers(min_value=0, max_value=10))
    def test_reverse_multiplication(self, data: bytes, n: int) -> None:
        """
        n * FastHexBytes returns FastHexBytes repeated n times.
        """
        fhb = FastHexBytes(data)
        result = n * fhb

        assert isinstance(result, FastHexBytes)
        assert bytes(result) == data * n


class TestFastHexBytesHash:
    """
    Test FastHexBytes hashing behavior.
    """

    @hypothesis.given(data=bytes_strategy)
    def test_hashable(self, data: bytes) -> None:
        """
        FastHexBytes can be hashed and used as dict key.
        """
        fhb = FastHexBytes(data)
        # Should not raise
        h = hash(fhb)
        assert isinstance(h, int)

    @hypothesis.given(data=bytes_strategy)
    def test_same_content_same_hash(self, data: bytes) -> None:
        """
        FastHexBytes with same content have same hash.
        """
        fhb1 = FastHexBytes(data)
        fhb2 = FastHexBytes(data)
        assert hash(fhb1) == hash(fhb2)

    @hypothesis.given(data=bytes_strategy)
    def test_can_use_as_dict_key(self, data: bytes) -> None:
        """
        FastHexBytes can be used as dictionary key.
        """
        fhb = FastHexBytes(data)
        d = {fhb: "value"}
        assert d[fhb] == "value"


class TestFastHexBytesHexComparison:
    """
    Test comparing FastHexBytes hex representation with Python's hex().
    """

    @hypothesis.given(data=bytes_strategy)
    def test_hex_matches_python_hex(self, data: bytes) -> None:
        """
        FastHexBytes.hex() matches Python's '0x' + bytes.hex().
        """
        fhb = FastHexBytes(data)
        python_hex = "0x" + data.hex()

        assert fhb.hex() == python_hex
        assert fhb.to_0x_hex() == python_hex

    @hypothesis.given(data=bytes_strategy)
    def test_hex_is_lowercase(self, data: bytes) -> None:
        """
        FastHexBytes.hex() always returns lowercase.
        """
        fhb = FastHexBytes(data)
        hex_result = fhb.hex()

        # Should be lowercase (except for '0x' prefix)
        assert hex_result[2:] == hex_result[2:].lower()


class TestFastHexBytesBufferProtocol:
    """
    Test FastHexBytes buffer protocol implementation.

    The buffer protocol allows zero-copy access to the underlying bytes,
    enabling operations like memoryview, bytearray construction, etc.
    """

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_creation(self, data: bytes) -> None:
        """
        memoryview can be created from FastHexBytes.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv is not None
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_length(self, data: bytes) -> None:
        """
        memoryview length matches FastHexBytes length.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert len(mv) == len(data)
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_tobytes(self, data: bytes) -> None:
        """
        memoryview.tobytes() returns original bytes.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.tobytes() == data
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_tolist(self, data: bytes) -> None:
        """
        memoryview.tolist() returns list of byte values.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.tolist() == list(data)
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_readonly(self, data: bytes) -> None:
        """
        memoryview of FastHexBytes is read-only.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.readonly is True
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_format(self, data: bytes) -> None:
        """
        memoryview format is 'B' (unsigned byte).
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.format == "B"
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_itemsize(self, data: bytes) -> None:
        """
        memoryview itemsize is 1 (single byte).
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.itemsize == 1
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_ndim(self, data: bytes) -> None:
        """
        memoryview ndim is 1 (one-dimensional buffer).
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.ndim == 1
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_shape(self, data: bytes) -> None:
        """
        memoryview shape matches bytes length.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.shape == (len(data),)
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_strides(self, data: bytes) -> None:
        """
        memoryview strides is (1,) for contiguous bytes.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.strides == (1,)
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_nbytes(self, data: bytes) -> None:
        """
        memoryview nbytes matches total byte count.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.nbytes == len(data)
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_contiguous(self, data: bytes) -> None:
        """
        memoryview is C-contiguous.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.contiguous is True
        assert mv.c_contiguous is True
        mv.release()

    @hypothesis.given(data=bytes_strategy, index=st.integers(min_value=-256, max_value=256))
    def test_memoryview_indexing(self, data: bytes, index: int) -> None:
        """
        memoryview indexing matches bytes indexing.
        """
        if not data:
            return

        fhb = FastHexBytes(data)
        mv = memoryview(fhb)

        try:
            expected = data[index]
        except IndexError:
            with pytest.raises(IndexError):
                _ = mv[index]
        else:
            assert mv[index] == expected
        finally:
            mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_slicing(self, data: bytes) -> None:
        """
        memoryview slicing returns memoryview with sliced data.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)

        # Various slice patterns
        slices = [
            slice(0, len(data)),
            slice(0, min(1, len(data))),
            slice(1, min(3, len(data))) if len(data) > 1 else slice(0, 0),
            slice(None, None, 2),
            slice(None, None, -1),
        ]

        for s in slices:
            assert mv[s].tobytes() == data[s]

        mv.release()

    @hypothesis.given(
        data=bytes_strategy,
        start=st.integers(min_value=-256, max_value=256),
        stop=st.integers(min_value=-256, max_value=256),
        step=st.integers(min_value=-10, max_value=10).filter(lambda x: x != 0),
    )
    def test_memoryview_arbitrary_slicing(
        self, data: bytes, start: int, stop: int, step: int
    ) -> None:
        """
        memoryview arbitrary slicing matches bytes slicing.
        """
        if not data:
            return

        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        s = slice(start, stop, step)

        assert mv[s].tobytes() == data[s]
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_bytearray_from_buffer(self, data: bytes) -> None:
        """
        bytearray can be constructed from FastHexBytes via buffer protocol.
        """
        fhb = FastHexBytes(data)
        ba = bytearray(fhb)
        assert ba == data
        assert bytes(ba) == data

    @hypothesis.given(data=bytes_strategy)
    def test_bytes_from_memoryview(self, data: bytes) -> None:
        """
        bytes can be constructed from memoryview of FastHexBytes.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        result = bytes(mv)
        assert result == data
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_buffer_protocol_comparison(self, data: bytes) -> None:
        """
        Buffer contents compare equal to original bytes.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)

        # Compare via tobytes
        assert mv.tobytes() == data

        # Compare each byte
        for i in range(len(data)):
            assert mv[i] == data[i]

        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_hex(self, data: bytes) -> None:
        """
        memoryview.hex() matches bytes.hex().
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        assert mv.hex() == data.hex()
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_cast_to_bytes(self, data: bytes) -> None:
        """
        memoryview can be cast to 'B' format (no-op for bytes).
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)
        cast_mv = mv.cast("B")
        assert cast_mv.tobytes() == data
        cast_mv.release()
        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_buffer_with_struct_unpack(self, data: bytes) -> None:
        """
        Buffer can be used with struct.unpack for binary data.
        """

        fhb = FastHexBytes(data)
        mv = memoryview(fhb)

        # Pack the same data and compare
        if len(data) >= 1:
            # Can unpack at least one byte
            unpacked = struct.unpack("B", mv[:1])
            assert unpacked[0] == data[0]

        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_write_raises(self, data: bytes) -> None:
        """
        Writing to read-only memoryview raises TypeError.
        """
        if not data:
            return

        fhb = FastHexBytes(data)
        mv = memoryview(fhb)

        with pytest.raises(TypeError, match="cannot modify read-only memory"):
            mv[0] = 0xFF

        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_context_manager(self, data: bytes) -> None:
        """
        memoryview works as context manager for automatic release.
        """
        fhb = FastHexBytes(data)

        with memoryview(fhb) as mv:
            assert mv.tobytes() == data
            assert len(mv) == len(data)

    @hypothesis.given(data1=bytes_strategy, data2=bytes_strategy)
    def test_memoryview_comparison(self, data1: bytes, data2: bytes) -> None:
        """
        memoryview objects can be compared for equality.
        """
        fhb1 = FastHexBytes(data1)
        fhb2 = FastHexBytes(data2)

        mv1 = memoryview(fhb1)
        mv2 = memoryview(fhb2)

        assert (mv1 == mv2) == (data1 == data2)

        mv1.release()
        mv2.release()

    @hypothesis.given(data=bytes_strategy)
    def test_buffer_in_array_module(self, data: bytes) -> None:
        """
        Buffer works with array module.
        """

        fhb = FastHexBytes(data)
        mv = memoryview(fhb)

        # Can create array from buffer
        arr = array.array("B", mv)
        assert arr.tobytes() == data

        mv.release()

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_obj_attribute(self, data: bytes) -> None:
        """
        memoryview.obj returns the originating FastHexBytes.
        """
        fhb = FastHexBytes(data)
        mv = memoryview(fhb)

        # obj should be the FastHexBytes instance
        assert mv.obj is fhb

        mv.release()


class TestFastHexBytesRoundTrip:
    """
    Test round-trip conversions to/from FastHexBytes.
    """

    @hypothesis.given(data=bytes_strategy)
    def test_bytes_roundtrip(self, data: bytes) -> None:
        """
        bytes -> FastHexBytes -> bytes returns original.
        """
        fhb = FastHexBytes(data)
        result = bytes(fhb)
        assert result == data

    @hypothesis.given(data=bytes_strategy)
    def test_bytearray_roundtrip(self, data: bytes) -> None:
        """
        bytearray -> FastHexBytes -> bytearray returns original.
        """
        original = bytearray(data)
        fhb = FastHexBytes(original)
        result = bytearray(fhb)
        assert result == original

    @hypothesis.given(data=bytes_strategy)
    def test_hex_string_with_prefix_roundtrip(self, data: bytes) -> None:
        """
        hex string (0x prefix) -> FastHexBytes -> hex string returns original.
        """
        hex_str = "0x" + data.hex()
        fhb = FastHexBytes(hex_str)
        result = fhb.hex()
        assert result == hex_str

    @hypothesis.given(data=bytes_strategy)
    def test_hex_string_without_prefix_roundtrip(self, data: bytes) -> None:
        """
        hex string (no prefix) -> FastHexBytes -> hex string returns 0x-prefixed.
        """
        hex_str = data.hex()
        fhb = FastHexBytes(hex_str)
        result = fhb.hex()
        # Result is always 0x-prefixed
        assert result == "0x" + hex_str

    @hypothesis.given(n=st.integers(min_value=0, max_value=2**256 - 1))
    def test_int_roundtrip(self, n: int) -> None:
        """
        int -> FastHexBytes -> int returns original.
        """
        fhb = FastHexBytes(n)
        # Convert back to int via hex
        result = int(fhb.hex(), 16)
        assert result == n

    @hypothesis.given(data=bytes_strategy)
    def test_fast_hexbytes_copy_roundtrip(self, data: bytes) -> None:
        """
        FastHexBytes -> FastHexBytes (copy) preserves data.
        """
        fhb1 = FastHexBytes(data)
        fhb2 = FastHexBytes(fhb1)
        assert bytes(fhb1) == bytes(fhb2)
        assert fhb1 == fhb2

    @hypothesis.given(data=bytes_strategy)
    def test_memoryview_roundtrip(self, data: bytes) -> None:
        """
        memoryview -> bytes -> FastHexBytes preserves data.
        """
        fhb = FastHexBytes(data)
        with memoryview(fhb) as mv:
            result = bytes(mv)
        assert result == data

    @hypothesis.given(data=bytes_strategy)
    def test_str_repr_roundtrip(self, data: bytes) -> None:
        """
        str(FastHexBytes) can reconstruct equivalent FastHexBytes.
        """
        fhb1 = FastHexBytes(data)
        hex_str = str(fhb1)
        fhb2 = FastHexBytes(hex_str)
        assert fhb1 == fhb2
        assert bytes(fhb1) == bytes(fhb2)

    @hypothesis.given(data=bytes_strategy)
    def test_repr_eval_roundtrip(self, data: bytes) -> None:
        """
        repr(FastHexBytes) can be eval'd to reconstruct (non-empty only).

        Note: Empty bytes repr is 'FastHexBytes(0x)' which isn't valid Python
        syntax, so we skip that edge case.
        """
        if not data:
            return

        fhb1 = FastHexBytes(data)
        repr_str = repr(fhb1)
        fhb2 = eval(repr_str)
        assert fhb1 == fhb2
        assert bytes(fhb1) == bytes(fhb2)

    @hypothesis.given(data=bytes_strategy)
    def test_slice_roundtrip(self, data: bytes) -> None:
        """
        Slicing and re-joining preserves data.
        """
        if len(data) < 2:
            return

        fhb = FastHexBytes(data)
        mid = len(data) // 2

        # Split and rejoin
        left = FastHexBytes(fhb[:mid])
        right = FastHexBytes(fhb[mid:])
        rejoined = left + right

        assert bytes(rejoined) == data

    @hypothesis.given(data=bytes_strategy, n=st.integers(min_value=1, max_value=5))
    def test_multiply_then_divide_roundtrip(self, data: bytes, n: int) -> None:
        """
        Multiplying and then extracting original-sized chunk preserves data.
        """
        fhb = FastHexBytes(data)
        multiplied = fhb * n

        # Extract the first chunk
        first_chunk = multiplied[: len(data)]
        assert bytes(first_chunk) == data
