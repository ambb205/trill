//! Text as VST3 passes it: fixed-size, NUL-terminated buffers of 8-bit or
//! UTF-16 characters.
//!
//! Nothing a plugin writes into such a buffer is trusted to be terminated
//! or well-formed; conversions stop at the buffer's end and replace what
//! does not decode.

use std::ffi::c_char;

/// The text in an 8-bit buffer, up to the first NUL or the buffer's end.
pub(crate) fn from_chars(buffer: &[c_char]) -> String {
    let bytes: Vec<u8> = buffer.iter().take_while(|unit| **unit != 0).map(|unit| *unit as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

/// The text in a UTF-16 buffer, up to the first NUL or the buffer's end.
pub(crate) fn from_utf16(buffer: &[u16]) -> String {
    let length = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length]).trim().to_string()
}

/// Writes `text` into a UTF-16 buffer, cut short if it does not fit and
/// always NUL-terminated.
pub(crate) fn to_utf16(text: &str, buffer: &mut [u16]) {
    let Some(capacity) = buffer.len().checked_sub(1) else { return };
    let mut length = 0;
    for (slot, unit) in buffer[..capacity].iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
        length += 1;
    }
    buffer[length] = 0;
}

/// Writes `text` into an 8-bit buffer, cut short if it does not fit and
/// always NUL-terminated. Only the plugins this crate's tests are hosted
/// against fill such buffers; a host only reads them.
#[cfg(test)]
pub(crate) fn to_chars(text: &str, buffer: &mut [c_char]) {
    let Some(capacity) = buffer.len().checked_sub(1) else { return };
    let mut length = 0;
    for (slot, byte) in buffer[..capacity].iter_mut().zip(text.bytes()) {
        *slot = byte as c_char;
        length += 1;
    }
    buffer[length] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_ends_at_the_first_nul_or_at_the_end_of_the_buffer() {
        let mut buffer = [0 as c_char; 8];
        to_chars("Synth", &mut buffer);
        assert_eq!(from_chars(&buffer), "Synth");

        // No terminator at all: the buffer's end is the end.
        let full = [b'a' as c_char; 4];
        assert_eq!(from_chars(&full), "aaaa");
        assert_eq!(from_chars(&[]), "");

        let wide: Vec<u16> = "Cutoff\0garbage".encode_utf16().collect();
        assert_eq!(from_utf16(&wide), "Cutoff");
        assert_eq!(from_utf16(&[0x48, 0x69]), "Hi");
    }

    #[test]
    fn text_that_does_not_decode_is_replaced_not_rejected() {
        let latin1 = [b'F' as c_char, 0xFCu8 as c_char, b'r' as c_char, 0];
        assert_eq!(from_chars(&latin1), "F\u{FFFD}r");
        // A lone surrogate.
        assert_eq!(from_utf16(&[0x41, 0xD800, 0x42, 0]), "A\u{FFFD}B");
        assert_eq!(from_utf16(&"  Gain \0".encode_utf16().collect::<Vec<_>>()), "Gain", "padding is trimmed");
    }

    #[test]
    fn writing_truncates_and_always_terminates() {
        let mut buffer = [0xFFFFu16; 4];
        to_utf16("Volume", &mut buffer);
        assert_eq!(buffer, [0x56, 0x6F, 0x6C, 0]);
        to_utf16("dB", &mut buffer);
        assert_eq!(buffer[..3], [0x64, 0x42, 0]);
        to_utf16("anything", &mut []);

        let mut narrow = [0x7F as c_char; 3];
        to_chars("Acme", &mut narrow);
        assert_eq!(from_chars(&narrow), "Ac");
        to_chars("anything", &mut []);
    }

    #[test]
    fn non_ascii_text_round_trips() {
        let mut buffer = [0u16; 16];
        to_utf16("Größe 🎹", &mut buffer);
        assert_eq!(from_utf16(&buffer), "Größe 🎹");
    }
}
