//! On-the-fly compression for Vajra responses.
//! See `docs/compression.md` for the design.
//!
//! Brotli preferred, gzip fallback, identity otherwise. Both codecs are
//! opt-in via `[compression] brotli` / `[compression] gzip`.

use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompressionSettings {
    /// Brotli enabled.
    pub brotli: bool,
    /// Gzip enabled.
    pub gzip: bool,
    /// Brotli quality, 0..=11.
    pub level: u32,
    /// Gzip level, 1..=9.
    pub gzip_level: u32,
    /// Responses below this size are served uncompressed.
    pub min_size: usize,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawCompression {
    #[serde(default)]
    pub brotli: bool,
    #[serde(default)]
    pub gzip: bool,
    #[serde(default = "default_level")]
    pub level: u32,
    #[serde(default = "default_gzip_level")]
    pub gzip_level: u32,
    #[serde(default = "default_min_size")]
    pub min_size: usize,
}

fn default_level() -> u32 {
    6
}
fn default_gzip_level() -> u32 {
    6
}
fn default_min_size() -> usize {
    1024
}

pub fn build_compression(raw: RawCompression) -> Result<CompressionSettings, String> {
    if raw.brotli && raw.level > 11 {
        return Err(format!(
            "[compression] level (brotli) must be 0..=11, got {}",
            raw.level
        ));
    }
    if raw.gzip && (raw.gzip_level < 1 || raw.gzip_level > 9) {
        return Err(format!(
            "[compression] gzip_level must be 1..=9, got {}",
            raw.gzip_level
        ));
    }
    if raw.brotli && raw.min_size == 0 {
        eprintln!("vajra: warning: [compression] min_size = 0; every response will be compressed");
    }
    if raw.gzip && raw.min_size == 0 {
        eprintln!("vajra: warning: [compression] min_size = 0; every response will be compressed");
    }
    Ok(CompressionSettings {
        brotli: raw.brotli,
        gzip: raw.gzip,
        level: raw.level,
        gzip_level: raw.gzip_level,
        min_size: raw.min_size,
    })
}

/// Which content encoding we will apply.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Encoding {
    Brotli,
    Gzip,
    Identity,
}

/// Parse `Accept-Encoding` and pick the best encoding available.
/// Preference order (when both are enabled and equally acceptable):
/// **brotli > gzip**. `identity` is the fallback.
///
/// - `br;q=0` explicitly disables brotli.
/// - `*;q=0` disables both.
/// - A codec that isn't enabled in settings is never returned.
pub fn negotiate(header: Option<&str>, comp: &CompressionSettings) -> Encoding {
    if !comp.brotli && !comp.gzip {
        return Encoding::Identity;
    }
    let Some(h) = header else {
        return Encoding::Identity;
    };
    let mut br_q: Option<f32> = None;
    let mut gz_q: Option<f32> = None;
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
            "gzip" | "x-gzip" => gz_q = Some(q),
            "*" => star_q = Some(q),
            _ => {}
        }
    }
    let eff_br = if comp.brotli {
        br_q.or(star_q).unwrap_or(0.0)
    } else {
        0.0
    };
    let eff_gz = if comp.gzip {
        gz_q.or(star_q).unwrap_or(0.0)
    } else {
        0.0
    };

    if eff_br > 0.0 && eff_br >= eff_gz {
        Encoding::Brotli
    } else if eff_gz > 0.0 {
        Encoding::Gzip
    } else {
        Encoding::Identity
    }
}

/// True if the response is already a compressed binary format and
/// re-compressing it is pure waste.
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

/// Decide whether a given response should be compressed.
pub fn decide(
    settings: &CompressionSettings,
    accept_encoding: Option<&str>,
    content_type: &str,
    body_len: usize,
    is_head: bool,
) -> Plan {
    if !settings.brotli && !settings.gzip {
        return Plan::Skip;
    }
    if is_head {
        return Plan::Skip;
    }
    if body_len < settings.min_size {
        return Plan::Skip;
    }
    if is_pre_compressed(content_type) {
        return Plan::Skip;
    }
    if negotiate(accept_encoding, settings) == Encoding::Identity {
        return Plan::Skip;
    }
    Plan::Compress
}

/// Compress `body` with brotli at `level` (clamped to 0..=11).
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

