//! VST3 class ids, and the one way this host writes them.

use std::fmt;
use vst3::Steinberg::TUID;

/// Identifies a class of a VST3 module: a plugin's processor, or its edit
/// controller.
///
/// The SDK declares a class id as four 32-bit numbers, and it is those
/// numbers that are the same everywhere. The 16 bytes a plugin hands out
/// are not: on Windows the first two numbers are stored like a COM `GUID`,
/// least significant byte first. So the id is kept, and written, as the
/// numbers: 32 hexadecimal digits, exactly what the SDK's `FUID::toString`
/// prints and what a plugin's `moduleinfo.json` lists as its `CID`. A
/// project saved on one platform then names the same plugin on another.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ClassId {
    /// The four numbers, each most significant byte first.
    bytes: [u8; 16],
}

/// Where each byte of the written form sits in a `TUID`. The mapping only
/// swaps bytes in pairs, so it also turns a `TUID` back into the written
/// form.
#[cfg(windows)]
const TUID_ORDER: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
#[cfg(not(windows))]
const TUID_ORDER: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

impl ClassId {
    /// The id a plugin reports or expects in memory.
    pub(crate) fn from_tuid(tuid: &TUID) -> Self {
        Self { bytes: TUID_ORDER.map(|index| tuid[index] as u8) }
    }

    pub(crate) fn to_tuid(self) -> TUID {
        TUID_ORDER.map(|index| self.bytes[index] as std::ffi::c_char)
    }

    /// Reads the written form: exactly 32 hexadecimal digits, in either
    /// case.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let digits = text.as_bytes();
        if digits.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 16];
        for (byte, pair) in bytes.iter_mut().zip(digits.chunks_exact(2)) {
            let high = char::from(pair[0]).to_digit(16)?;
            let low = char::from(pair[1]).to_digit(16)?;
            *byte = (high * 16 + low) as u8;
        }
        Some(Self { bytes })
    }

    /// Whether every byte is zero, which no real class has. Plugins leave
    /// an id like that where they have nothing to say, for example as the
    /// controller class of a processor that is its own controller.
    pub(crate) fn is_nil(self) -> bool {
        self.bytes == [0; 16]
    }
}

impl fmt::Display for ClassId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.bytes.iter().try_for_each(|byte| write!(f, "{byte:02X}"))
    }
}

impl fmt::Debug for ClassId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClassId({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The class id of a plugin the way its `moduleinfo.json` lists it, and
    /// the numbers its source declares it with.
    const WRITTEN: &str = "ABCDEF019182FAEB5776737957767331";
    const NUMBERS: [u32; 4] = [0xABCD_EF01, 0x9182_FAEB, 0x5776_7379, 0x5776_7331];

    #[test]
    fn an_id_is_written_as_its_four_numbers() {
        // `vst3::uid` lays the numbers out the way the SDK does on this
        // platform, so this holds on all of them.
        let [a, b, c, d] = NUMBERS;
        let tuid = vst3::uid(a, b, c, d);
        let id = ClassId::from_tuid(&tuid);
        assert_eq!(id.to_string(), WRITTEN);
        assert_eq!(id.to_tuid(), tuid, "the plugin gets back the bytes it gave");
        assert_eq!(format!("{id:?}"), format!("ClassId({WRITTEN})"));
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_the_first_two_numbers_are_stored_like_a_guid() {
        let id = ClassId::parse("00112233445566778899AABBCCDDEEFF").unwrap();
        let bytes = id.to_tuid().map(|byte| byte as u8);
        assert_eq!(
            bytes,
            [0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]
        );
    }

    #[test]
    fn parsing_accepts_exactly_32_hex_digits() {
        let id = ClassId::parse(WRITTEN).unwrap();
        assert_eq!(id.to_string(), WRITTEN);
        assert_eq!(ClassId::parse(&WRITTEN.to_lowercase()), Some(id), "case does not matter when reading");
        assert_eq!(ClassId::parse(&WRITTEN.to_lowercase()).unwrap().to_string(), WRITTEN, "and is upper when written");

        for bad in ["", "ABCDEF", &WRITTEN[1..], &format!("{WRITTEN}0"), &WRITTEN.replace('A', "G")] {
            assert_eq!(ClassId::parse(bad), None, "{bad:?}");
        }
        // 32 bytes, but not 32 digits.
        assert_eq!(ClassId::parse("ABCDEF019182FAEB57767379577673é"), None);
        assert_eq!(ClassId::parse("+BCDEF019182FAEB5776737957767331"), None);
    }

    #[test]
    fn the_nil_id_is_recognized() {
        assert!(ClassId::from_tuid(&[0; 16]).is_nil());
        assert!(!ClassId::parse(WRITTEN).unwrap().is_nil());
    }
}
