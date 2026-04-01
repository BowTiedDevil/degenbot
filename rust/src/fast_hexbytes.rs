//! `FastHexBytes` - High-performance hex/bytes type for Python.
//!
//! A frozen Rust-native type that stores both raw bytes and pre-computed hex
//! string, eliminating conversion overhead. Implements Python buffer protocol
//! for seamless bytes compatibility.

use pyo3::exceptions::{PyIndexError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyMemoryView, PySlice, PyType};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Static format string for buffer protocol (avoids allocation per getbuffer call).
#[allow(clippy::unwrap_used)]
static FORMAT_B: std::sync::LazyLock<std::ffi::CString> =
    std::sync::LazyLock::new(|| std::ffi::CString::new("B").unwrap());

/// High-performance hex/bytes type with pre-computed hex representation.
///
/// Stores both bytes and pre-computed "0x"-prefixed hex string for zero-cost
/// `hex()` calls. Implements Python buffer protocol for bytes compatibility.
///
/// # Performance Tips
///
/// - Use `memoryview(obj)` for zero-copy buffer access (no Python object creation)
/// - Use `obj.raw` property for direct `bytes` access without allocation
/// - Avoid `bytes(obj)` in hot paths - it creates a new Python object each call
/// - Slicing returns `FastHexBytes` with pre-computed hex for zero-cost `.hex()`
#[pyclass(frozen, name = "FastHexBytes", module = "degenbot_rs", from_py_object)]
#[derive(Clone)]
pub struct FastHexBytes {
    bytes: Vec<u8>,
    hex: String, // Pre-computed "0x" + lowercase hex
}

#[pymethods]
impl FastHexBytes {
    /// Create `FastHexBytes` from various input types.
    ///
    /// Accepts: hex string (with/without 0x), bytes, bytearray, memoryview,
    ///          int, bool, or another `FastHexBytes`.
    #[new]
    fn new(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        // Check for FastHexBytes first (copy constructor)
        if let Ok(existing) = value.extract::<Self>() {
            return Ok(existing);
        }

        // Handle string (hex string)
        if let Ok(s) = value.extract::<String>() {
            return Self::from_hex(&s);
        }

        // Handle bytes
        if let Ok(b) = value.extract::<Vec<u8>>() {
            return Ok(Self::from_bytes(&b));
        }

        // Handle bytearray
        if let Ok(ba) = value.cast::<PyByteArray>() {
            let bytes: Vec<u8> = ba.to_vec();
            return Ok(Self::from_bytes(&bytes));
        }

        // Handle memoryview
        if let Ok(mv) = value.cast::<PyMemoryView>() {
            // Convert memoryview to bytes then extract
            let bytes: PyResult<Vec<u8>> = mv.call_method0("tobytes")?.extract();
            if let Ok(buf) = bytes {
                return Ok(Self::from_bytes(&buf));
            }
        }

        // Handle int (convert to hex then bytes)
        // Use BigUint to support EVM-sized integers (up to 256 bits)
        if let Ok(n) = value.extract::<num_bigint::BigUint>() {
            if n == num_bigint::BigUint::ZERO {
                // Zero is a special case - format!("0x{}", "0") works
                return Self::from_hex("0x0");
            }
            return Self::from_hex(&format!("0x{n:x}"));
        }

        // Handle bool
        if let Ok(b) = value.extract::<bool>() {
            let bytes = if b { vec![0x01] } else { vec![0x00] };
            return Ok(Self::from_bytes(&bytes));
        }

        Err(PyTypeError::new_err(format!(
            "Cannot convert {} to FastHexBytes",
            value.get_type().name()?
        )))
    }

    /// Return pre-computed hex string with 0x prefix (zero cost).
    fn hex(&self) -> &str {
        &self.hex
    }

    /// Return hex string with 0x prefix (same as `hex()`).
    fn to_0x_hex(&self) -> &str {
        &self.hex
    }

    /// Return length in bytes.
    #[allow(clippy::missing_const_for_fn)]
    fn __len__(&self) -> usize {
        self.bytes.len()
    }

    /// Return byte at index (int) or slice (bytes).
    fn __getitem__<'py>(
        &self,
        py: Python<'py>,
        key: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Handle integer index
        if let Ok(index) = key.extract::<isize>() {
            let len = self.bytes.len().try_into().unwrap_or(isize::MAX);
            let idx = if index < 0 { len + index } else { index };
            if idx < 0 || idx >= len {
                return Err(PyIndexError::new_err("index out of range"));
            }
            let idx_usize: usize = idx.try_into().unwrap_or(0);
            return Ok(self.bytes[idx_usize].into_pyobject(py)?.into_any());
        }

