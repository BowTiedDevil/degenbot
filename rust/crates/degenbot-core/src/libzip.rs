//! Solady LibZip: FastLZ compress/decompress for EVM bytecode.
//!
//! Pure-Rust port of the FastLZ LZ77 variant in Solady's `solady.js`
//! (`LibZip.flzCompress` / `LibZip.flzDecompress`). Pure functions over
//! `&[u8]` — no `pyo3`, no I/O, no state held across calls. Direct 1:1
//! transcription of the deployed JS; cross-checked byte-for-byte against the
//! Python oracle (`degenbot.utils.solady.libzip`).
//!
//! ref: <https://github.com/Vectorized/solady/blob/main/js/solady.js>
//!
//! # Instruction format
//!
//! Every opcode's top 3 bits (`opcode >> 5`) select the instruction:
//! - `0` → literal run: `1 + (opcode & 0x1F)` raw bytes follow.
//! - `1..=6` → short match: 2-byte opcode, match length `2 + (opcode >> 5)`,
//!   reference offset `256 * (opcode & 0x1F) + opcode_1`.
//! - `7` → long match: 3-byte opcode, match length `9 + opcode_1`,
//!   reference offset `256 * (opcode & 0x1F) + opcode_2`.
//!
//! The reference offset is a back-reference distance into the output already
//! produced (`output[output.len() - 1 - offset]`).

// `FastLZ`, `Solady`, `LibZip` are product/format names that clippy's
// `doc_markdown` lint wants back-ticked; allowing at module scope keeps the
// prose natural.
#![allow(clippy::doc_markdown)]

/// Maximum literals emitted in a single literal-run opcode.
const MAX_LITERALS: usize = 32;
/// Match length above which a long-match instruction is emitted (the cap on the
/// match length encoded in a single short opcode is `MAX_MATCH_LENGTH - 1`).
const MAX_MATCH_LENGTH: usize = 262;
/// Maximum back-reference offset encodable in the 13-bit offset field.
const MAX_MATCH_OFFSET: usize = 8191;
/// Sentinel: a 3-byte int that can never occur in the input hash domain.
const MAX_3BYTE_INT_PLUS_ONE: u32 = 0x0100_0000;

/// Errors that can occur during LibZip (de)compression.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LibZipError {
    /// Truncated opcode: the compressed stream ended mid-instruction.
    #[error("truncated compressed data at offset {offset}")]
    Truncated {
        /// Byte offset where the truncation was detected.
        offset: usize,
    },
    /// A back-reference offset exceeded the output produced so far.
    #[error(
        "invalid reference offset {offset} at opcode index {index} (output length {output_len})"
    )]
    InvalidReferenceOffset {
        /// The offending back-reference offset.
        offset: usize,
        /// Index of the opcode in the compressed stream.
        index: usize,
        /// Length of the output buffer at the point of failure.
        output_len: usize,
    },
}

// `From<LibZipError> for PyErr` lives here (with the type) rather than in the
// downstream binding crate because the orphan rule forbids `impl ForeignTrait
// for ForeignType` elsewhere. Gated behind the non-default `pyo3` feature so
// pure-Rust consumers build without pulling pyo3.
#[cfg(feature = "pyo3")]
impl From<LibZipError> for pyo3::PyErr {
    fn from(err: LibZipError) -> Self {
        Self::new::<pyo3::exceptions::PyValueError, _>(err.to_string())
    }
}

/// Read a 24-bit little-endian unsigned integer from `bytes` at `pos`.
///
/// Equivalent to the JS `u24(i)`: returns `ib[i] | (ib[i+1] << 8) | (ib[i+2] << 16)`.
///
/// # Panics
///
/// Panics if `pos + 2` is out of bounds — callers guarantee the three bytes
/// exist (compression only calls this on `input_buffer_offset` such that the
/// `+4` tail in `compressible_bytes = len - 4` keeps `pos+3` in range).
fn get_3_byte_int(bytes: &[u8], pos: usize) -> u32 {
    u32::from(bytes[pos]) | (u32::from(bytes[pos + 1]) << 8) | (u32::from(bytes[pos + 2]) << 16)
}

