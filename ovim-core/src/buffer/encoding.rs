use anyhow::Result;

/// File encoding for the buffer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileEncoding {
    /// UTF-8 (default for new files)
    #[default]
    Utf8,
    /// UTF-8 with BOM
    Utf8Bom,
    /// UTF-16 Little Endian
    Utf16Le,
    /// UTF-16 Big Endian
    Utf16Be,
    /// Windows-1252 (Western European)
    Windows1252,
    /// ISO-8859-1 (Latin-1)
    Latin1,
    /// Shift JIS (Japanese)
    ShiftJis,
    /// EUC-JP (Japanese)
    EucJp,
    /// GBK (Simplified Chinese)
    Gbk,
    /// Big5 (Traditional Chinese)
    Big5,
    /// EUC-KR (Korean)
    EucKr,
}

impl FileEncoding {
    /// Detects encoding from file bytes using chardetng and BOM detection
    pub fn detect(bytes: &[u8]) -> (Self, usize) {
        // Check for BOM first
        if bytes.len() >= 3 && bytes[0..3] == [0xEF, 0xBB, 0xBF] {
            return (FileEncoding::Utf8Bom, 3);
        }
        if bytes.len() >= 2 {
            if bytes[0..2] == [0xFE, 0xFF] {
                return (FileEncoding::Utf16Be, 2);
            }
            if bytes[0..2] == [0xFF, 0xFE] {
                return (FileEncoding::Utf16Le, 2);
            }
        }

        // Try UTF-8 first (most common)
        if std::str::from_utf8(bytes).is_ok() {
            return (FileEncoding::Utf8, 0);
        }

        // Use chardetng for encoding detection
        let mut detector = chardetng::EncodingDetector::new();
        detector.feed(bytes, true);
        let detected = detector.guess(None, true);

        let encoding = match detected.name() {
            "UTF-8" => FileEncoding::Utf8,
            "windows-1252" => FileEncoding::Windows1252,
            "ISO-8859-1" => FileEncoding::Latin1,
            "Shift_JIS" => FileEncoding::ShiftJis,
            "EUC-JP" => FileEncoding::EucJp,
            "GBK" | "gb18030" => FileEncoding::Gbk,
            "Big5" => FileEncoding::Big5,
            "EUC-KR" => FileEncoding::EucKr,
            "UTF-16LE" => FileEncoding::Utf16Le,
            "UTF-16BE" => FileEncoding::Utf16Be,
            _ => FileEncoding::Latin1, // Fallback - Latin-1 accepts any byte
        };

        (encoding, 0)
    }

    /// Decodes bytes to UTF-8 string using this encoding
    pub fn decode(&self, bytes: &[u8], bom_offset: usize) -> Result<String> {
        let bytes = &bytes[bom_offset..];

        match self {
            FileEncoding::Utf8 | FileEncoding::Utf8Bom => String::from_utf8(bytes.to_vec())
                .map_err(|e| anyhow::anyhow!("Invalid UTF-8: {}", e)),
            FileEncoding::Utf16Le => {
                let u16_vec: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|chunk| u16::from_le_bytes(*chunk))
                    .collect();
                String::from_utf16(&u16_vec).map_err(|e| anyhow::anyhow!("Invalid UTF-16LE: {}", e))
            }
            FileEncoding::Utf16Be => {
                let u16_vec: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|chunk| u16::from_be_bytes(*chunk))
                    .collect();
                String::from_utf16(&u16_vec).map_err(|e| anyhow::anyhow!("Invalid UTF-16BE: {}", e))
            }
            _ => {
                // Use encoding_rs for other encodings
                let encoding = self.as_encoding_rs();
                let (cow, _, had_errors) = encoding.decode(bytes);
                if had_errors {
                    // Note: Decoding errors handled with replacement chars - don't print to stderr
                    // This avoids interrupting user output
                }
                Ok(cow.into_owned())
            }
        }
    }

    /// Encodes UTF-8 string back to original encoding for saving
    pub fn encode(&self, content: &str) -> Result<Vec<u8>> {
        match self {
            FileEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
            FileEncoding::Utf8Bom => {
                let mut bytes = vec![0xEF, 0xBB, 0xBF];
                bytes.extend_from_slice(content.as_bytes());
                Ok(bytes)
            }
            FileEncoding::Utf16Le => {
                let mut bytes = vec![0xFF, 0xFE]; // BOM
                for c in content.encode_utf16() {
                    bytes.extend_from_slice(&c.to_le_bytes());
                }
                Ok(bytes)
            }
            FileEncoding::Utf16Be => {
                let mut bytes = vec![0xFE, 0xFF]; // BOM
                for c in content.encode_utf16() {
                    bytes.extend_from_slice(&c.to_be_bytes());
                }
                Ok(bytes)
            }
            _ => {
                // Use encoding_rs for other encodings
                let encoding = self.as_encoding_rs();
                let (cow, _, had_errors) = encoding.encode(content);
                if had_errors {
                    return Err(anyhow::anyhow!(
                        "Some characters cannot be represented in {:?}",
                        self
                    ));
                }
                Ok(cow.into_owned())
            }
        }
    }

    /// Convert to encoding_rs Encoding
    fn as_encoding_rs(&self) -> &'static encoding_rs::Encoding {
        match self {
            FileEncoding::Utf8 | FileEncoding::Utf8Bom => encoding_rs::UTF_8,
            FileEncoding::Utf16Le => encoding_rs::UTF_16LE,
            FileEncoding::Utf16Be => encoding_rs::UTF_16BE,
            FileEncoding::Windows1252 => encoding_rs::WINDOWS_1252,
            FileEncoding::Latin1 => encoding_rs::WINDOWS_1252, // Close enough
            FileEncoding::ShiftJis => encoding_rs::SHIFT_JIS,
            FileEncoding::EucJp => encoding_rs::EUC_JP,
            FileEncoding::Gbk => encoding_rs::GBK,
            FileEncoding::Big5 => encoding_rs::BIG5,
            FileEncoding::EucKr => encoding_rs::EUC_KR,
        }
    }

    /// Returns a short display name for the status line
    pub fn display_name(&self) -> &'static str {
        match self {
            FileEncoding::Utf8 => "UTF-8",
            FileEncoding::Utf8Bom => "UTF-8 BOM",
            FileEncoding::Utf16Le => "UTF-16LE",
            FileEncoding::Utf16Be => "UTF-16BE",
            FileEncoding::Windows1252 => "CP1252",
            FileEncoding::Latin1 => "Latin-1",
            FileEncoding::ShiftJis => "Shift-JIS",
            FileEncoding::EucJp => "EUC-JP",
            FileEncoding::Gbk => "GBK",
            FileEncoding::Big5 => "Big5",
            FileEncoding::EucKr => "EUC-KR",
        }
    }
}
