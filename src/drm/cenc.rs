//! ISO Common Encryption (CENC) scheme identifiers.

/// Four-character codes used by `urn:mpeg:dash:mp4protection:2011` `@value`
/// and the ISOBMFF `schm` box `scheme_type` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommonEncryptionScheme {
    /// Full-sample AES-CTR (`cenc`).
    Cenc,
    /// Pattern-based AES-CBC (`cbcs`).
    Cbcs,
    /// Pattern-based AES-CTR (`cens`).
    Cens,
    /// Full-sample AES-CBC (`cbc1`).
    Cbc1,
}

impl CommonEncryptionScheme {
    /// Parse a case-insensitive 4CC (`cenc`, `cbcs`, `cens`, `cbc1`).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cenc" => Some(Self::Cenc),
            "cbcs" => Some(Self::Cbcs),
            "cens" => Some(Self::Cens),
            "cbc1" => Some(Self::Cbc1),
            _ => None,
        }
    }

    /// Parse from four raw ASCII bytes (e.g. `schm.scheme_type`).
    pub fn from_fourcc(bytes: &[u8; 4]) -> Option<Self> {
        Self::parse(std::str::from_utf8(bytes).ok()?)
    }

    /// Canonical lowercase 4CC.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cenc => "cenc",
            Self::Cbcs => "cbcs",
            Self::Cens => "cens",
            Self::Cbc1 => "cbc1",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_schemes_case_insensitive() {
        assert_eq!(
            CommonEncryptionScheme::parse("CBCS"),
            Some(CommonEncryptionScheme::Cbcs)
        );
        assert_eq!(
            CommonEncryptionScheme::parse(" cenc "),
            Some(CommonEncryptionScheme::Cenc)
        );
        assert_eq!(CommonEncryptionScheme::parse("xyz0"), None);
    }

    #[test]
    fn from_fourcc_reads_schm_bytes() {
        assert_eq!(
            CommonEncryptionScheme::from_fourcc(b"cbcs"),
            Some(CommonEncryptionScheme::Cbcs)
        );
    }
}
