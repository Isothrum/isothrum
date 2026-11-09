//! The variables that tell a tool how this host reaches the network: its proxies and the CA
//! bundles a corporate TLS inspection needs. A tool started as a child of the runner inherits
//! them; one that starts elsewhere (an elevated broker, a container) is handed them by name, so
//! it reaches the network the way the runner's own tools do.

/// The proxy variables (both spellings: Unix tools read either, Windows' names ignore case) and
/// the CA bundles of Go, OpenSSL, Python's requests and curl.
pub(crate) const NETWORK_VARS: [&str; 9] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
];

/// Whether `name` is one of [`NETWORK_VARS`], whatever its case (as Windows reads names).
pub(crate) fn is_network_var(name: &str) -> bool {
    NETWORK_VARS.iter().any(|v| v.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_variable_is_known_in_either_case() {
        assert!(is_network_var("HTTPS_PROXY") && is_network_var("https_proxy") && is_network_var("Https_Proxy"));
        assert!(is_network_var("SSL_CERT_FILE") && !is_network_var("PATH") && !is_network_var("AWS_SECRET_ACCESS_KEY"));
    }
}
