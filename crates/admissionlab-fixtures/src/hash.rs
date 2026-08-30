//! Content hashing for fixture provenance.
//!
//! [`sha256_hex`] is the one hashing primitive this crate uses. It
//! exists as its own small module (rather than being inlined at its one
//! call site in [`crate::discover`]) because the brief for this task
//! calls out hashing as its own step; there is nothing else here.
//!
//! **This hash is for content provenance and change detection, never
//! security authentication.** It lets two runs (or two machines) confirm
//! they read byte-identical fixture content, and lets a later run
//! manifest (PRODUCT.md §28) record what was actually replayed. It is
//! not a MAC, is not keyed, and gives no integrity guarantee against a
//! motivated adversary who can also rewrite the hash sitting next to it
//! — nothing here should ever be repurposed as a tamper check.

use sha2::{Digest, Sha256};

/// Returns the SHA-256 digest of `bytes`, rendered as lowercase hex.
///
/// `bytes` should be a fixture file's canonical source bytes -- exactly
/// as read from disk, never a re-serialization of a parsed document (see
/// [`crate::discover`]'s module documentation for why this crate hashes
/// whole-file bytes rather than a re-encoded form). This function itself
/// has no opinion on *which* bytes it is handed; it only hashes and
/// formats.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    /// Pins a well-known SHA-256 test vector (the empty input) rather
    /// than only asserting "some 64-character string comes back" --
    /// proves this actually computes SHA-256, not merely something
    /// digest-shaped. Mutation check: a placeholder that always returned
    /// a fixed 64-character string of zeroes would pass every other
    /// test in this module but fails this one.
    #[test]
    fn empty_input_matches_known_sha256_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// A second, independently-known vector ("abc"), so
    /// `empty_input_matches_known_sha256_vector` alone cannot be
    /// satisfied by an implementation special-cased for empty input.
    #[test]
    fn known_input_matches_known_sha256_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn output_is_lowercase_hex_of_length_64() {
        let digest = sha256_hex(b"admission-lab fixture content");
        assert_eq!(
            digest.len(),
            64,
            "digest {digest:?} is not 64 hex characters"
        );
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "digest {digest:?} contains a character outside lowercase hex"
        );
    }

    #[test]
    fn same_input_hashes_identically_every_time() {
        let bytes = b"deterministic across calls";
        assert_eq!(sha256_hex(bytes), sha256_hex(bytes));
    }

    #[test]
    fn different_input_hashes_differently() {
        assert_ne!(sha256_hex(b"fixture-a"), sha256_hex(b"fixture-b"));
    }
}