/// FastLZ hash of a 24-bit value, masked into the 13-bit offset domain.
///
/// Equivalent to the Python oracle's `hash(x)`:
/// `((2654435769 * x) >> 19) & 8191`.
///
/// The product `2654435769 * x` reaches ~4.45e16 (beyond `u32`) for large
/// 24-bit `x`, so the multiply is computed in `u64`. Because the final `& 8191`
/// only inspects bits 19..=31 of the product, a `u32`-wrapping multiply happens
/// to yield the same result — `u64` is used for clarity and to match the Python
/// oracle's arbitrary-precision arithmetic exactly.
fn hash(x: u32) -> usize {
    // Knuth-multiplicative hash; masking with `0x1FFF` fits it into the 13-bit
    // back-reference offset domain (the hash table is indexed directly by the
    // same 13 bits to keep it compact).
    (((2_654_435_769_u64 * u64::from(x)) >> 19) & 0x1FFF) as usize
}

/// Compress `data` using Solady's FastLZ algorithm.
///
/// Returns the compressed bytes. The empty input compresses to the empty
/// output. Decompressing the result with [`decompress`] yields `data`.
///
/// # Errors
///
/// Returns [`LibZipError`] never in the current implementation (compression
/// cannot fail for any input); the `Result` is kept for symmetry with
/// [`decompress`] and future-proofing.
///
/// # Examples
///
/// ```
/// use degenbot_core::libzip::{compress, decompress};
/// let data = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// let compressed = compress(data).unwrap();
/// let round = decompress(&compressed).unwrap();
/// assert_eq!(round, data);
/// ```
#[expect(clippy::cast_possible_truncation)]
pub fn compress(data: &[u8]) -> Result<Vec<u8>, LibZipError> {
    // `compressible_bytes = ib.length - 4`: the final 4 bytes are incompressible
    // tail reserved so the inner hash/match window never reads out of bounds
    // (the +3 match window + the +4 tail guarantee `p+1..p+1+2` are in range).
    let compressible_bytes = data.len().saturating_sub(4);

    let mut hash_table = vec![0_usize; MAX_MATCH_OFFSET + 1];
    let mut output_buffer: Vec<u8> = Vec::new();

    let mut last_literal_offset = 0_usize;
    let mut input_buffer_offset = 2_usize;

    // Compress nothing past the head; the `b - 9` guard mirrors the JS
    // `while (i < b - 9)` loop bound (the 9-byte headroom avoids the match
    // encoder writing a reference whose `+3` window plus the `+4` tail would
    // read out of bounds).
    while input_buffer_offset < compressible_bytes.saturating_sub(9) {
        // Inner搜寻: walk forward until the current 3-byte chunk matches a
        // previously-seen chunk (or until we exhaust the compressible region).
        let mut reference_chunk;
        let mut last_seen_offset;
        loop {
            let input_chunk = get_3_byte_int(data, input_buffer_offset);
            let input_chunk_hash = hash(input_chunk);
            last_seen_offset = hash_table[input_chunk_hash];
            hash_table[input_chunk_hash] = input_buffer_offset;

            let distance_to_last_seen_chunk = input_buffer_offset - last_seen_offset;
            reference_chunk = if distance_to_last_seen_chunk <= MAX_MATCH_OFFSET {
                get_3_byte_int(data, last_seen_offset)
            } else {
                MAX_3BYTE_INT_PLUS_ONE
            };

            if input_buffer_offset < compressible_bytes - 9 && input_chunk != reference_chunk {
                input_buffer_offset += 1;
                continue;
            }
            input_buffer_offset += 1;
            break;
        }

        if input_buffer_offset >= compressible_bytes - 9 {
            break;
        }

        input_buffer_offset -= 1;
        if input_buffer_offset > last_literal_offset {
            add_literals(
                data,
                &mut output_buffer,
                input_buffer_offset - last_literal_offset,
                last_literal_offset,
            );
        }

        let p = last_seen_offset + 3; // Start of previous occurrence
        let q = input_buffer_offset + 3; // Start of current occurrence
        let max_possible_match_length = compressible_bytes - q;

        // Find the maximum match length. The JS `e *= ib[p+l]===ib[q+l]` idiom
        // stops at the first mismatch or at `max_possible_match_length`; the
        // `+1` here mirrors the Python oracle's `match_length = offset + 1`.
        let mut match_length = max_possible_match_length;
        for offset in 0..max_possible_match_length {
            if data[p + offset] != data[q + offset] {
                match_length = offset + 1;
                break;
            }
        }

        input_buffer_offset += match_length;
        // The Perl/Python oracle tracks `distance_to_last_seen_chunk = i - r`
        // *before* the `i += match_length` step (`r` == `last_seen_offset`),
        // then applies `--d` (the JS `--d`) because match offsets are encoded
        // as `distance - 1`. Reconstruct from the post-increment variables.
        let distance_to_last_seen_chunk = input_buffer_offset - match_length - last_seen_offset - 1;

        // Emit LONG MATCH opcodes for each full `MAX_MATCH_LENGTH` chunk.
        let mut remaining = match_length;
        while remaining >= MAX_MATCH_LENGTH {
            // LONG MATCH instruction — 3 byte opcode
            let opcode_0 = 0b1110_0000_u8 + ((distance_to_last_seen_chunk >> 8) as u8);
            let opcode_1 = (MAX_MATCH_LENGTH - 9) as u8; // 253
            let opcode_2 = (distance_to_last_seen_chunk & 0b000_1111_1111) as u8;
            output_buffer.push(opcode_0);
            output_buffer.push(opcode_1);
            output_buffer.push(opcode_2);
            remaining -= MAX_MATCH_LENGTH;
        }

        // Emit the residual chunk as a SHORT or LONG match opcode.
        if remaining < 7 {
            // SHORT MATCH instruction — 2 byte opcode
            let opcode_0 = ((remaining << 5) as u8) + ((distance_to_last_seen_chunk >> 8) as u8);
            let opcode_1 = (distance_to_last_seen_chunk & 0b1111_1111) as u8;
            output_buffer.push(opcode_0);
            output_buffer.push(opcode_1);
        } else {
            // LONG MATCH instruction — 3 byte opcode
            let opcode_0 = 0b1110_0000_u8 + ((distance_to_last_seen_chunk >> 8) as u8);
            let opcode_1 = (remaining - 7) as u8;
            let opcode_2 = (distance_to_last_seen_chunk & 0b000_1111_1111) as u8;
            output_buffer.push(opcode_0);
            output_buffer.push(opcode_1);
            output_buffer.push(opcode_2);
        }

        // Update the hash table with the next two positions.
        hash_table[hash(get_3_byte_int(data, input_buffer_offset))] = input_buffer_offset;
        input_buffer_offset += 1;
        hash_table[hash(get_3_byte_int(data, input_buffer_offset))] = input_buffer_offset;
        input_buffer_offset += 1;

        last_literal_offset = input_buffer_offset;
    }

    // Add the remaining literals (the incompressible tail + any unmatched head).
    // `compressible_bytes + 4` == `data.len()` (signed arithmetic in the Python
    // oracle); `last_literal_offset <= data.len()` always holds, so this never
    // underflows. This also makes small inputs (`len < 13`, where the compress
    // loop never fires) emit themselves verbatim as one literal run.
    add_literals(
        data,
        &mut output_buffer,
        data.len() - last_literal_offset,
        last_literal_offset,
    );

    Ok(output_buffer)
}

