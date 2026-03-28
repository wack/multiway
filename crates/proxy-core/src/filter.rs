// Pure filter computation (Sans-I/O).
//
// Computes request/response transformations (header modification, redirects,
// URL rewrites, mirroring) as pure data. Filter functions take a request and
// filter configuration, returning the modified request or response metadata.

use crate::config::{Backend, Filter, HeaderValue, PathModifier};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Accumulated filter effects collected from a list of [`Filter`] values.
///
/// Header modifications are stored in declaration order. The caller applies
/// them in the Gateway API mandated order: **Remove -> Set -> Add**.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppliedFilter {
    /// Request headers to add (append).
    pub request_header_add: Vec<HeaderValue>,
    /// Request headers to set (overwrite).
    pub request_header_set: Vec<HeaderValue>,
    /// Request header names to remove.
    pub request_header_remove: Vec<String>,

    /// Response headers to add (append).
    pub response_header_add: Vec<HeaderValue>,
    /// Response headers to set (overwrite).
    pub response_header_set: Vec<HeaderValue>,
    /// Response header names to remove.
    pub response_header_remove: Vec<String>,

    /// URL rewrite to apply before forwarding.
    pub rewrite: Option<RewriteResult>,

    /// Mirror targets.
    pub mirrors: Vec<MirrorTarget>,

    /// Timeout configuration from the matched route rule.
    pub timeout: Option<crate::config::TimeoutConfig>,
}

/// A fully computed HTTP redirect response.
#[derive(Debug, Clone, PartialEq)]
pub struct RedirectResponse {
    /// HTTP status code for the redirect (e.g. 301, 302).
    pub status_code: u16,
    /// The value for the `Location` header.
    pub location: String,
}

/// Computed URL rewrite — new path and/or host to use.
#[derive(Debug, Clone, PartialEq)]
pub struct RewriteResult {
    /// Rewritten hostname (for the `Host` header sent to the backend).
    pub hostname: Option<String>,
    /// Rewritten path.
    pub path: Option<String>,
}

/// A mirror target with its sampling percentage.
#[derive(Debug, Clone, PartialEq)]
pub struct MirrorTarget {
    pub backend: Backend,
    /// Percentage of requests to mirror (100 if unspecified).
    pub percent: u32,
}

// ---------------------------------------------------------------------------
// collect_filters
// ---------------------------------------------------------------------------

/// Walk a slice of [`Filter`] values and accumulate their effects into a
/// single [`AppliedFilter`].
///
/// `RequestRedirect` filters are **not** collected here — redirects short-circuit
/// request processing and should be handled separately via [`compute_redirect`].
pub fn collect_filters(filters: &[Filter]) -> AppliedFilter {
    let mut applied = AppliedFilter::default();

    for filter in filters {
        match filter {
            Filter::RequestHeaderModifier { add, set, remove } => {
                applied.request_header_add.extend(add.iter().cloned());
                applied.request_header_set.extend(set.iter().cloned());
                applied.request_header_remove.extend(remove.iter().cloned());
            }
            Filter::ResponseHeaderModifier { add, set, remove } => {
                applied.response_header_add.extend(add.iter().cloned());
                applied.response_header_set.extend(set.iter().cloned());
                applied
                    .response_header_remove
                    .extend(remove.iter().cloned());
            }
            Filter::URLRewrite { .. } => {
                applied.rewrite = Some(compute_rewrite(filter, "", None));
            }
            Filter::RequestMirror { backend, percent } => {
                applied.mirrors.push(MirrorTarget {
                    backend: backend.clone(),
                    percent: percent.unwrap_or(100),
                });
            }
            // Redirects are handled separately.
            Filter::RequestRedirect { .. } => {}
        }
    }

    applied
}

// ---------------------------------------------------------------------------
// compute_redirect
// ---------------------------------------------------------------------------

