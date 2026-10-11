//! On-the-fly Brotli compression for Vajra responses.
//! See `docs/compression.md` for the design.

use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompressionSettings {
    /// Master switch. `false` = pass every response through unchanged.
    pub enabled: bool,
    /// Brotli quality, 0..=11. Higher = smaller, slower.
    pub level: u32,
    /// Responses below this byte size are served uncompressed.
    pub min_size: usize,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawCompression {
    #[serde(default)]
    pub brotli: bool,
    #[serde(default = "default_level")]
    pub level: u32,
    #[serde(default = "default_min_size")]
    pub min_size: usize,
}

fn default_level() -> u32 {
    6
}
fn default_min_size() -> usize {
    1024
}

pub fn build_compression(raw: RawCompression) -> Result<CompressionSettings, String> {
    if raw.brotli {
        if raw.level > 11 {
            return Err(format!(
                "[compression] level must be 0..=11, got {}",
                raw.level
            ));
        }
        if raw.min_size == 0 {
            eprintln!(
                "vajra: warning: [compression] min_size = 0; every response will be compressed"
            );
        }
    }
    Ok(CompressionSettings {
        enabled: raw.brotli,
        level: raw.level,
        min_size: raw.min_size,
    })
}

/// Which content encoding we will actually apply.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Encoding {
    Brotli,
    Identity,
}

/// Parse `Accept-Encoding` and decide whether brotli is acceptable.
/// Returns `Brotli` iff `br` is present with q > 0, or `*` is present
/// with q > 0 and `br` is not explicitly rejected.
pub fn negotiate(header: Option<&str>) -> Encoding {
    let Some(h) = header else {
        return Encoding::Identity;
    };
    let mut br_q: Option<f32> = None;
    let mut star_q: Option<f32> = None;
    for tok in h.split(',') {
        let mut parts = tok.split(';');
        let name = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let mut q = 1.0f32;
        for p in parts {
            let p = p.trim();
            if let Some(v) = p.strip_prefix("q=") {
                if let Ok(x) = v.parse::<f32>() {
                    q = x;
                }
            }
        }
        match name.as_str() {
            "br" => br_q = Some(q),
            "*" => star_q = Some(q),
            _ => {}
        }
    }
    let eff = br_q.or(star_q).unwrap_or(0.0);
    if eff > 0.0 {
        Encoding::Brotli
    } else {
        Encoding::Identity
    }
}

/// True if the response is already a compressed binary format and
/// re-compressing it is pure waste. `Content-Type` is compared
/// case-insensitively with parameters stripped.
pub fn is_pre_compressed(content_type: &str) -> bool {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    // SVG is XML text — highly compressible despite the image/ prefix.
    if ct == "image/svg+xml" {
        return false;
    }
    const SKIP_EXACT: &[&str] = &[
        "application/zip",
        "application/gzip",
        "application/x-gzip",
        "application/x-7z-compressed",
        "application/x-rar-compressed",
        "application/x-bzip2",
        "application/zstd",
        "application/pdf",
        "font/woff",
        "font/woff2",
        "font/ttf",
        "font/otf",
        "application/vnd.ms-fontobject",
        "application/x-font-ttf",
        "application/x-font-otf",
    ];
    const SKIP_PREFIX: &[&str] = &["image/", "video/", "audio/"];
    if SKIP_EXACT.contains(&ct.as_str()) {
        return true;
    }
    SKIP_PREFIX.iter().any(|p| ct.starts_with(p))
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Compress,
    Skip,
}

/// Decide whether a given response should be compressed. Every gate is
/// independently testable; see the unit tests at the bottom of this file.
pub fn decide(
    settings: &CompressionSettings,
    accept_encoding: Option<&str>,
    content_type: &str,
    body_len: usize,
    is_head: bool,
) -> Plan {
    if !settings.enabled {
        return Plan::Skip;
    }
    if is_head {
        // HEAD responses carry no body to compress; skip to avoid burning
        // CPU on bytes nobody reads. See docs/compression.md.
        return Plan::Skip;
    }
    if body_len < settings.min_size {
        return Plan::Skip;
    }
    if is_pre_compressed(content_type) {
        return Plan::Skip;
    }
    if negotiate(accept_encoding) != Encoding::Brotli {
        return Plan::Skip;
    }
    Plan::Compress
}