/// Append a run of literal bytes from `input` to `output`.
///
/// Mirrors the JS `literals(r, s)`: emit up to `MAX_LITERALS` literals per
/// opcode, looping until the run is exhausted.
#[expect(clippy::cast_possible_truncation)]
fn add_literals(input: &[u8], output: &mut Vec<u8>, mut run_length: usize, mut start: usize) {
    while run_length > 0 {
        let number_of_literals = run_length.min(MAX_LITERALS);
        // Encode the run length (0-indexed: `number_of_literals - 1`).
        output.push((number_of_literals - 1) as u8);
        // Encode the literals
        output.extend_from_slice(&input[start..start + number_of_literals]);
        run_length -= number_of_literals;
        start += number_of_literals;
    }
}

/// Decompress `data` using Solady's FastLZ algorithm.
///
/// Returns the decompressed bytes. The empty input decompresses to the empty
/// output.
///
/// # Errors
///
/// Returns [`LibZipError::Truncated`] if the stream ends mid-instruction, or
/// [`LibZipError::InvalidReferenceOffset`] if a back-reference points before
/// the start of the output produced so far.
///
/// # Examples
///
/// ```
/// use degenbot_core::libzip::decompress;
/// // 0x02 = literal run of 3 bytes
/// let compressed = [0x02, b'A', b'B', b'C'];
/// assert_eq!(decompress(&compressed).unwrap(), b"ABC");
/// ```
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, LibZipError> {
    let mut output: Vec<u8> = Vec::new();
    let mut index = 0_usize;

    while index < data.len() {
        let opcode_0 = data[index];
        let instruction_type = opcode_0 >> 5;

        match instruction_type {
            0 => {
                // Literal run
                let number_of_literals = 1 + (opcode_0 & 0b0001_1111) as usize;
                let literals_start = index + 1;
                let literals_end = literals_start + number_of_literals;
                if literals_end > data.len() {
                    return Err(LibZipError::Truncated { offset: index });
                }
                output.extend_from_slice(&data[literals_start..literals_end]);
                index = literals_end;
            }
            1..=6 => {
                // Short match — 2 byte opcode
                let opcode_1_index = index + 1;
                if opcode_1_index >= data.len() {
                    return Err(LibZipError::Truncated { offset: index });
                }
                let opcode_1 = data[opcode_1_index];
                let match_length = 2 + instruction_type as usize;
                let reference_offset = 256 * (opcode_0 & 0b0001_1111) as usize + opcode_1 as usize;
                copy_match(&mut output, reference_offset, match_length, index)?;
                index = opcode_1_index + 1;
            }
            7 => {
                // Long match — 3 byte opcode
                let opcode_1_index = index + 1;
                let opcode_2_index = index + 2;
                if opcode_2_index >= data.len() {
                    return Err(LibZipError::Truncated { offset: index });
                }
                let opcode_1 = data[opcode_1_index];
                let opcode_2 = data[opcode_2_index];
                let match_length = 9 + opcode_1 as usize;
                let reference_offset = 256 * (opcode_0 & 0b0001_1111) as usize + opcode_2 as usize;
                copy_match(&mut output, reference_offset, match_length, index)?;
                index = opcode_2_index + 1;
            }
            // Unreachable: `opcode_0 >> 5` is a `u8` in `0..=7`.
            _ => unreachable!("instruction_type is opcode >> 5, which is in 0..=7"),
        }
    }

    Ok(output)
}