        // Handle slice - return FastHexBytes for consistency
        if let Ok(slice) = key.cast::<PySlice>() {
            let len = self.bytes.len();
            let indices = slice.indices(len.try_into().unwrap_or(isize::MAX))?;
            let step = indices.step;
            let start = indices.start;
            let stop_idx = indices.stop;

            // Python slice iteration logic:
            // - For positive step: include indices where i < stop_idx
            // - For negative step: include indices where i > stop_idx
            // - The stop_idx value is normalized by indices() to -1 for negative step
            //   when slicing to the beginning

            let sliced: Vec<u8> = if step == 1 {
                let start_usize = start.max(0).cast_unsigned();
                let stop_usize = stop_idx.clamp(0, len.cast_signed()).cast_unsigned();
                self.bytes[start_usize..stop_usize].to_vec()
            } else {
                // For negative step, we need to iterate like Python does
                let mut result = Vec::new();
                let mut i = start;

                if step < 0 {
                    // Negative step: continue while i > stop_idx
                    while i > stop_idx {
                        // Convert to usize index, bounds check
                        if i >= 0 && i.cast_unsigned() < len {
                            result.push(self.bytes[i.cast_unsigned()]);
                        }
                        i += step;
                    }
                } else {
                    // Positive step > 1
                    let stop_clamped = stop_idx.min(len.cast_signed());
                    while i < stop_clamped {
                        if i >= 0 && i.cast_unsigned() < len {
                            result.push(self.bytes[i.cast_unsigned()]);
                        }
                        i += step;
                    }
                }
                result
            };

            // Return FastHexBytes for consistent type and pre-computed hex
            let result = Self::from_bytes(&sliced);
            return Ok(result.into_pyobject(py)?.into_any());
        }

