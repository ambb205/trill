//! Bytes as text a person can read out, type and paste: the letters an
//! invitation's secret and a keeper's key are written in, and the
//! sixteen digits a meeting and a ticket are.

/// The letters: no I, L, O or U, which are read as something else.
const LETTERS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Why a text is not the bytes it was to be.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Misspelt {
    /// A letter that none is written in.
    Letter(char),
    /// Not as many letters as the whole has.
    Length,
}

/// `bytes` in letters, five bits a letter, in groups of four with a dash
/// between.
pub fn spell(bytes: &[u8]) -> String {
    let bits: Vec<u8> = bytes.iter().flat_map(|byte| (0..8).rev().map(move |bit| byte >> bit & 1)).collect();
    let letters: Vec<char> = bits
        .chunks(5)
        .map(|five| {
            // (The last letter of a whole that is no multiple of five
            // bits is filled with zeros.)
            let value = five.iter().fold(0usize, |value, bit| value << 1 | *bit as usize) << (5 - five.len());
            LETTERS[value] as char
        })
        .collect();
    let groups: Vec<String> = letters.chunks(4).map(|group| group.iter().collect()).collect();
    groups.join("-")
}

/// Reads what [`spell`] wrote of `count` bytes, as someone types it:
/// spaces and dashes do not matter, nor whether letters are large or
/// small, and an O is a zero, an I or an L a one.
pub fn unspell(text: &str, count: usize) -> Result<Vec<u8>, Misspelt> {
    let mut bits: Vec<u8> = Vec::with_capacity(count * 8 + 4);
    for letter in text.chars().filter(|letter| !letter.is_whitespace() && *letter != '-') {
        // (Nobody is given room by typing on: the whole has an end.)
        if bits.len() > count * 8 + 4 {
            return Err(Misspelt::Length);
        }
        let letter = match letter.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            letter => letter,
        };
        let value = LETTERS.iter().position(|known| *known as char == letter).ok_or(Misspelt::Letter(letter))?;
        bits.extend((0..5).rev().map(|bit| (value >> bit) as u8 & 1));
    }
    // As many letters as hold the bytes, and no more.
    if bits.len() != (count * 8).div_ceil(5) * 5 || bits[count * 8..].iter().any(|bit| *bit != 0) {
        return Err(Misspelt::Length);
    }
    Ok(bits[..count * 8].chunks(8).map(|eight| eight.iter().fold(0, |byte, bit| byte << 1 | bit)).collect())
}

/// `bytes` as sixteen-digit numbers, two a byte.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Reads what [`hex`] wrote of exactly `N` bytes.
pub fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let text = text.as_bytes();
    if text.len() != N * 2 {
        return None;
    }
    let digit = |digit: u8| (digit as char).to_digit(16).map(|value| value as u8);
    let mut bytes = [0u8; N];
    for (byte, pair) in bytes.iter_mut().zip(text.chunks(2)) {
        *byte = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_spelt_and_read_back_however_they_are_typed() {
        let bytes: Vec<u8> = (0..20u8).map(|n| n.wrapping_mul(37) ^ 0x5a).collect();
        let spelt = spell(&bytes);
        assert_eq!(spelt.len(), 32 + 7, "thirty-two letters in groups of four: {spelt}");
        assert_eq!(unspell(&spelt, 20).unwrap(), bytes);
        let typed = spelt.to_lowercase().replace('-', " ").replace('0', "o").replace('1', "l");
        assert_eq!(unspell(&format!("  {typed}\n"), 20).unwrap(), bytes);
        // A whole that is no multiple of five bits.
        for count in [1usize, 2, 3, 4, 5, 25] {
            let bytes: Vec<u8> = (0..count as u8).map(|n| 0xff - n).collect();
            assert_eq!(unspell(&spell(&bytes), count).unwrap(), bytes, "{count} bytes");
        }
        assert_eq!(unspell(&spelt[..spelt.len() - 1], 20), Err(Misspelt::Length));
        assert_eq!(unspell(&format!("{spelt}Z"), 20), Err(Misspelt::Length));
        assert_eq!(unspell(&spelt.replace(&spelt[..1], "U"), 20), Err(Misspelt::Letter('U')));
        assert_eq!(unspell(&"0".repeat(100_000), 20), Err(Misspelt::Length));
        // What does not fit the bytes it is to be: a last letter with bits beyond them.
        assert_eq!(unspell("ZZ", 1), Err(Misspelt::Length));
    }

    #[test]
    fn sixteen_digit_numbers_are_written_and_read_whole_or_not_at_all() {
        let bytes = [0u8, 1, 0xab, 0xff];
        assert_eq!(hex(&bytes), "0001abff");
        assert_eq!(unhex::<4>("0001abff"), Some(bytes));
        assert_eq!(unhex::<4>("0001ABFF"), Some(bytes));
        for wrong in ["", "0001abf", "0001abff0", "0001abfg", "0001 bff"] {
            assert_eq!(unhex::<4>(wrong), None, "{wrong}");
        }
    }
}
