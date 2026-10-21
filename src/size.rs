//! Human readable byte sizes ("20MiB", "1.5GB", "unlimited").

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteSize(pub u64);

impl FromStr for ByteSize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let split = s
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(s.len());
        let (num, unit) = s.split_at(split);
        let num: f64 = num.parse().map_err(|_| format!("invalid size: {s:?}"))?;
        let factor: u64 = match unit.trim().to_ascii_lowercase().as_str() {
            "" | "b" => 1,
            "k" | "kb" => 1_000,
            "kib" => 1 << 10,
            "m" | "mb" => 1_000_000,
            "mib" => 1 << 20,
            "g" | "gb" => 1_000_000_000,
            "gib" => 1 << 30,
            "t" | "tb" => 1_000_000_000_000,
            "tib" => 1 << 40,
            other => return Err(format!("unknown size unit: {other:?}")),
        };
        let bytes = num * factor as f64;
        if !bytes.is_finite() || bytes < 0.0 || bytes > i64::MAX as f64 {
            return Err(format!("size out of range: {s:?}"));
        }
        Ok(ByteSize(bytes.round() as u64))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        let mut value = self.0 as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            write!(f, "{} B", self.0)
        } else {
            write!(f, "{value:.1} {}", UNITS[unit])
        }
    }
}

/// A quota limit: either a number or "unlimited".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit<T>(pub Option<T>);

impl<T: FromStr<Err = String>> FromStr for Limit<T> {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "unlimited" | "none" => Ok(Limit(None)),
            _ => s.parse().map(|v| Limit(Some(v))),
        }
    }
}

/// Plain integer counts, so `Limit<Count>` parses like `Limit<ByteSize>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Count(pub u64);

impl FromStr for Count {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.trim()
            .parse()
            .map(Count)
            .map_err(|_| format!("invalid number: {s:?}"))
    }
}

impl From<ByteSize> for u64 {
    fn from(v: ByteSize) -> u64 {
        v.0
    }
}

impl From<Count> for u64 {
    fn from(v: Count) -> u64 {
        v.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!("20MiB".parse(), Ok(ByteSize(20 << 20)));
        assert_eq!("1.5 GB".parse(), Ok(ByteSize(1_500_000_000)));
        assert_eq!("512".parse(), Ok(ByteSize(512)));
        assert!("12 parsecs".parse::<ByteSize>().is_err());
        assert_eq!("unlimited".parse(), Ok(Limit::<ByteSize>(None)));
        assert_eq!("1kib".parse(), Ok(Limit(Some(ByteSize(1024)))));
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(ByteSize(100).to_string(), "100 B");
        assert_eq!(ByteSize(3 << 20).to_string(), "3.0 MiB");
    }
}
