"""Parity + round-trip tests for the Rust Solady LibZip (FastLZ) port.

Cross-checks the Rust implementation (`degenbot.degenbot_rs.flz_compress` /
`flz_decompress`, backed by `degenbot_core::libzip`) against the original
Python oracle (`degenbot.utils.solady.libzip`) and asserts the lossless
round-trip invariant under hypothesis-generated fuzz inputs.
"""

import hypothesis
import hypothesis.strategies as st
from hexbytes import HexBytes

from degenbot.degenbot_rs import flz_compress as rust_compress
from degenbot.degenbot_rs import flz_decompress as rust_decompress
from degenbot.utils.solady.libzip import flz_compress as py_compress
from degenbot.utils.solady.libzip import flz_decompress as py_decompress

# The canonical Solady prose fixture (shared with tests/utils/test_solady.py).
# Built from implicit-concatenated byte literals so no single source line
# exceeds the ruff line-length limit; the byte content is identical to the
# original single-string form.
NETWORK_SPIRITUALITY = (
    b"New wave digital art should not be judged solely on aesthetic merit but just as "
    b"importantly for its ability to develop a total vision of the Wired and above all create "
    b"network spirituality. Ancient Greek art is held in the highest regard because it developed "
    b"a whole mythology that shaped religion, morality and way of life. Thought is implicit in "
    b"the art works of Ancient Greece but not sufficiently disengaged from the sensory to "
    b"reflect its own products. The problem of modernity is the development of rational self "
    b"reflection. The beauty of art appears in a form which contrasts abstract thought. Thus, "
    b"abstract thought destroys the naive/sensuous appreciation of art in exerting its nature. "
    b"Reality is slain by comprehension. Proper interaction with the wired involves the trance "
    b"separation of real abstract thought and accelerates externalisation into pure intuition, "
    b"embodying the network and unselfconsciously drawing out truths from the collective "
    b"noosphere. Through this being on the wired we achieve a return to naive, unselfconscious "
    b"interaction. The individual ego is sacrificed into the collective noosphere, uniting us "
    b"under a totalising spirit. The best art in the wired is not only beautiful but produces a "
    b"network spirituality. I long for Network Spirituality!"
)


class TestRustPythonParity:
    """The Rust port must match the Python oracle byte-for-byte."""

    def test_compress_matches_python_on_canonical_text(self):
        assert rust_compress(NETWORK_SPIRITUALITY) == py_compress(NETWORK_SPIRITUALITY)

    def test_decompress_matches_python_on_canonical_text(self):
        compressed = py_compress(NETWORK_SPIRITUALITY)
        assert rust_decompress(compressed) == py_decompress(compressed)

    def test_compress_empty(self):
        assert rust_compress(b"") == HexBytes(b"")

    def test_decompress_empty(self):
        assert rust_decompress(b"") == HexBytes(b"")

    def test_decompress_python_compressed_is_original(self):
        # Rust decompressor reads Python-compressed output.
        compressed = py_compress(NETWORK_SPIRITUALITY)
        assert rust_decompress(compressed) == NETWORK_SPIRITUALITY

    def test_decompress_rust_compressed_is_original(self):
        # Python decompressor reads Rust-compressed output (cross-impl interop).
        compressed = rust_compress(NETWORK_SPIRITUALITY)
        assert py_decompress(compressed) == NETWORK_SPIRITUALITY


class TestRoundTrip:
    """compress → decompress is lossless for any input."""

    @hypothesis.given(data=st.binary())
    @hypothesis.settings(max_examples=500)
    def test_rust_roundtrip(self, data: bytes):
        compressed = rust_compress(data)
        assert rust_decompress(compressed).hex() == data.hex()

    @hypothesis.given(data=st.binary())
    @hypothesis.settings(max_examples=500)
    def test_python_roundtrip(self, data: bytes):
        # Sanity-check the oracle itself round-trips (guards against a broken
        # oracle making the parity comparison meaningless).
        compressed = py_compress(data)
        assert py_decompress(compressed).hex() == data.hex()


class TestFuzzParity:
    """Fuzz: Rust compress output must equal Python compress output."""

    @hypothesis.given(data=st.binary(min_size=1, max_size=2048))
    @hypothesis.settings(max_examples=500)
    def test_compress_parity(self, data: bytes):
        assert rust_compress(data) == py_compress(data)

    @hypothesis.given(data=st.binary(min_size=1, max_size=2048))
    @hypothesis.settings(max_examples=500)
    def test_decompress_parity(self, data: bytes):
        # Compare decompressors over Python-compressed streams (guaranteed
        # valid FastLZ, exercising every opcode family the compressor emits).
        compressed = py_compress(data)
        assert rust_decompress(compressed) == py_decompress(compressed)

    @hypothesis.given(data=st.binary(min_size=1, max_size=2048))
    @hypothesis.settings(max_examples=500)
    def test_cross_decompress_rust_compressed(self, data: bytes):
        # The Python oracle must decompress Rust-compressed output — proves
        # the two implementations agree on the wire format, not just on bytes.
        compressed = rust_compress(data)
        assert py_decompress(compressed).hex() == data.hex()
