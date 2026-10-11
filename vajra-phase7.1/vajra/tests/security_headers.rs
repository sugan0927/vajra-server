//! Security-header policy: parsing, prefix override, remove semantics.

use vajra::config::{apply_security_headers, HeaderOverride, HeaderSettings};

fn v(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn has(h: &[(String, String)], name: &str, value: &str) -> bool {
    h.iter()
        .any(|(n, x)| n.eq_ignore_ascii_case(name) && x == value)
}

fn count(h: &[(String, String)], name: &str) -> usize {
    h.iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .count()
}

#[test]
fn empty_policy_is_noop() {
    let cfg = HeaderSettings::default();
    let mut h = v(&[("Content-Type", "text/html")]);
    apply_security_headers(&cfg, "/", &mut h);
    assert_eq!(h, v(&[("Content-Type", "text/html")]));
}

#[test]
fn top_level_headers_are_applied() {
    let cfg = HeaderSettings {
        hsts: Some("max-age=31536000".into()),
        x_frame_options: Some("DENY".into()),
        x_content_type_options: Some("nosniff".into()),
        referrer_policy: Some("strict-origin-when-cross-origin".into()),
        permissions_policy: Some("geolocation=()".into()),
        overrides: vec![],
        content_security_policy: None,
    };
    let mut h = vec![];
    apply_security_headers(&cfg, "/anywhere", &mut h);
    assert!(has(&h, "Strict-Transport-Security", "max-age=31536000"));
    assert!(has(&h, "X-Frame-Options", "DENY"));
    assert!(has(&h, "X-Content-Type-Options", "nosniff"));
    assert!(has(
        &h,
        "Referrer-Policy",
        "strict-origin-when-cross-origin"
    ));
    assert!(has(&h, "Permissions-Policy", "geolocation=()"));
}

#[test]
fn managed_header_is_replaced_not_duplicated() {
    let cfg = HeaderSettings {
        x_frame_options: Some("DENY".into()),
        ..Default::default()
    };
    let mut h = v(&[
        ("X-Frame-Options", "SAMEORIGIN"),
        ("Content-Type", "text/html"),
    ]);
    apply_security_headers(&cfg, "/", &mut h);
    assert_eq!(count(&h, "X-Frame-Options"), 1);
    assert!(has(&h, "X-Frame-Options", "DENY"));
}

#[test]
fn managed_header_replace_is_case_insensitive() {
    let cfg = HeaderSettings {
        x_frame_options: Some("DENY".into()),
        ..Default::default()
    };
    let mut h = v(&[("x-frame-options", "SAMEORIGIN")]);
    apply_security_headers(&cfg, "/", &mut h);
    assert_eq!(count(&h, "X-Frame-Options"), 1);
}

#[test]
fn unmanaged_upstream_header_is_left_alone() {
    // We don't set X-Frame-Options; upstream set it; we must not touch it.
    let cfg = HeaderSettings::default();
    let mut h = v(&[("X-Frame-Options", "SAMEORIGIN")]);
    apply_security_headers(&cfg, "/", &mut h);
    assert!(has(&h, "X-Frame-Options", "SAMEORIGIN"));
}

#[test]
fn override_replaces_value_for_matching_prefix() {
    let cfg = HeaderSettings {
        x_frame_options: Some("DENY".into()),
        overrides: vec![HeaderOverride {
            path: "/api/".into(),
            x_frame_options: Some("SAMEORIGIN".into()),
            hsts: None,
            x_content_type_options: None,
            referrer_policy: None,
            permissions_policy: None,
            content_security_policy: None,
        }],
        ..Default::default()
    };

    let mut inside = vec![];
    apply_security_headers(&cfg, "/api/users", &mut inside);
    assert!(has(&inside, "X-Frame-Options", "SAMEORIGIN"));

    let mut outside = vec![];
    apply_security_headers(&cfg, "/dashboard", &mut outside);
    assert!(has(&outside, "X-Frame-Options", "DENY"));
}

#[test]
fn override_with_empty_string_removes_header() {
    let cfg = HeaderSettings {
        hsts: Some("max-age=31536000".into()),
        x_frame_options: Some("DENY".into()),
        overrides: vec![HeaderOverride {
            path: "/embed/".into(),
            x_frame_options: Some(String::new()), // remove
            hsts: None,
            x_content_type_options: None,
            referrer_policy: None,
            permissions_policy: None,
            content_security_policy: None,
        }],
        ..Default::default()
    };

    let mut h = vec![];
    apply_security_headers(&cfg, "/embed/frame", &mut h);
    assert_eq!(count(&h, "X-Frame-Options"), 0);
    assert!(has(&h, "Strict-Transport-Security", "max-age=31536000"));
}

#[test]
fn override_with_empty_string_also_strips_upstream_header() {
    let cfg = HeaderSettings {
        x_frame_options: Some("DENY".into()),
        overrides: vec![HeaderOverride {
            path: "/embed/".into(),
            x_frame_options: Some(String::new()),
            hsts: None,
            x_content_type_options: None,
            referrer_policy: None,
            permissions_policy: None,
            content_security_policy: None,
        }],
        ..Default::default()
    };
    let mut h = v(&[("X-Frame-Options", "SAMEORIGIN")]);
    apply_security_headers(&cfg, "/embed/", &mut h);
    assert_eq!(count(&h, "X-Frame-Options"), 0);
}

#[test]
fn longest_prefix_override_wins() {
    let cfg = HeaderSettings {
        hsts: Some("base".into()),
        overrides: vec![
            HeaderOverride {
                path: "/a/".into(),
                hsts: Some("a-val".into()),
                x_frame_options: None,
                x_content_type_options: None,
                referrer_policy: None,
                permissions_policy: None,
                content_security_policy: None,
            },
            HeaderOverride {
                path: "/a/b/".into(),
                hsts: Some("ab-val".into()),
                x_frame_options: None,
                x_content_type_options: None,
                referrer_policy: None,
                permissions_policy: None,
                content_security_policy: None,
            },
        ],
        ..Default::default()
    };

    let mut h1 = vec![];
    apply_security_headers(&cfg, "/a/b/x", &mut h1);
    assert!(has(&h1, "Strict-Transport-Security", "ab-val"));

    let mut h2 = vec![];
    apply_security_headers(&cfg, "/a/x", &mut h2);
    assert!(has(&h2, "Strict-Transport-Security", "a-val"));

    let mut h3 = vec![];
    apply_security_headers(&cfg, "/other", &mut h3);
    assert!(has(&h3, "Strict-Transport-Security", "base"));
}

#[test]
fn override_partial_leaves_other_headers_inherited() {
    let cfg = HeaderSettings {
        hsts: Some("h".into()),
        x_frame_options: Some("DENY".into()),
        overrides: vec![HeaderOverride {
            path: "/x/".into(),
            hsts: Some("overridden".into()),
            x_frame_options: None, // inherit DENY
            x_content_type_options: None,
            referrer_policy: None,
            permissions_policy: None,
            content_security_policy: None,
        }],
        ..Default::default()
    };
    let mut h = vec![];
    apply_security_headers(&cfg, "/x/", &mut h);
    assert!(has(&h, "Strict-Transport-Security", "overridden"));
    assert!(has(&h, "X-Frame-Options", "DENY"));
}

// ─── Config parsing ──────────────────────────────────────────────────

fn parse_cfg(body: &str) -> Result<vajra::config::Settings, String> {
    // Base dir for the parser is a real path; the config text is prepended
    // with a [static] section so static-root validation passes.
    let tmp = std::env::temp_dir().join(format!("vhdr-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let full = format!("[static]\nroot = {:?}\n{}", tmp.to_str().unwrap(), body);
    vajra::config::Settings::from_toml_str(&full, &tmp)
}

#[test]
fn config_parses_all_fields() {
    let s = parse_cfg(
        "[headers]\n\
         hsts = \"max-age=31536000\"\n\
         x_frame_options = \"DENY\"\n\
         x_content_type_options = \"nosniff\"\n\
         referrer_policy = \"no-referrer\"\n\
         permissions_policy = \"geolocation=()\"\n\
         [[headers.overrides]]\n\
         path = \"/api/\"\n\
         hsts = \"max-age=60\"\n",
    )
    .unwrap();
    assert_eq!(s.headers.hsts.as_deref(), Some("max-age=31536000"));
    assert_eq!(s.headers.x_frame_options.as_deref(), Some("DENY"));
    assert_eq!(s.headers.overrides.len(), 1);
    assert_eq!(s.headers.overrides[0].path, "/api/");
    assert_eq!(s.headers.overrides[0].hsts.as_deref(), Some("max-age=60"));
}

#[test]
fn config_rejects_empty_top_level() {
    let err = parse_cfg("[headers]\nhsts = \"\"").unwrap_err();
    assert!(err.contains("empty value"), "{err}");
}

#[test]
fn config_rejects_crlf_injection() {
    let err = parse_cfg("[headers]\nhsts = \"a\\nb\"").unwrap_err();
    assert!(err.contains("CR/LF"), "{err}");
}

#[test]
fn config_rejects_relative_override_path() {
    let err = parse_cfg(
        "[headers]\nx_frame_options = \"DENY\"\n\
         [[headers.overrides]]\npath = \"api/\"\nx_frame_options = \"SAMEORIGIN\"",
    )
    .unwrap_err();
    assert!(err.contains("absolute"), "{err}");
}

#[test]
fn config_rejects_unknown_key() {
    let err = parse_cfg("[headers]\nbogus = \"x\"").unwrap_err();
    assert!(err.contains("bogus") || err.contains("unknown"), "{err}");
}

// ─── Content-Security-Policy ─────────────────────────────────────────
#[test]
fn csp_top_level_applied() {
    let cfg = HeaderSettings {
        content_security_policy: Some("default-src 'self'".into()),
        ..HeaderSettings::default()
    };
    let mut h = vec![];
    apply_security_headers(&cfg, "/any", &mut h);
    assert!(has(&h, "Content-Security-Policy", "default-src 'self'"));
}

#[test]
fn csp_per_path_override() {
    let cfg = HeaderSettings {
        content_security_policy: Some("default-src 'self'".into()),
        overrides: vec![HeaderOverride {
            path: "/admin/".into(),
            content_security_policy: Some("default-src 'self'; frame-ancestors 'none'".into()),
            ..HeaderOverride::default()
        }],
        ..HeaderSettings::default()
    };
    let mut admin = vec![];
    apply_security_headers(&cfg, "/admin/users", &mut admin);
    assert!(has(
        &admin,
        "Content-Security-Policy",
        "default-src 'self'; frame-ancestors 'none'"
    ));
    let mut public = vec![];
    apply_security_headers(&cfg, "/blog/", &mut public);
    assert!(has(
        &public,
        "Content-Security-Policy",
        "default-src 'self'"
    ));
}

#[test]
fn csp_removed_by_empty_string_override() {
    let cfg = HeaderSettings {
        content_security_policy: Some("default-src 'self'".into()),
        overrides: vec![HeaderOverride {
            path: "/embed/".into(),
            content_security_policy: Some(String::new()),
            ..HeaderOverride::default()
        }],
        ..HeaderSettings::default()
    };
    let mut h = vec![];
    apply_security_headers(&cfg, "/embed/widget", &mut h);
    assert_eq!(count(&h, "Content-Security-Policy"), 0);
}

#[test]
fn wordpress_csp_const_is_valid() {
    let v = vajra::config::WORDPRESS_CSP;
    assert!(!v.is_empty());
    assert!(!v.contains('\n'), "must be single-line");
    assert!(!v.contains('\r'), "must be single-line");
    assert!(v.contains("default-src 'self'"), "{v}");
    assert!(v.contains("frame-ancestors 'self'"), "{v}");
}

#[test]
fn csp_config_parses_from_toml() {
    let cfg = parse_cfg("[headers]\ncontent_security_policy = \"default-src 'self'\"\n").unwrap();
    assert_eq!(
        cfg.headers.content_security_policy.as_deref(),
        Some("default-src 'self'")
    );
}

#[test]
fn csp_config_rejects_crlf() {
    // Two-layer defense: the TOML crate itself rejects a literal newline
    // inside a basic string ("invalid basic string"), and our own
    // validation rejects CR/LF if the string somehow reaches us as a
    // raw value (e.g. from a programmatic config). Either way the
    // config must not parse.
    let err = parse_cfg("[headers]\ncontent_security_policy = \"a\nb\"").unwrap_err();
    assert!(
        err.contains("CR/LF") || err.contains("invalid basic string"),
        "unexpected error: {err}"
    );
}