        Err(PyTypeError::new_err("indices must be integers or slices"))
    }

    /// Return iterator over byte values (as ints).
    ///
    /// Creates a `PyBytes` wrapper and returns its iterator. While this
    /// creates one Python object, the actual iteration happens at C speed
    /// without per-element FFI overhead.
    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let bytes = PyBytes::new(py, &self.bytes);
        bytes.call_method0("__iter__")
    }

    /// Return reverse iterator over byte values (as ints).
    ///
    /// Uses Python's built-in `reversed()` function for optimal performance.
    fn __reversed__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let bytes = PyBytes::new(py, &self.bytes);
        let builtins = py.import("builtins")?;
        builtins.call_method1("reversed", (bytes,))
    }

    /// Return bytes representation.
    fn __bytes__<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.bytes)
    }

    /// Return repr: `FastHexBytes('0x...')`
    fn __repr__(&self) -> String {
        format!("FastHexBytes('{}')", self.hex)
    }

    /// Return str: 0x...
    fn __str__(&self) -> &str {
        &self.hex
    }

    /// Compare with bytes, str, or `FastHexBytes` by content.
    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        // Compare with FastHexBytes (fastest - direct field access)
        if let Ok(other_fhb) = other.extract::<Self>() {
            return self.bytes == other_fhb.bytes;
        }

        // Compare with bytes using buffer protocol (zero-copy)
        if let Ok(other_slice) = other.extract::<&[u8]>() {
            return self.bytes == other_slice;
        }

        // Compare with bytearray (requires Vec<u8> extraction)
        if let Ok(other_bytes) = other.extract::<Vec<u8>>() {
            return self.bytes == other_bytes;
        }

        // Compare with hex string - check prefix once instead of two comparisons
        other.extract::<&str>().is_ok_and(|other_str| {
            let other_bytes = other_str.as_bytes();
            // Check if string starts with "0x" or "0X"
            if other_bytes.len() >= 2
                && other_bytes[0] == b'0'
                && (other_bytes[1] == b'x' || other_bytes[1] == b'X')
            {
                // Has prefix - compare full strings case-insensitively
                self.hex.eq_ignore_ascii_case(other_str)
            } else {
                // No prefix - compare without our prefix, case-insensitively
                self.hex[2..].eq_ignore_ascii_case(other_str)
            }
        })
    }

    /// Hash based on bytes content.
    fn __hash__(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.bytes.hash(&mut hasher);
        hasher.finish()
    }

    /// Check if a byte value is contained in the bytes.
    ///
    /// Supports `in` operator for checking byte values (0-255).
    fn __contains__(&self, item: &Bound<'_, PyAny>) -> bool {
        item.extract::<u8>().is_ok_and(|byte| self.bytes.contains(&byte))
    }

    /// Support pickle serialization.
    #[allow(clippy::unnecessary_wraps)]
    fn __reduce__<'py>(&self, py: Python<'py>) -> (Bound<'py, PyType>, (Bound<'py, PyBytes>,)) {
        let type_obj = py.get_type::<Self>();
        let bytes = PyBytes::new(py, &self.bytes);
        (type_obj, (bytes,))
    }

    /// Check if `FastHexBytes` is truthy (non-empty).
    #[allow(clippy::missing_const_for_fn)]
    fn __bool__(&self) -> bool {
        !self.bytes.is_empty()
    }

    /// Concatenate with bytes or `FastHexBytes`.
    fn __add__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        let other_bytes: Vec<u8> = if let Ok(other_fhb) = other.extract::<Self>() {
            other_fhb.bytes
        } else if let Ok(other_bytes) = other.extract::<Vec<u8>>() {
            other_bytes
        } else if let Ok(other_str) = other.extract::<String>() {
            hex::decode(other_str.trim_start_matches("0x"))
                .map_err(|e| PyValueError::new_err(format!("Invalid hex string: {e}")))?
        } else {
            return Err(PyTypeError::new_err(format!(
                "Can only concatenate FastHexBytes with bytes, str, or FastHexBytes, not {}",
                other.get_type().name()?
            )));
        };

        let mut combined = self.bytes.clone();
        combined.extend(other_bytes);
        Ok(Self::from_bytes(&combined))
    }

    /// Repeat `FastHexBytes` n times.
    fn __mul__(&self, n: usize) -> Self {
        let mut result = Vec::with_capacity(self.bytes.len() * n);
        for _ in 0..n {
            result.extend_from_slice(&self.bytes);
        }
        Self::from_bytes(&result)
    }

    /// Reverse multiplication (n * `FastHexBytes`).
    fn __rmul__(&self, n: usize) -> Self {
        self.__mul__(n)
    }

    /// Getter for hex property.
    #[getter]
    fn hex_property(&self) -> &str {
        &self.hex
    }

    /// Getter for raw property (bytes as bytes).
    #[getter]
    fn raw(&self) -> &[u8] {
        &self.bytes
    }

    /// Buffer protocol: get buffer view.
    unsafe fn __getbuffer__(
        slf: Bound<'_, Self>,
        view: *mut pyo3::ffi::Py_buffer,
        flags: std::os::raw::c_int,
    ) -> PyResult<()> {
        use pyo3::ffi::{PyBUF_ND, PyBUF_STRIDES, PyBUF_WRITABLE};

        if view.is_null() {
            return Err(PyValueError::new_err("view is null"));
        }

        if (flags & PyBUF_WRITABLE) == PyBUF_WRITABLE {
            return Err(PyValueError::new_err("FastHexBytes is read-only"));
        }

        let borrowed = slf.borrow();
        let data = &borrowed.bytes;
        let len = data.len();

        unsafe {
            // Transfer ownership to the buffer - PyO3 handles the reference count
            (*view).obj = slf.into_any().into_ptr();

            (*view).buf = data.as_ptr().cast_mut().cast();
            (*view).len = len.cast_signed();
            (*view).readonly = 1;
            (*view).itemsize = 1;

            // Format string - only set if requested (static, no allocation)
            (*view).format = if (flags & pyo3::ffi::PyBUF_FORMAT) == pyo3::ffi::PyBUF_FORMAT {
                FORMAT_B.as_ptr().cast_mut()
            } else {
                std::ptr::null_mut()
            };

            (*view).ndim = 1;
            (*view).shape = if (flags & PyBUF_ND) == PyBUF_ND {
                &raw mut (*view).len
            } else {
                std::ptr::null_mut()
            };

            (*view).strides = if (flags & PyBUF_STRIDES) == PyBUF_STRIDES {
                &raw mut (*view).itemsize
            } else {
                std::ptr::null_mut()
            };

            (*view).suboffsets = std::ptr::null_mut();
            (*view).internal = std::ptr::null_mut();
        }

        Ok(())
    }

    /// Buffer protocol: release buffer view.
    ///
    /// No cleanup needed - format string is static.
    unsafe fn __releasebuffer__(_slf: Bound<'_, Self>, _view: *mut pyo3::ffi::Py_buffer) {
        // Format string is static (FORMAT_B), no need to free
    }
}