/// Copy `match_length` bytes from `output[len - 1 - reference_offset]` forward,
/// yielding the LZ77 overlapping back-reference semantics.
///
/// Mirrors the JS `r = o - f - 1; while (l--) ob[o++] = ob[r++]` — the read
/// pointer advances forward, so a short offset into a long match repeats the
/// initial referenced bytes (the RLE-like overlap behavior).
///
/// # Errors
///
/// Returns [`LibZipError::InvalidReferenceOffset`] if `reference_offset >=
/// output.len()` (the back-reference would read before the output start).
#[expect(clippy::explicit_counter_loop)]
fn copy_match(
    output: &mut Vec<u8>,
    reference_offset: usize,
    match_length: usize,
    opcode_index: usize,
) -> Result<(), LibZipError> {
    if reference_offset >= output.len() {
        return Err(LibZipError::InvalidReferenceOffset {
            offset: reference_offset,
            index: opcode_index,
            output_len: output.len(),
        });
    }
    let mut read_pos = output.len() - 1 - reference_offset;
    for _ in 0..match_length {
        // Read first, push second: the read may land in bytes that this very
        // loop is appending (overlapping back-reference → byte-wise RLE).
        let byte = output[read_pos];
        output.push(byte);
        read_pos += 1;
    }
    Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        let hex = hex.strip_prefix("0x").unwrap_or(hex);
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decompress_literal_run() {
        let decompressed = [0x41, 0x42, 0x43];
        let compressed = [0x02, 0x41, 0x42, 0x43];
        assert_eq!(decompress(&compressed).unwrap(), decompressed);
    }

    #[test]
    fn decompress_literal_run_short_match_1() {
        let decompressed = [0x41, 0x42, 0x43, 0x44, 0x42, 0x43, 0x44];
        let compressed = [0x03, 0x41, 0x42, 0x43, 0x44, 0x20, 0x02];
        assert_eq!(decompress(&compressed).unwrap(), decompressed);
    }

    #[test]
    fn decompress_literal_run_short_match_2() {
        let decompressed = [0x61, 0x61, 0x61, 0x61, 0x61];
        let compressed = [0x00, 0x61, 0x40, 0x00];
        assert_eq!(decompress(&compressed).unwrap(), decompressed);
    }

    #[test]
    fn decompress_literal_run_long_match() {
        let decompressed = [
            0x44, 0x45, 0x44, 0x45, 0x44, 0x45, 0x44, 0x45, 0x44, 0x45, 0x44, 0x45,
        ];
        let compressed = [0x01, 0x44, 0x45, 0xE0, 0x01, 0x01];
        assert_eq!(decompress(&compressed).unwrap(), decompressed);
    }

    #[test]
    fn compress_empty() {
        assert!(compress(b"").unwrap().is_empty());
    }

    #[test]
    fn decompress_empty() {
        assert!(decompress(b"").unwrap().is_empty());
    }

    #[test]
    fn compress_network_spirituality_text() {
        // The full "network spirituality" prose — the canonical Solady fixture.
        let input = b"New wave digital art should not be judged solely on aesthetic merit \
but just as importantly for its ability to develop a total vision of the Wired and above all \
create network spirituality. Ancient Greek art is held in the highest regard because it \
developed a whole mythology that shaped religion, morality and way of life. Thought is \
implicit in the art works of Ancient Greece but not sufficiently disengaged from the \
sensory to reflect its own products. The problem of modernity is the development of \
rational self reflection. The beauty of art appears in a form which contrasts abstract \
thought. Thus, abstract thought destroys the naive/sensuous appreciation of art in \
exerting its nature. Reality is slain by comprehension. Proper interaction with the \
wired involves the trance separation of real abstract thought and accelerates \
externalisation into pure intuition, embodying the network and unselfconsciously \
drawing out truths from the collective noosphere. Through this being on the wired \
we achieve a return to naive, unselfconscious interaction. The individual ego is \
sacrificed into the collective noosphere, uniting us under a totalising spirit. \
The best art in the wired is not only beautiful but produces a network spirituality. \
I long for Network Spirituality!";
        // Expected output from the Python oracle (`flz_compress`), byte-for-byte.
        let expected = hex_to_bytes(
            "1f4e65772077617665206469676974616c206172742073686f756c64206e6f74201f6265206a7564\
67656420736f6c656c79206f6e20616573746865746963206d65017269202301757420240e7374206173\
20696d706f7274616e74202a1b666f7220697473206162696c69747920746f20646576656c6f7020612\
00c406d03766973692051026f662020510320576972206801616e200301626f209315616c6c20637265\
617465206e6574776f726b20737069207201756140500e2e20416e6369656e7420477265656b60ba046\
97320686520bb01696e6051036869676820ad062072656761726420cd0463617573652095c089406f02\
20776820db03206d79742007016f6720a801686141040061401d0272656c2119066f6e2c206d6f72607\
660a0017761210a0866206c6966652e2054212f016768607c20ff026c6963205fa080414e40b8007340\
2de003ae016365613741630373756666203320c8213406646973656e676141710366726f6d208b016520\
201102736f722097006f208e03666c6563205e21570b6f776e2070726f647563747340840065400d0362\
6c656d406b056d6f6465726e417621120074201ba17a006d2066401f0272617420d521f00273656c200d\
805620118048036265617520e920e140c00561707065617220dc21fb41d5006d21310a69636820636f6e\
74726173208a02616273200920972142410a40450275732ce0081720852028016f798092056e61697665\
2f40dd02756f752044057070726563696098c07a2072076578657274696e6760f3046e61747572217201\
5265818b20dc02736c612025016279208e006d20400068212c60c701507221de2281026e7465409a2013\
04207769746820a200652008426906696e766f6c7665808c20bf006e21940373657061811b4082227c231\
2e006dc620004616363656c20580074203e0165782062214b01697380bb2072026f207020a7400901756\
9407a072c20656d626f647940c74080c2cf42ec01756e4183214c0273636921074204027261774029006\
f235905747275746873e2010c02636f6c42012139223c046f73706865410f025468724178205b2111016\
265603c830280ea0077228a02636869237e21c20172652144201d006f214e2047002ce0078140b620da20\
64c20f06696e64697669642377032065676f6175036163726922b6234060ee40d4e00b9a405c210121be2\
05701756e2294208063fc23fa201583d040c0427523aa427021cf404f80ba20d04325016f6e2118629703\
6966756c633c82fc21800061e1004f804e20a722f9052e2049206c6f20634488024e657443870d2053706\
972697475616c69747921",
        );
        let compressed = compress(input).unwrap();
        assert_eq!(compressed, expected);
        // Round-trip
        assert_eq!(decompress(&compressed).unwrap(), input);
    }

    #[test]
    fn roundtrip_random_bytes() {
        use proptest::prelude::*;
        proptest!(|(data in prop::collection::vec(any::<u8>(), 0..512))| {
            let compressed = compress(&data).unwrap();
            let decompressed = decompress(&compressed).unwrap();
            prop_assert_eq!(decompressed, data);
        });
    }
}
