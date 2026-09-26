//! Credential redaction for rendered diagnostics.
//!
//! A resolved endpoint may carry a provider credential in its userinfo
//! (`https://user:secret@host/...`) or in a query parameter
//! (`...?api_key=...`). Those values must never reach a rendered line: a
//! diagnostic ends up in a scrollback, an issue, or a CI log. The file keeps
//! what the operator wrote; only the RENDER path redacts.

/// The credential-bearing query-parameter names this helper redacts.
///
/// A denylist, not an allowlist: a provider may name its credential anything,
/// so there is no closed set to allow. These are the names the RPC providers
/// the bot dials use (`api_key`/`apikey` for Alchemy-style URLs, `key` for
/// Infura-style URLs, `token`/`access_token` for authenticated gateways, plus
/// the generic `secret`/`password`). The comparison is case-insensitive. A
/// provider that invents a new name is a deliberate false negative: guessing
/// by value shape would strip real configuration and hide data.
pub const CREDENTIAL_QUERY_PARAMS: &[&str] = &[
    "api_key",
    "apikey",
    "key",
    "token",
    "secret",
    "password",
    "access_token",
];

/// The text substituted for a redacted query value.
pub const REDACTED: &str = "REDACTED";

/// Redact credentials from a rendered endpoint, leaving everything else
/// (host, path, non-credential query values, fragment) intact.
///
/// Userinfo (`user:secret@`) is removed wholesale; a credential-bearing
/// query parameter keeps its name but loses its value (the operator can see
/// WHICH parameter was there without seeing the secret).
///
/// A value with no `://` is still parsed for a query and a fragment, so a
/// forgot-the-scheme endpoint like `rpc.example.com?api_key=abc` loses its
/// credential. Its userinfo is stripped only when the text before the last
/// `@` contains a `:` — that colon is what distinguishes `user:secret@host`
/// from an `@` inside a path segment, so `/tmp/an@vil.ipc` is unchanged.
#[must_use]
pub fn redact_uri(uri: &str) -> String {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return redact_non_hierarchical(uri);
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);

    let (path, fragment) = match tail.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (tail, None),
    };
    let (path, query) = match path.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path, None),
    };

    let mut redacted = String::with_capacity(uri.len());
    redacted.push_str(scheme);
    redacted.push_str("://");
    redacted.push_str(host);
    redacted.push_str(path);
    if let Some(query) = query {
        redacted.push('?');
        redacted.push_str(&redact_query(query));
    }
    if let Some(fragment) = fragment {
        redacted.push('#');
        redacted.push_str(fragment);
    }
    redacted
}

/// Redact a value that carries no scheme: split off any fragment and query
/// as the hierarchical branch does, and strip userinfo only when the colon
/// before the last `@` marks it as a credential rather than a path segment.
fn redact_non_hierarchical(value: &str) -> String {
    let (before_fragment, fragment) = match value.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (value, None),
    };
    let (path, query) = match before_fragment.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (before_fragment, None),
    };
    let path = match path.rsplit_once('@') {
        Some((prefix, host)) if prefix.contains(':') => host,
        _ => path,
    };

    let mut redacted = String::with_capacity(value.len());
    redacted.push_str(path);
    if let Some(query) = query {
        redacted.push('?');
        redacted.push_str(&redact_query(query));
    }
    if let Some(fragment) = fragment {
        redacted.push('#');
        redacted.push_str(fragment);
    }
    redacted
}

/// Redact the credential-bearing parameters of a query string, preserving
/// parameter order, names, and non-credential values.
fn redact_query(query: &str) -> String {
    query
        .split('&')
        .map(|parameter| {
            let Some((name, _value)) = parameter.split_once('=') else {
                return parameter.to_string();
            };
            if CREDENTIAL_QUERY_PARAMS
                .iter()
                .any(|credential| credential.eq_ignore_ascii_case(name.trim()))
            {
                format!("{name}={REDACTED}")
            } else {
                parameter.to_string()
            }
        })
        .collect::<Vec<String>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::redact_uri;

    #[test]
    fn strips_userinfo() {
        assert_eq!(
            redact_uri("https://user:secret@host.example/x"),
            "https://host.example/x"
        );
    }

    #[test]
    fn redacts_credential_query_parameters() {
        assert_eq!(
            redact_uri("https://host.example/x?api_key=abc&chain=1&token=deadbeef"),
            "https://host.example/x?api_key=REDACTED&chain=1&token=REDACTED"
        );
    }

    #[test]
    fn credentials_never_survive_as_a_diagnostic_value() {
        let uri = "https://user:secret@host.example/rpc?api_key=abc&x=1";
        let redacted = redact_uri(uri);
        assert!(!redacted.contains("secret"), "{redacted}");
        assert!(!redacted.contains("abc"), "{redacted}");
        assert!(redacted.contains("host.example"), "{redacted}");
        assert!(redacted.contains("x=1"), "{redacted}");
    }

    #[test]
    fn a_non_hierarchical_value_is_unchanged() {
        assert_eq!(redact_uri("/tmp/anvil.ipc"), "/tmp/anvil.ipc");
        assert_eq!(redact_uri("ipc:///tmp/anvil.ipc"), "ipc:///tmp/anvil.ipc");
    }

    #[test]
    fn a_schemeless_value_redacts_its_credential_query() {
        assert_eq!(
            redact_uri("rpc.example.com?api_key=abc"),
            "rpc.example.com?api_key=REDACTED"
        );
    }

    #[test]
    fn a_schemeless_value_strips_forgot_scheme_userinfo() {
        assert_eq!(
            redact_uri("user:secret@host.example/rpc"),
            "host.example/rpc"
        );
    }

    #[test]
    fn a_schemeless_socket_path_keeps_an_at_that_is_not_userinfo() {
        assert_eq!(redact_uri("/tmp/an@vil.ipc"), "/tmp/an@vil.ipc");
        assert_eq!(redact_uri("/tmp/anvil.ipc"), "/tmp/anvil.ipc");
    }

    #[test]
    fn a_clean_uri_is_unchanged() {
        assert_eq!(redact_uri("ws://127.0.0.1:8546"), "ws://127.0.0.1:8546");
    }

    #[test]
    fn query_names_match_case_insensitively_and_fragments_survive() {
        assert_eq!(
            redact_uri("wss://host/feed?API_KEY=abc#frag"),
            "wss://host/feed?API_KEY=REDACTED#frag"
        );
    }
}