/// Compute a [`RedirectResponse`] from a `RequestRedirect` filter.
///
/// # Arguments
///
/// * `filter` — Must be a [`Filter::RequestRedirect`] variant.
/// * `original_uri` — The full original request URI (e.g. `/old/path?q=1`).
/// * `original_host` — The original `Host` header value.
/// * `matched_path_prefix` — If the route was matched via `PathPrefix`, the
///   matched prefix (used by `ReplacePrefixMatch`).
///
/// # Panics
///
/// Panics if `filter` is not a `RequestRedirect`.
pub fn compute_redirect(
    filter: &Filter,
    original_uri: &str,
    original_host: &str,
    matched_path_prefix: Option<&str>,
) -> RedirectResponse {
    let Filter::RequestRedirect {
        scheme,
        hostname,
        port,
        path,
        status_code,
    } = filter
    else {
        panic!("compute_redirect called with non-RequestRedirect filter");
    };

    let effective_scheme = scheme.as_deref().unwrap_or("http");
    let effective_host = hostname.as_deref().unwrap_or(original_host);
    let status = status_code.unwrap_or(302);

    // Split original URI into path and query.
    let (original_path, query) = split_uri(original_uri);

    // Compute the final path.
    let effective_path = match path {
        Some(PathModifier::ReplaceFullPath { value }) => value.clone(),
        Some(PathModifier::ReplacePrefixMatch { value }) => {
            replace_prefix(original_path, matched_path_prefix.unwrap_or("/"), value)
        }
        None => original_path.to_string(),
    };

    // Build the Location URL.
    let port_str = match port {
        Some(p) => {
            if is_standard_port(effective_scheme, *p) {
                String::new()
            } else {
                format!(":{p}")
            }
        }
        None => String::new(),
    };

    let query_str = if query.is_empty() {
        String::new()
    } else {
        format!("?{query}")
    };

    let location =
        format!("{effective_scheme}://{effective_host}{port_str}{effective_path}{query_str}");

    RedirectResponse {
        status_code: status,
        location,
    }
}

// ---------------------------------------------------------------------------
// compute_rewrite
// ---------------------------------------------------------------------------

/// Compute a [`RewriteResult`] from a `URLRewrite` filter.
///
/// # Arguments
///
/// * `filter` — Must be a [`Filter::URLRewrite`] variant.
/// * `original_path` — The original request path.
/// * `matched_path_prefix` — If the route was matched via `PathPrefix`, the
///   matched prefix (used by `ReplacePrefixMatch`).
///
/// # Panics
///
/// Panics if `filter` is not a `URLRewrite`.
pub fn compute_rewrite(
    filter: &Filter,
    original_path: &str,
    matched_path_prefix: Option<&str>,
) -> RewriteResult {
    let Filter::URLRewrite { hostname, path } = filter else {
        panic!("compute_rewrite called with non-URLRewrite filter");
    };

    let rewritten_path = match path {
        Some(PathModifier::ReplaceFullPath { value }) => Some(value.clone()),
        Some(PathModifier::ReplacePrefixMatch { value }) => Some(replace_prefix(
            original_path,
            matched_path_prefix.unwrap_or("/"),
            value,
        )),
        None => None,
    };

    RewriteResult {
        hostname: hostname.clone(),
        path: rewritten_path,
    }
}

// ---------------------------------------------------------------------------
// Header application
// ---------------------------------------------------------------------------

/// Apply request header modifications from an [`AppliedFilter`] to a mutable
/// header list.
///
/// Processing order per Gateway API spec: **Remove -> Set -> Add**.
pub fn apply_request_headers(headers: &mut Vec<(String, String)>, applied: &AppliedFilter) {
    apply_header_modifications(
        headers,
        &applied.request_header_remove,
        &applied.request_header_set,
        &applied.request_header_add,
    );
}

