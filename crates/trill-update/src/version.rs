//! A version of Trill: three numbers, as `Cargo.toml` and a release's tag
//! have them.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// A version of Trill. A later one is greater.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for Version {
    type Err = String;

    /// `0.2.0`, or a tag's `v0.2.0`: three numbers and nothing else.
    fn from_str(text: &str) -> Result<Self, String> {
        let wrong = || format!("'{text}' is no version of Trill: one is three numbers, as in 0.2.0");
        let numbers = text.strip_prefix('v').unwrap_or(text);
        let mut parts = numbers.split('.').map(|part| {
            // Digits only: no sign, no space, and nothing so long that it is no number.
            let plain = !part.is_empty() && part.len() <= 9 && part.bytes().all(|byte| byte.is_ascii_digit());
            plain.then(|| part.parse::<u32>().ok()).flatten()
        });
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => Ok(Self { major, minor, patch }),
            _ => Err(wrong()),
        }
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_read_as_it_is_written_and_a_tag_too() {
        let version: Version = "0.12.3".parse().unwrap();
        assert_eq!(version, Version::new(0, 12, 3));
        assert_eq!(version.to_string(), "0.12.3");
        assert_eq!("v1.0.0".parse::<Version>().unwrap(), Version::new(1, 0, 0));
        let json = serde_json::to_string(&version).unwrap();
        assert_eq!(json, "\"0.12.3\"");
        assert_eq!(serde_json::from_str::<Version>(&json).unwrap(), version);
    }

    #[test]
    fn a_later_version_is_greater_by_its_numbers_and_not_by_its_letters() {
        let order = ["0.1.0", "0.1.9", "0.1.10", "0.2.0", "0.10.0", "1.0.0"];
        let versions: Vec<Version> = order.iter().map(|text| text.parse().unwrap()).collect();
        assert!(versions.windows(2).all(|pair| pair[0] < pair[1]), "{versions:?}");
    }

    #[test]
    fn what_is_not_three_numbers_is_refused_in_a_sentence() {
        for text in ["", "1", "1.2", "1.2.3.4", "1.2.x", "1..3", "+1.2.3", "1.2.3-beta", " 1.2.3", "1.2.99999999999"] {
            let refusal = text.parse::<Version>().unwrap_err();
            assert!(refusal.contains("three numbers"), "{text}: {refusal}");
        }
    }
}