/// Compress `body` with brotli at `level` (clamped to 0..=11).
/// Returns `None` on encoder failure; the caller then falls back to
/// identity and the response is served as-is.
pub fn brotli_compress(body: &[u8], level: u32) -> Option<Vec<u8>> {
    use brotli::enc::BrotliEncoderParams;
    let params = BrotliEncoderParams {
        quality: level.min(11) as i32,
        ..Default::default()
    };
    let mut out = Vec::with_capacity(body.len() / 2 + 16);
    if brotli::BrotliCompress(&mut &body[..], &mut out, &params).is_err() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiate_handles_simple_and_q_values() {
        assert_eq!(negotiate(None), Encoding::Identity);
        assert_eq!(negotiate(Some("")), Encoding::Identity);
        assert_eq!(negotiate(Some("gzip")), Encoding::Identity);
        assert_eq!(negotiate(Some("br")), Encoding::Brotli);
        assert_eq!(negotiate(Some("gzip, br")), Encoding::Brotli);
        assert_eq!(negotiate(Some("br;q=0.8, gzip;q=0.9")), Encoding::Brotli);
        assert_eq!(negotiate(Some("br;q=0")), Encoding::Identity);
        assert_eq!(negotiate(Some("gzip, *;q=0.5")), Encoding::Brotli);
        assert_eq!(negotiate(Some("*;q=0")), Encoding::Identity);
    }

    #[test]
    fn pre_compressed_types_are_skipped() {
        for ct in &[
            "image/png",
            "image/jpeg",
            "image/webp",
            "image/avif",
            "video/mp4",
            "audio/mpeg",
            "application/zip",
            "application/gzip",
            "application/pdf",
            "font/woff2",
            "image/PNG",
        ] {
            assert!(is_pre_compressed(ct), "{ct} should be skipped");
        }
        for ct in &[
            "text/html",
            "text/css",
            "application/json",
            "application/javascript",
            "image/svg+xml",
            "text/html; charset=utf-8",
        ] {
            assert!(!is_pre_compressed(ct), "{ct} should be compressible");
        }
    }

    #[test]
    fn decide_respects_all_gates() {
        let on = CompressionSettings {
            enabled: true,
            level: 6,
            min_size: 1024,
        };
        let off = CompressionSettings {
            enabled: false,
            ..on.clone()
        };

        assert_eq!(
            decide(&off, Some("br"), "text/html", 4096, false),
            Plan::Skip
        );
        assert_eq!(decide(&on, None, "text/html", 4096, false), Plan::Skip);
        assert_eq!(
            decide(&on, Some("gzip"), "text/html", 4096, false),
            Plan::Skip
        );
        assert_eq!(decide(&on, Some("br"), "text/html", 100, false), Plan::Skip);
        assert_eq!(
            decide(&on, Some("br"), "image/png", 4096, false),
            Plan::Skip
        );
        assert_eq!(decide(&on, Some("br"), "text/html", 4096, true), Plan::Skip);
        assert_eq!(
            decide(&on, Some("br"), "text/html", 4096, false),
            Plan::Compress
        );
    }

    #[test]
    fn brotli_roundtrip() {
        let data = b"hello world ".repeat(200);
        let comp = brotli_compress(&data, 6).expect("compression should succeed");
        assert!(comp.len() < data.len(), "compressed must be smaller");

        let mut dec = Vec::new();
        brotli::BrotliDecompress(&mut &comp[..], &mut dec).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn config_validation_rejects_bad_level() {
        let err = build_compression(RawCompression {
            brotli: true,
            level: 12,
            min_size: 1024,
        })
        .unwrap_err();
        assert!(err.contains("level"), "{err}");
    }
}
