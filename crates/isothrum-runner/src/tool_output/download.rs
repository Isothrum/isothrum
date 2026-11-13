//! What a failed download says, read: a registry's or a box catalog's answer as Docker, Compose,
//! Vagrant and curl print it, for the retry of a download that failed for the network
//! (`executor::retry`). Its fixtures are in `fixtures/download/`.

/// Why a download failed, by what its tool printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure<'a> {
    /// A network error that passes (its line, as printed): a new try most likely succeeds.
    Passing(&'a str),
    /// Anything else: a refusal ([`REFUSED`]) a new try wouldn't change, or a failure not known
    /// to pass.
    Other,
}

impl<'a> Failure<'a> {
    /// What `reason`, a failed download's ([`Captured::reason`](crate::process::Captured::reason)),
    /// says.
    pub(crate) fn of(reason: &'a str) -> Failure<'a> {
        transient(reason).map_or(Failure::Other, Failure::Passing)
    }
}

/// What a failed download prints for a network error that passes: a connection cut or refused, a
/// name not resolved for now, a gateway in front of a registry failing (lower case, as compared).
const PASSING: [&str; 14] = [
    "connection reset by peer",
    "i/o timeout",
    "tls handshake timeout",
    "unexpected eof",
    "connection refused",
    "temporary failure in name resolution",
    "502 bad gateway",
    "503 service unavailable",
    "504 gateway timeout",
    "504 gateway time-out",
    // curl's, in a Vagrant box download: a partial file, a timeout, a TLS connection failing, a
    // connection reset as data came.
    "curl:(18)",
    "curl:(28)",
    "curl:(35)",
    "curl:(56)",
];

/// What a failed download prints for a refusal that a new try wouldn't change (lower case): a
/// registry's or a catalog's sign-in or rate limit, a name or a version it doesn't have. Any of
/// them in its failure, and it isn't tried again, a network error alongside it or not.
const REFUSED: [&str; 7] = [
    "toomanyrequests",
    "too many requests",
    "denied",
    "unauthorized",
    "authentication required",
    "not found",
    "manifest unknown",
];

/// The line of `reason` (a failed download's, [`Captured::reason`](crate::process::Captured::reason))
/// that shows a network error that passes, when it shows one and no refusal ([`REFUSED`]).
fn transient(reason: &str) -> Option<&str> {
    let said = reason.to_lowercase();
    if REFUSED.iter().any(|r| said.contains(r)) {
        return None;
    }
    reason.lines().map(str::trim).find(|line| passes(&line.to_lowercase()))
}

/// Whether `line`, in lower case, says a network error that passes: one of [`PASSING`], or a
/// connection that ended in the middle of the download (`…: EOF`).
fn passes(line: &str) -> bool {
    // curl writes `curl:(28)` in Vagrant's message; `curl: (28)` on its own.
    let line = line.replace("curl: (", "curl:(");
    PASSING.iter().any(|p| line.contains(p)) || ends_eof(&line)
}

/// Whether `line`, in lower case, has a `: eof` that no word follows (`read: eof`, then the end of
/// the line or of its sentence).
fn ends_eof(line: &str) -> bool {
    line.match_indices(": eof").any(|(at, found)| {
        line.get(at + found.len()..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|c| !c.is_alphanumeric())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_error_that_passes_is_found_in_the_downloads_failure() {
        // Regression: a reset in the middle of a 1.93 GB layer failed the lab's start.
        let reset = "36b0bafd7f97 Downloading 1.932GB\nfailed to copy: read tcp 10.0.2.15:42512->13.249.228.76:443: read: connection reset by peer (exited with code 1)";
        assert_eq!(
            transient(reset),
            Some("failed to copy: read tcp 10.0.2.15:42512->13.249.228.76:443: read: connection reset by peer (exited with code 1)")
        );
        for passing in [
            "Error response from daemon: Get \"https://registry-1.docker.io/v2/\": net/http: TLS handshake timeout",
            "dial tcp 104.18.121.25:443: i/o timeout",
            "error pulling image configuration: download failed after attempts=6: unexpected EOF",
            "failed to copy: httpReadSeeker: failed open: failed to do request: Get \"https://x\": EOF",
            "read tcp 10.0.2.15:42512->13.249.228.76:443: read: EOF (exited with code 1)",
            "dial tcp 10.0.0.1:443: connect: connection refused",
            "dial tcp: lookup registry-1.docker.io on 10.0.2.3:53: Temporary failure in name resolution",
            "received unexpected HTTP status: 502 Bad Gateway",
            "unexpected status from GET request to https://ghcr.io/v2/x/blobs/sha256:ab: 503 Service Unavailable",
            "received unexpected HTTP status: 504 Gateway Time-out",
            "curl:(18) transfer closed with 1048576 bytes remaining to read",
            "curl:(28) Operation timed out after 30001 milliseconds with 0 bytes received",
            "curl: (35) OpenSSL SSL_connect: SSL_ERROR_SYSCALL in connection to vagrantcloud.com:443",
            "curl:(56) OpenSSL SSL_read: Connection reset by peer, errno 104",
        ] {
            assert_eq!(transient(passing), Some(passing), "{passing}");
        }
    }

    #[test]
    fn a_refusal_or_an_unknown_failure_is_not_tried_again() {
        for refused in [
            "Error response from daemon: toomanyrequests: You have reached your pull rate limit",
            "Error response from daemon: pull access denied for isoloom/nope, repository does not exist or may require 'docker login'",
            "Error response from daemon: manifest for kali:nope not found: manifest unknown: manifest unknown",
            "unauthorized: authentication required",
            "The box 'nope/nope' could not be found or could not be accessed in the remote catalog.",
            "curl:(22) The requested URL returned error: 404 Not Found",
            // A refusal wins over a network error seen alongside it (another image's reset).
            "read: connection reset by peer\nError response from daemon: manifest unknown",
            "no space left on device",
            "Unexpected end of JSON input: the EOFs counted",
            "error: eofmark",
        ] {
            assert_eq!(transient(refused), None, "{refused}");
        }
    }

    #[test]
    fn fixtures_read_as_each_tool_prints_them() {
        let passing = |text: &str| matches!(Failure::of(text), Failure::Passing(_));
        assert!(passing(include_str!("fixtures/download/docker-27.3-pull-reset.txt")));
        assert!(passing(include_str!("fixtures/download/vagrant-2.4.1-box-timeout.txt")));
        assert_eq!(Failure::of(include_str!("fixtures/download/docker-27.3-pull-rate-limit.txt")), Failure::Other);
        assert_eq!(Failure::of(include_str!("fixtures/download/vagrant-2.4.1-box-missing.txt")), Failure::Other);
        assert_eq!(Failure::of(""), Failure::Other);
    }
}
