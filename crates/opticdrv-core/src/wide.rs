//! Compile-time string conversion for kernel `UNICODE_STRING` literals.

/// Converts an ASCII string to UTF-16 at compile time.
///
/// Contract: `s.len() == N` and `s` is pure ASCII.
///
/// # Panics
/// On a violation; in a `const`/`static` initialiser that is a compile error.
#[must_use]
pub const fn ascii_to_utf16<const N: usize>(s: &str) -> [u16; N] {
    let bytes = s.as_bytes();
    assert!(bytes.len() == N, "N must equal the string length");
    let mut out = [0u16; N];
    let mut i = 0;
    while i < N {
        assert!(bytes[i].is_ascii(), "only ASCII is supported");
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // If this fails: device and symlink names are corrupted and the device never appears.
    #[test]
    fn converts_ascii_to_utf16_units() {
        const NAME: [u16; 4] = ascii_to_utf16::<4>("\\Dev");
        assert_eq!(NAME, [0x5c, 0x44, 0x65, 0x76]);
    }

    // If this fails: a miscounted literal silently truncates or zero-pads the name.
    #[test]
    #[should_panic(expected = "string length")]
    fn rejects_wrong_length() {
        let _ = ascii_to_utf16::<3>("abcd");
    }

    // If this fails: non-ASCII text is mangled instead of rejected.
    #[test]
    #[should_panic(expected = "ASCII")]
    fn rejects_non_ascii() {
        let _ = ascii_to_utf16::<2>("é");
    }
}
