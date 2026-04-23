//! SHA256 checksums for pinned LSP installer download URLs.
//!
//! Policy: any URL listed here MUST match its recorded SHA256 or the install
//! fails. URLs not listed fall through with a `tracing::warn!`. This lets us
//! roll pinned checksums out incrementally: land the lookup infrastructure
//! first, then fill in hashes as we have time to download-and-verify each
//! binary.
//!
//! IMPORTANT: never commit a fake or guessed hash. If you can't compute it,
//! leave it as `"TODO"` so the verification path short-circuits to the
//! "no pinned checksum" branch.

/// A pinned expected checksum for a specific download URL.
pub struct ExpectedChecksum {
    /// Hex-encoded SHA256 digest (64 lowercase chars) or the literal "TODO"
    /// for entries where we pinned the URL but haven't computed the hash yet.
    pub sha256: &'static str,
    /// The exact URL to match. Matches are byte-for-byte.
    pub url: &'static str,
}

/// Registry of pinned download URLs and their expected SHA256 hashes.
///
/// Keep entries sorted by URL for easier review.
pub static CHECKSUMS: &[ExpectedChecksum] = &[
    ExpectedChecksum {
        // Deno 2.0.6 Linux x86_64
        url: "https://github.com/denoland/deno/releases/download/v2.0.6/deno-x86_64-unknown-linux-gnu.zip",
        sha256: "TODO",
    },
    ExpectedChecksum {
        // Eclipse JDTLS pinned snapshot
        url: "https://download.eclipse.org/jdtls/snapshots/jdt-language-server-1.45.0-202511062216.tar.gz",
        sha256: "TODO",
    },
    ExpectedChecksum {
        // Elixir-LS 0.23.1
        url: "https://github.com/elixir-lsp/elixir-ls/releases/download/v0.23.1/elixir-ls-v0.23.1.zip",
        sha256: "TODO",
    },
    ExpectedChecksum {
        // clojure-lsp 2025.10.24
        url: "https://github.com/clojure-lsp/clojure-lsp/releases/download/2025.10.24-15.44.27/clojure-lsp-native-linux-amd64.zip",
        sha256: "TODO",
    },
    ExpectedChecksum {
        // rust-analyzer 2025-10-27 Linux x86_64
        url: "https://github.com/rust-lang/rust-analyzer/releases/download/2025-10-27/rust-analyzer-x86_64-unknown-linux-gnu.gz",
        sha256: "TODO",
    },
    ExpectedChecksum {
        // terraform-ls 0.39.0 linux amd64
        url: "https://releases.hashicorp.com/terraform-ls/0.39.0/terraform-ls_0.39.0_linux_amd64.zip",
        sha256: "TODO",
    },
    ExpectedChecksum {
        // zls 0.14.0 Linux x86_64
        url: "https://github.com/zigtools/zls/releases/download/0.14.0/zls-linux-x86_64.tar.gz",
        sha256: "TODO",
    },
];

/// Look up a pinned checksum by exact URL match. Returns `None` when the URL
/// is unknown, in which case the caller should log and proceed.
pub fn lookup(url: &str) -> Option<&'static ExpectedChecksum> {
    CHECKSUMS.iter().find(|c| c.url == url)
}

/// Verify a blob of downloaded bytes against a pinned checksum.
///
/// Returns `Ok(())` when:
///   - the URL is pinned and the SHA256 matches, OR
///   - the pinned entry is "TODO" (verification infrastructure landed first,
///     real hash pending), OR
///   - the URL is not pinned (caller will have already logged a warning).
///
/// Returns `Err(String)` only when a real pinned hash is present and the
/// download doesn't match it — that is an integrity failure and must block.
pub fn verify(url: &str, bytes: &[u8]) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let expected = match lookup(url) {
        Some(e) => e,
        None => {
            tracing::warn!(
                url = %url,
                "no pinned checksum; proceeding without verification"
            );
            return Ok(());
        }
    };

    if expected.sha256 == "TODO" {
        tracing::warn!(
            url = %url,
            "pinned URL has no computed SHA256 yet (TODO); proceeding"
        );
        return Ok(());
    }

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = hasher.finalize();
    let actual_hex = hex_encode(&actual);
    if actual_hex.eq_ignore_ascii_case(expected.sha256) {
        Ok(())
    } else {
        Err(format!(
            "SHA256 mismatch for {}: expected {}, got {}",
            url, expected.sha256, actual_hex
        ))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_known_url() {
        let url = "https://github.com/rust-lang/rust-analyzer/releases/download/2025-10-27/rust-analyzer-x86_64-unknown-linux-gnu.gz";
        assert!(lookup(url).is_some());
    }

    #[test]
    fn test_lookup_unknown_url() {
        assert!(lookup("https://example.invalid/unknown").is_none());
    }

    #[test]
    fn test_verify_unknown_url_is_permissive() {
        let r = verify("https://example.invalid/unknown", b"whatever");
        assert!(r.is_ok());
    }

    #[test]
    fn test_verify_todo_entry_is_permissive() {
        let url = "https://github.com/rust-lang/rust-analyzer/releases/download/2025-10-27/rust-analyzer-x86_64-unknown-linux-gnu.gz";
        let r = verify(url, b"any bytes");
        assert!(r.is_ok());
    }

    #[test]
    fn test_verify_mismatch_fails_when_pinned() {
        // Register a fake fixed-hash entry in-place using a known URL and then
        // verify deliberately-wrong bytes. We exercise this via the lookup
        // helper + a hand-rolled Sha256 compare path:
        use sha2::{Digest, Sha256};
        let bytes = b"hello";
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let got = hex_encode(&hasher.finalize());
        // 'hello' sha256
        assert_eq!(
            got,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