/// Apply response header modifications from an [`AppliedFilter`] to a mutable
/// header list.
///
/// Processing order per Gateway API spec: **Remove -> Set -> Add**.
pub fn apply_response_headers(headers: &mut Vec<(String, String)>, applied: &AppliedFilter) {
    apply_header_modifications(
        headers,
        &applied.response_header_remove,
        &applied.response_header_set,
        &applied.response_header_add,
    );
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Apply header modifications in the Gateway API mandated order:
/// Remove -> Set -> Add.
fn apply_header_modifications(
    headers: &mut Vec<(String, String)>,
    remove: &[String],
    set: &[HeaderValue],
    add: &[HeaderValue],
) {
    // 1. Remove — case-insensitive match on header name.
    for name in remove {
        headers.retain(|(h, _)| !h.eq_ignore_ascii_case(name));
    }

    // 2. Set — overwrite existing or insert if absent.
    for hv in set {
        let mut found = false;
        for (h, v) in headers.iter_mut() {
            if h.eq_ignore_ascii_case(&hv.name) {
                *v = hv.value.clone();
                found = true;
            }
        }
        if !found {
            headers.push((hv.name.clone(), hv.value.clone()));
        }
    }

    // 3. Add — always append.
    for hv in add {
        headers.push((hv.name.clone(), hv.value.clone()));
    }
}

/// Split a URI into (path, query). The query part does **not** include the
/// leading `?`.
fn split_uri(uri: &str) -> (&str, &str) {
    match uri.find('?') {
        Some(idx) => (&uri[..idx], &uri[idx + 1..]),
        None => (uri, ""),
    }
}

/// Returns `true` if the port is the standard port for the scheme (80 for
/// http, 443 for https).
fn is_standard_port(scheme: &str, port: u16) -> bool {
    matches!((scheme, port), ("http", 80) | ("https", 443))
}

/// Replace a matched path prefix with a new prefix, preserving the remainder.
///
/// Examples:
///   replace_prefix("/old/foo/bar", "/old", "/new") => "/new/foo/bar"
///   replace_prefix("/old",         "/old", "/new") => "/new"
///   replace_prefix("/old/",        "/old", "/")    => "/"
///   replace_prefix("/old/x",       "/old", "/")    => "/x"
fn replace_prefix(original: &str, matched_prefix: &str, replacement: &str) -> String {
    let remainder = &original[matched_prefix.len()..];

    // Ensure we don't produce doubled or missing slashes at the join point.
    let replacement_has_trailing_slash = replacement.ends_with('/');
    let remainder_has_leading_slash = remainder.starts_with('/');

    match (replacement_has_trailing_slash, remainder_has_leading_slash) {
        (true, true) => {
            // "/new/" + "/foo" => "/new/foo"
            format!("{}{}", replacement, &remainder[1..])
        }
        (false, false) if !remainder.is_empty() => {
            // "/new" + "foo" => "/new/foo"
            format!("{replacement}/{remainder}")
        }
        _ => {
            // "/new" + "/foo" or "/new/" + "foo" => concatenate directly
            format!("{replacement}{remainder}")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Backend, Filter, HeaderValue, PathModifier};

    // -- Header modification tests -------------------------------------------

    #[test]
    fn remove_headers_by_name_case_insensitive() {
        let applied = AppliedFilter {
            request_header_remove: vec!["X-Remove-Me".to_string()],
            ..Default::default()
        };
        let mut headers = vec![
            ("x-remove-me".to_string(), "value1".to_string()),
            ("X-Remove-Me".to_string(), "value2".to_string()),
            ("X-Keep".to_string(), "keep".to_string()),
        ];

        apply_request_headers(&mut headers, &applied);

        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "X-Keep");
    }

    #[test]
    fn set_headers_overwrites_existing() {
        let applied = AppliedFilter {
            request_header_set: vec![HeaderValue {
                name: "X-Existing".to_string(),
                value: "new-value".to_string(),
            }],
            ..Default::default()
        };
        let mut headers = vec![
            ("x-existing".to_string(), "old-value".to_string()),
            ("Other".to_string(), "untouched".to_string()),
        ];

        apply_request_headers(&mut headers, &applied);

        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].1, "new-value");
        assert_eq!(headers[1].1, "untouched");
    }

    #[test]
    fn set_headers_inserts_when_absent() {
        let applied = AppliedFilter {
            request_header_set: vec![HeaderValue {
                name: "X-New".to_string(),
                value: "brand-new".to_string(),
            }],
            ..Default::default()
        };
        let mut headers = vec![("Other".to_string(), "val".to_string())];

        apply_request_headers(&mut headers, &applied);

        assert_eq!(headers.len(), 2);
        assert_eq!(headers[1].0, "X-New");
        assert_eq!(headers[1].1, "brand-new");
    }

    #[test]
    fn add_headers_appends_without_overwriting() {
        let applied = AppliedFilter {
            request_header_add: vec![HeaderValue {
                name: "X-Existing".to_string(),
                value: "second".to_string(),
            }],
            ..Default::default()
        };
        let mut headers = vec![("X-Existing".to_string(), "first".to_string())];

        apply_request_headers(&mut headers, &applied);

        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].1, "first");
        assert_eq!(headers[1].1, "second");
    }

    #[test]
    fn combined_remove_set_add_in_correct_order() {
        // Start with headers: A=old, B=remove-me, C=keep
        // Remove: B
        // Set: A=overwritten
        // Add: D=added
        let applied = AppliedFilter {
            request_header_remove: vec!["B".to_string()],
            request_header_set: vec![HeaderValue {
                name: "A".to_string(),
                value: "overwritten".to_string(),
            }],
            request_header_add: vec![HeaderValue {
                name: "D".to_string(),
                value: "added".to_string(),
            }],
            ..Default::default()
        };
        let mut headers = vec![
            ("A".to_string(), "old".to_string()),
            ("B".to_string(), "remove-me".to_string()),
            ("C".to_string(), "keep".to_string()),
        ];

        apply_request_headers(&mut headers, &applied);

        // B removed, A overwritten, D added
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0], ("A".to_string(), "overwritten".to_string()));
        assert_eq!(headers[1], ("C".to_string(), "keep".to_string()));
        assert_eq!(headers[2], ("D".to_string(), "added".to_string()));
    }

    #[test]
    fn response_header_modifications() {
        let applied = AppliedFilter {
            response_header_remove: vec!["Server".to_string()],
            response_header_set: vec![HeaderValue {
                name: "Cache-Control".to_string(),
                value: "no-store".to_string(),
            }],
            response_header_add: vec![HeaderValue {
                name: "X-Custom".to_string(),
                value: "yes".to_string(),
            }],
            ..Default::default()
        };
        let mut headers = vec![
            ("Server".to_string(), "nginx".to_string()),
            ("Cache-Control".to_string(), "public".to_string()),
        ];

        apply_response_headers(&mut headers, &applied);

        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers[0],
            ("Cache-Control".to_string(), "no-store".to_string())
        );
        assert_eq!(headers[1], ("X-Custom".to_string(), "yes".to_string()));
    }

    // -- Redirect tests ------------------------------------------------------

    #[test]
    fn hostname_redirect_default_302() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: Some("new.example.com".to_string()),
            port: None,
            path: None,
            status_code: None,
        };

        let result = compute_redirect(&filter, "/path", "old.example.com", None);

        assert_eq!(result.status_code, 302);
        assert_eq!(result.location, "http://new.example.com/path");
    }

    #[test]
    fn hostname_redirect_custom_status_301() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: Some("new.example.com".to_string()),
            port: None,
            path: None,
            status_code: Some(301),
        };

        let result = compute_redirect(&filter, "/path", "old.example.com", None);

        assert_eq!(result.status_code, 301);
        assert_eq!(result.location, "http://new.example.com/path");
    }

    #[test]
    fn scheme_redirect_http_to_https() {
        let filter = Filter::RequestRedirect {
            scheme: Some("https".to_string()),
            hostname: None,
            port: None,
            path: None,
            status_code: None,
        };

        let result = compute_redirect(&filter, "/secure", "example.com", None);

        assert_eq!(result.status_code, 302);
        assert_eq!(result.location, "https://example.com/secure");
    }

    #[test]
    fn port_redirect_standard_port_omitted() {
        let filter = Filter::RequestRedirect {
            scheme: Some("https".to_string()),
            hostname: None,
            port: Some(443),
            path: None,
            status_code: None,
        };

        let result = compute_redirect(&filter, "/path", "example.com", None);

        // Standard port 443 for https should be omitted.
        assert_eq!(result.location, "https://example.com/path");
    }

    #[test]
    fn port_redirect_nonstandard_port_included() {
        let filter = Filter::RequestRedirect {
            scheme: Some("https".to_string()),
            hostname: None,
            port: Some(8443),
            path: None,
            status_code: None,
        };

        let result = compute_redirect(&filter, "/path", "example.com", None);

        assert_eq!(result.location, "https://example.com:8443/path");
    }

    #[test]
    fn port_redirect_http_standard_port_omitted() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: None,
            port: Some(80),
            path: None,
            status_code: None,
        };

        let result = compute_redirect(&filter, "/path", "example.com", None);

        assert_eq!(result.location, "http://example.com/path");
    }

    #[test]
    fn full_path_replacement_redirect() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: None,
            port: None,
            path: Some(PathModifier::ReplaceFullPath {
                value: "/new-path".to_string(),
            }),
            status_code: None,
        };

        let result = compute_redirect(&filter, "/old-path", "example.com", None);

        assert_eq!(result.location, "http://example.com/new-path");
    }

    #[test]
    fn prefix_path_replacement_redirect() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: None,
            port: None,
            path: Some(PathModifier::ReplacePrefixMatch {
                value: "/new".to_string(),
            }),
            status_code: None,
        };

        let result = compute_redirect(&filter, "/old/foo/bar", "example.com", Some("/old"));

        assert_eq!(result.location, "http://example.com/new/foo/bar");
    }

    #[test]
    fn query_string_preservation() {
        let filter = Filter::RequestRedirect {
            scheme: Some("https".to_string()),
            hostname: Some("new.example.com".to_string()),
            port: None,
            path: None,
            status_code: None,
        };

        let result = compute_redirect(&filter, "/path?key=value&other=1", "old.example.com", None);

        assert_eq!(
            result.location,
            "https://new.example.com/path?key=value&other=1"
        );
    }

    #[test]
    fn combined_scheme_hostname_port_path_redirect() {
        let filter = Filter::RequestRedirect {
            scheme: Some("https".to_string()),
            hostname: Some("new.example.com".to_string()),
            port: Some(8443),
            path: Some(PathModifier::ReplaceFullPath {
                value: "/landing".to_string(),
            }),
            status_code: Some(307),
        };

        let result = compute_redirect(&filter, "/old-path?q=search", "old.example.com", None);

        assert_eq!(result.status_code, 307);
        assert_eq!(
            result.location,
            "https://new.example.com:8443/landing?q=search"
        );
    }

    // -- Status code tests ---------------------------------------------------

    #[test]
    fn redirect_status_code_301() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: Some("x.com".to_string()),
            port: None,
            path: None,
            status_code: Some(301),
        };
        assert_eq!(
            compute_redirect(&filter, "/", "a.com", None).status_code,
            301
        );
    }

    #[test]
    fn redirect_status_code_302() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: Some("x.com".to_string()),
            port: None,
            path: None,
            status_code: Some(302),
        };
        assert_eq!(
            compute_redirect(&filter, "/", "a.com", None).status_code,
            302
        );
    }

    #[test]
    fn redirect_status_code_303() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: Some("x.com".to_string()),
            port: None,
            path: None,
            status_code: Some(303),
        };
        assert_eq!(
            compute_redirect(&filter, "/", "a.com", None).status_code,
            303
        );
    }

    #[test]
    fn redirect_status_code_307() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: Some("x.com".to_string()),
            port: None,
            path: None,
            status_code: Some(307),
        };
        assert_eq!(
            compute_redirect(&filter, "/", "a.com", None).status_code,
            307
        );
    }

    #[test]
    fn redirect_status_code_308() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: Some("x.com".to_string()),
            port: None,
            path: None,
            status_code: Some(308),
        };
        assert_eq!(
            compute_redirect(&filter, "/", "a.com", None).status_code,
            308
        );
    }

    #[test]
    fn redirect_default_status_is_302() {
        let filter = Filter::RequestRedirect {
            scheme: None,
            hostname: Some("x.com".to_string()),
            port: None,
            path: None,
            status_code: None,
        };
        assert_eq!(
            compute_redirect(&filter, "/", "a.com", None).status_code,
            302
        );
    }

    // -- URL rewrite tests ---------------------------------------------------

    #[test]
    fn rewrite_hostname() {
        let filter = Filter::URLRewrite {
            hostname: Some("internal.example.com".to_string()),
            path: None,
        };

        let result = compute_rewrite(&filter, "/path", None);

        assert_eq!(result.hostname, Some("internal.example.com".to_string()));
        assert_eq!(result.path, None);
    }

    #[test]
    fn rewrite_full_path_replacement() {
        let filter = Filter::URLRewrite {
            hostname: None,
            path: Some(PathModifier::ReplaceFullPath {
                value: "/brand-new".to_string(),
            }),
        };

        let result = compute_rewrite(&filter, "/old-path", None);

        assert_eq!(result.hostname, None);
        assert_eq!(result.path, Some("/brand-new".to_string()));
    }

    #[test]
    fn rewrite_prefix_replacement() {
        let filter = Filter::URLRewrite {
            hostname: None,
            path: Some(PathModifier::ReplacePrefixMatch {
                value: "/new".to_string(),
            }),
        };

        let result = compute_rewrite(&filter, "/old/foo/bar", Some("/old"));

        assert_eq!(result.path, Some("/new/foo/bar".to_string()));
    }

    #[test]
    fn rewrite_prefix_replacement_exact_match() {
        let filter = Filter::URLRewrite {
            hostname: None,
            path: Some(PathModifier::ReplacePrefixMatch {
                value: "/new".to_string(),
            }),
        };

        let result = compute_rewrite(&filter, "/old", Some("/old"));

        assert_eq!(result.path, Some("/new".to_string()));
    }

    #[test]
    fn rewrite_prefix_to_root() {
        let filter = Filter::URLRewrite {
            hostname: None,
            path: Some(PathModifier::ReplacePrefixMatch {
                value: "/".to_string(),
            }),
        };

        let result = compute_rewrite(&filter, "/old/x", Some("/old"));

        assert_eq!(result.path, Some("/x".to_string()));
    }

    // -- collect_filters tests -----------------------------------------------

    #[test]
    fn collect_filters_accumulates_header_modifiers() {
        let filters = vec![
            Filter::RequestHeaderModifier {
                add: vec![HeaderValue {
                    name: "X-A".to_string(),
                    value: "a".to_string(),
                }],
                set: vec![],
                remove: vec!["X-Remove".to_string()],
            },
            Filter::ResponseHeaderModifier {
                add: vec![],
                set: vec![HeaderValue {
                    name: "Server".to_string(),
                    value: "multiway".to_string(),
                }],
                remove: vec![],
            },
        ];

        let applied = collect_filters(&filters);

        assert_eq!(applied.request_header_add.len(), 1);
        assert_eq!(applied.request_header_remove.len(), 1);
        assert_eq!(applied.response_header_set.len(), 1);
    }

    #[test]
    fn collect_filters_captures_mirrors() {
        let filters = vec![Filter::RequestMirror {
            backend: Backend::new("default", "mirror-svc", 9090),
            percent: Some(50),
        }];

        let applied = collect_filters(&filters);

        assert_eq!(applied.mirrors.len(), 1);
        assert_eq!(applied.mirrors[0].percent, 50);
        assert_eq!(applied.mirrors[0].backend.name, "mirror-svc");
    }

    #[test]
    fn collect_filters_mirror_default_percent_is_100() {
        let filters = vec![Filter::RequestMirror {
            backend: Backend::new("default", "mirror-svc", 9090),
            percent: None,
        }];

        let applied = collect_filters(&filters);

        assert_eq!(applied.mirrors[0].percent, 100);
    }

    #[test]
    fn collect_filters_captures_rewrite() {
        let filters = vec![Filter::URLRewrite {
            hostname: Some("internal.example.com".to_string()),
            path: Some(PathModifier::ReplaceFullPath {
                value: "/api".to_string(),
            }),
        }];

        let applied = collect_filters(&filters);

        let rewrite = applied.rewrite.unwrap();
        assert_eq!(rewrite.hostname, Some("internal.example.com".to_string()));
        assert_eq!(rewrite.path, Some("/api".to_string()));
    }

    #[test]
    fn collect_filters_ignores_redirects() {
        let filters = vec![Filter::RequestRedirect {
            scheme: Some("https".to_string()),
            hostname: None,
            port: None,
            path: None,
            status_code: Some(301),
        }];

        let applied = collect_filters(&filters);

        // Redirects should not appear in the collected filters.
        assert!(applied.request_header_add.is_empty());
        assert!(applied.mirrors.is_empty());
        assert!(applied.rewrite.is_none());
    }

    // -- Internal helper tests -----------------------------------------------

    #[test]
    fn split_uri_with_query() {
        assert_eq!(split_uri("/path?q=1"), ("/path", "q=1"));
    }

    #[test]
    fn split_uri_without_query() {
        assert_eq!(split_uri("/path"), ("/path", ""));
    }

    #[test]
    fn replace_prefix_basic() {
        assert_eq!(
            replace_prefix("/old/foo/bar", "/old", "/new"),
            "/new/foo/bar"
        );
    }

    #[test]
    fn replace_prefix_exact() {
        assert_eq!(replace_prefix("/old", "/old", "/new"), "/new");
    }

    #[test]
    fn replace_prefix_to_root_removes_prefix() {
        assert_eq!(replace_prefix("/old/x", "/old", "/"), "/x");
    }

    #[test]
    fn replace_prefix_trailing_slash_no_double() {
        assert_eq!(replace_prefix("/old/x", "/old", "/new/"), "/new/x");
    }
}