impl FastHexBytes {
    /// Create from byte slice.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let hex = format!("0x{}", hex::encode(bytes));
        Self {
            bytes: bytes.to_vec(),
            hex,
        }
    }

    /// Create from hex string (with or without 0x prefix).
    pub fn from_hex(hex_str: &str) -> PyResult<Self> {
        let stripped = hex_str.strip_prefix("0x").unwrap_or(hex_str);
        let stripped = stripped.strip_prefix("0X").unwrap_or(stripped);

        // Pad with leading zero if odd length (e.g., "0x0" -> "0x00")
        let padded = if stripped.len() % 2 == 1 {
            format!("0{stripped}")
        } else {
            stripped.to_string()
        };

        let bytes = hex::decode(&padded)
            .map_err(|e| PyValueError::new_err(format!("Invalid hex string: {e}")))?;

        // Pre-compute hex with lowercase 0x prefix
        let hex = format!("0x{}", padded.to_lowercase());

        Ok(Self { bytes, hex })
    }

    /// Get the bytes content.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Get the pre-computed hex string.
    #[must_use]
    pub fn as_hex(&self) -> &str {
        &self.hex
    }
}

/// Create a `FastHexBytes` object from bytes.
///
/// This is a convenience function for use by other Rust code.
pub fn create_fast_hexbytes<'py>(
    py: Python<'py>,
    data: &[u8],
) -> PyResult<Bound<'py, FastHexBytes>> {
    let fhb = FastHexBytes::from_bytes(data);
    fhb.into_pyobject(py)
}

/// Create a `FastHexBytes` object from a hex string.
///
/// This is a convenience function for use by other Rust code.
pub fn create_fast_hexbytes_from_hex<'py>(
    py: Python<'py>,
    hex_str: &str,
) -> PyResult<Bound<'py, FastHexBytes>> {
    let fhb = FastHexBytes::from_hex(hex_str)?;
    fhb.into_pyobject(py)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_from_bytes() {
        let data = [0x12, 0x34, 0xab, 0xcd];
        let fhb = FastHexBytes::from_bytes(&data);
        assert_eq!(fhb.bytes, vec![0x12, 0x34, 0xab, 0xcd]);
        assert_eq!(fhb.hex, "0x1234abcd");
    }

    #[test]
    fn test_from_hex_with_prefix() {
        let fhb = FastHexBytes::from_hex("0x1234AbCd").expect("valid hex");
        assert_eq!(fhb.bytes, vec![0x12, 0x34, 0xab, 0xcd]);
        assert_eq!(fhb.hex, "0x1234abcd"); // lowercase
    }

    #[test]
    fn test_from_hex_without_prefix() {
        let fhb = FastHexBytes::from_hex("DEADBEEF").expect("valid hex");
        assert_eq!(fhb.bytes, vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(fhb.hex, "0xdeadbeef");
    }

    #[test]
    fn test_from_hex_invalid() {
        let result = FastHexBytes::from_hex("nothex");
        assert!(result.is_err());
    }

    #[test]
    fn test_len() {
        let fhb = FastHexBytes::from_bytes(&[0x00; 32]);
        assert_eq!(fhb.__len__(), 32);
    }

    #[test]
    fn test_repr() {
        let fhb = FastHexBytes::from_hex("0x1234").unwrap();
        assert_eq!(fhb.__repr__(), "FastHexBytes('0x1234')");
    }

    #[test]
    fn test_str() {
        let fhb = FastHexBytes::from_hex("0xABCD").unwrap();
        assert_eq!(fhb.__str__(), "0xabcd");
    }

    #[test]
    fn test_bool() {
        let empty = FastHexBytes::from_bytes(b"");
        let non_empty = FastHexBytes::from_bytes(b"\x01");
        assert!(!empty.__bool__());
        assert!(non_empty.__bool__());
    }

    #[test]
    fn test_add() {
        let fhb1 = FastHexBytes::from_bytes(&[0x12, 0x34]);
        let fhb2 = FastHexBytes::from_bytes(&[0x56, 0x78]);

        let combined: Vec<u8> = fhb1.bytes.iter().chain(&fhb2.bytes).copied().collect();
        let result = FastHexBytes::from_bytes(&combined);

        assert_eq!(result.bytes, vec![0x12, 0x34, 0x56, 0x78]);
        assert_eq!(result.hex, "0x12345678");
    }

    #[test]
    fn test_mul() {
        let fhb = FastHexBytes::from_bytes(&[0x12, 0x34]);
        let result = fhb.__mul__(3);
        assert_eq!(result.bytes, vec![0x12, 0x34, 0x12, 0x34, 0x12, 0x34]);
        assert_eq!(result.hex, "0x123412341234");
    }
}