/// Compress `body` with gzip at `level` (clamped to 1..=9).
pub fn gzip_compress(body: &[u8], level: u32) -> Option<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    let lvl = level.clamp(1, 9);
    let mut enc = GzEncoder::new(
        Vec::with_capacity(body.len() / 2 + 32),
        Compression::new(lvl),
    );
    if enc.write_all(body).is_err() {
        return None;
    }
    enc.finish().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both() -> CompressionSettings {
        CompressionSettings {
            brotli: true,
            gzip: true,
            level: 6,
            gzip_level: 6,
            min_size: 1024,
        }
    }
    fn br_only() -> CompressionSettings {
        CompressionSettings {
            brotli: true,
            gzip: false,
            level: 6,
            gzip_level: 6,
            min_size: 1024,
        }
    }
    fn gz_only() -> CompressionSettings {
        CompressionSettings {
            brotli: false,
            gzip: true,
            level: 6,
            gzip_level: 6,
            min_size: 1024,
        }
    }
    fn neither() -> CompressionSettings {
        CompressionSettings {
            brotli: false,
            gzip: false,
            level: 6,
            gzip_level: 6,
            min_size: 1024,
        }
    }

    #[test]
    fn negotiate_prefers_brotli_over_gzip() {
        let s = both();
        assert_eq!(negotiate(Some("gzip, br"), &s), Encoding::Brotli);
        assert_eq!(negotiate(Some("br, gzip"), &s), Encoding::Brotli);
        assert_eq!(
            negotiate(Some("br;q=0.8, gzip;q=0.8"), &s),
            Encoding::Brotli
        );
    }

    #[test]
    fn negotiate_falls_back_to_gzip() {
        let s = both();
        assert_eq!(negotiate(Some("gzip"), &s), Encoding::Gzip);
        assert_eq!(negotiate(Some("br;q=0, gzip"), &s), Encoding::Gzip);
        assert_eq!(negotiate(Some("*;q=0.5"), &s), Encoding::Brotli);
    }

    #[test]
    fn negotiate_respects_disabled_codecs() {
        assert_eq!(negotiate(Some("br, gzip"), &br_only()), Encoding::Brotli);
        assert_eq!(negotiate(Some("gzip"), &br_only()), Encoding::Identity);
        assert_eq!(negotiate(Some("br"), &gz_only()), Encoding::Identity);
        assert_eq!(negotiate(Some("gzip"), &gz_only()), Encoding::Gzip);
        assert_eq!(negotiate(Some("br, gzip"), &neither()), Encoding::Identity);
    }

    #[test]
    fn negotiate_handles_edge_cases() {
        let s = both();
        assert_eq!(negotiate(None, &s), Encoding::Identity);
        assert_eq!(negotiate(Some(""), &s), Encoding::Identity);
        assert_eq!(negotiate(Some("*;q=0"), &s), Encoding::Identity);
        assert_eq!(negotiate(Some("gzip;q=0"), &s), Encoding::Identity);
        assert_eq!(negotiate(Some("identity"), &s), Encoding::Identity);
    }

    #[test]
    fn pre_compressed_types_are_skipped() {
        for ct in &["image/png", "video/mp4", "application/zip", "font/woff2"] {
            assert!(is_pre_compressed(ct), "{ct}");
        }
        for ct in &["text/html", "application/json", "image/svg+xml"] {
            assert!(!is_pre_compressed(ct), "{ct}");
        }
    }

    #[test]
    fn decide_respects_all_gates() {
        let s = both();
        let off = neither();
        assert_eq!(
            decide(&off, Some("br"), "text/html", 4096, false),
            Plan::Skip
        );
        assert_eq!(decide(&s, None, "text/html", 4096, false), Plan::Skip);
        assert_eq!(
            decide(&s, Some("gzip"), "text/html", 100, false),
            Plan::Skip
        );
        assert_eq!(decide(&s, Some("br"), "image/png", 4096, false), Plan::Skip);
        assert_eq!(decide(&s, Some("br"), "text/html", 4096, true), Plan::Skip);
        assert_eq!(
            decide(&s, Some("br"), "text/html", 4096, false),
            Plan::Compress
        );
        assert_eq!(
            decide(&s, Some("gzip"), "text/html", 4096, false),
            Plan::Compress
        );
    }

    #[test]
    fn brotli_roundtrip() {
        let data = b"hello world ".repeat(200);
        let comp = brotli_compress(&data, 6).unwrap();
        assert!(comp.len() < data.len());
        let mut dec = Vec::new();
        brotli::BrotliDecompress(&mut &comp[..], &mut dec).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn gzip_roundtrip() {
        use std::io::Read;
        let data = b"hello world ".repeat(200);
        let comp = gzip_compress(&data, 6).unwrap();
        assert!(comp.len() < data.len());
        let mut dec = Vec::new();
        flate2::read::GzDecoder::new(&comp[..])
            .read_to_end(&mut dec)
            .unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn config_validation() {
        assert!(build_compression(RawCompression {
            brotli: true,
            level: 12,
            ..Default::default()
        })
        .is_err());
        assert!(build_compression(RawCompression {
            gzip: true,
            gzip_level: 0,
            ..Default::default()
        })
        .is_err());
        assert!(build_compression(RawCompression {
            gzip: true,
            gzip_level: 10,
            ..Default::default()
        })
        .is_err());
        assert!(build_compression(RawCompression {
            gzip: true,
            gzip_level: 6,
            ..Default::default()
        })
        .is_ok());
    }
}
