//! Verifier for the SSHSIG signatures `release.yml` produces with
//! `ssh-keygen -Y sign` — the anchor under [`crate::release`], where the only
//! trust the updater has in a release is a public key compiled into this
//! binary. Ed25519 only: the pipeline signs with one key of one type, and a
//! verifier that accepts more algorithms than the signer emits is surface
//! bought for nothing.
//!
//! Wire format, from OpenSSH's `PROTOCOL.sshsig`. The armored blob decodes to
//!
//! ```text
//! byte[6]  "SSHSIG"        magic preamble
//! uint32   version         1
//! string   publickey       string "ssh-ed25519" ‖ string <32-byte key>
//! string   namespace
//! string   reserved
//! string   hash_algorithm  "sha256" or "sha512"
//! string   signature       string "ssh-ed25519" ‖ string <64-byte sig>
//! ```
//!
//! and what the key actually signs is a second blob over the HASH of the
//! message, never the message itself:
//!
//! ```text
//! byte[6]  "SSHSIG"
//! string   namespace
//! string   reserved
//! string   hash_algorithm
//! string   H(message)
//! ```
//!
//! `string` is a big-endian u32 length followed by that many bytes.

use crate::error::{Error, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256, Sha512};

const MAGIC: &[u8] = b"SSHSIG";
const VERSION: u32 = 1;
const KEY_TYPE: &str = "ssh-ed25519";
const ARMOR_BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
const ARMOR_END: &str = "-----END SSH SIGNATURE-----";

/// An `ssh-ed25519` public key plus the exact wire blob it was written as.
/// The blob is what [`verify`] compares against the key embedded in a
/// signature: equality on the serialized form needs no reasoning about
/// canonical encodings, since both sides came off the same wire.
pub struct PublicKey {
    blob: Vec<u8>,
    key: VerifyingKey,
}

impl PublicKey {
    /// Parses one OpenSSH public-key line — `ssh-ed25519 <base64> [comment]`,
    /// the contents of a `.pub` file.
    pub fn parse(line: &str) -> Result<PublicKey> {
        let mut fields = line.split_whitespace();
        let (Some(kind), Some(encoded)) = (fields.next(), fields.next()) else {
            return Err(refuse("public key is not an `ssh-ed25519 <base64>` line"));
        };
        if kind != KEY_TYPE {
            return Err(refuse(format!("public key type {kind} is not {KEY_TYPE}")));
        }
        let blob = BASE64
            .decode(encoded)
            .map_err(|err| refuse(format!("public key base64 is invalid: {err}")))?;
        let key = key_from_blob(&blob)?;
        Ok(PublicKey { blob, key })
    }
}

/// Reads a `string "ssh-ed25519" ‖ string <32 bytes>` key blob — the payload
/// of a `.pub` line and of a signature's embedded key alike.
fn key_from_blob(blob: &[u8]) -> Result<VerifyingKey> {
    let mut reader = Reader::new(blob);
    if reader.string()? != KEY_TYPE.as_bytes() {
        return Err(refuse(format!("key blob does not declare {KEY_TYPE}")));
    }
    let bytes: [u8; 32] = reader
        .string()?
        .try_into()
        .map_err(|_| refuse("ed25519 key is not 32 bytes"))?;
    if !reader.done() {
        return Err(refuse("trailing bytes after the ed25519 key"));
    }
    VerifyingKey::from_bytes(&bytes).map_err(|err| refuse(format!("invalid ed25519 key: {err}")))
}

/// Verifies an armored SSHSIG `signature` over `message`, demanding that it
/// was made by `key` under `namespace`. Every refusal is
/// [`Error::Security`] — there is no soft failure on this path.
///
/// `namespace` is checked rather than merely read: it is what stops a
/// signature this key made for some other purpose from being replayed here.
pub fn verify(signature: &str, message: &[u8], key: &PublicKey, namespace: &str) -> Result<()> {
    let blob = dearmor(signature)?;
    let mut reader = Reader::new(&blob);

    if reader.take(MAGIC.len())? != MAGIC {
        return Err(refuse("signature does not start with the SSHSIG magic"));
    }
    let version = reader.u32()?;
    if version != VERSION {
        return Err(refuse(format!(
            "signature declares version {version}, expected {VERSION}"
        )));
    }
    let signer = reader.string()?;
    if signer != key.blob {
        return Err(refuse(
            "signature was made by a key other than the one this build trusts",
        ));
    }
    let signed_namespace = reader.string()?;
    if signed_namespace != namespace.as_bytes() {
        return Err(refuse(format!(
            "signature namespace is {}, expected {namespace}",
            String::from_utf8_lossy(signed_namespace)
        )));
    }
    // Read and discarded on purpose: OpenSSH signs and verifies with reserved
    // empty, so echoing a non-empty one back into the signed blob below would
    // accept signatures `ssh-keygen -Y verify` rejects. Both anchors have to
    // agree on what a release is.
    reader.string()?;
    let hash_algorithm = reader.string()?;
    let digest = hash(message, hash_algorithm)?;
    let signature = signature_from_blob(reader.string()?)?;
    if !reader.done() {
        return Err(refuse("trailing bytes after the signature"));
    }

    let mut signed = Vec::new();
    signed.extend_from_slice(MAGIC);
    put_string(&mut signed, namespace.as_bytes());
    put_string(&mut signed, b"");
    put_string(&mut signed, hash_algorithm);
    put_string(&mut signed, &digest);

    key.key
        .verify_strict(&signed, &signature)
        .map_err(|err| refuse(format!("signature does not verify: {err}")))
}

/// Hashes the message the way the signature says it was hashed. An algorithm
/// this build does not know is a refusal: the alternative is verifying a
/// digest of something other than what was signed.
fn hash(message: &[u8], algorithm: &[u8]) -> Result<Vec<u8>> {
    match algorithm {
        b"sha512" => Ok(Sha512::digest(message).to_vec()),
        b"sha256" => Ok(Sha256::digest(message).to_vec()),
        other => Err(refuse(format!(
            "unsupported signature hash {}",
            String::from_utf8_lossy(other)
        ))),
    }
}

/// Reads a `string "ssh-ed25519" ‖ string <64 bytes>` signature blob.
fn signature_from_blob(blob: &[u8]) -> Result<Signature> {
    let mut reader = Reader::new(blob);
    if reader.string()? != KEY_TYPE.as_bytes() {
        return Err(refuse(format!(
            "signature blob does not declare {KEY_TYPE}"
        )));
    }
    let bytes: [u8; 64] = reader
        .string()?
        .try_into()
        .map_err(|_| refuse("ed25519 signature is not 64 bytes"))?;
    if !reader.done() {
        return Err(refuse("trailing bytes after the signature blob"));
    }
    Ok(Signature::from_bytes(&bytes))
}

/// The bytes inside the `-----BEGIN/END SSH SIGNATURE-----` armor. The base64
/// arrives wrapped at 76 columns, so every whitespace run is dropped before
/// decoding.
fn dearmor(armored: &str) -> Result<Vec<u8>> {
    let body = armored
        .split_once(ARMOR_BEGIN)
        .and_then(|(_, rest)| rest.split_once(ARMOR_END))
        .map(|(body, _)| body)
        .ok_or_else(|| refuse("signature is not wrapped in SSH SIGNATURE armor"))?;
    let packed: String = body.split_whitespace().collect();
    BASE64
        .decode(&packed)
        .map_err(|err| refuse(format!("signature base64 is invalid: {err}")))
}

fn put_string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

fn refuse(reason: impl std::fmt::Display) -> Error {
    Error::Security(format!("release signature: {reason}"))
}

/// Cursor over an SSH wire blob. Every read is bounds-checked, so a truncated
/// or hostile blob ends as a refusal instead of a panic.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, at: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(len)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| refuse("signature ends mid-field"))?;
        let taken = &self.data[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("take(4) yields 4 bytes");
        Ok(u32::from_be_bytes(bytes))
    }

    fn string(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn done(&self) -> bool {
        self.at == self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `ssh-keygen -Y sign` output, so the parser is pinned to OpenSSH's
    /// bytes rather than to this file's reading of the spec. Regenerate with:
    ///
    /// ```text
    /// ssh-keygen -t ed25519 -N "" -C "jdk-release TEST KEY" -f signer
    /// ssh-keygen -Y sign -f signer -n jdk-release SHA256SUMS
    /// ```
    ///
    /// then commit `signer.pub`, `SHA256SUMS` and `SHA256SUMS.sig` — never
    /// `signer` itself. The seed of this key is repeated in `test-support`,
    /// which re-signs dynamic fixtures with it.
    const SUMS: &str = include_str!("../fixtures/sshsig/SHA256SUMS");
    const SIG: &str = include_str!("../fixtures/sshsig/SHA256SUMS.sig");
    const SIGNER: &str = include_str!("../fixtures/sshsig/signer.pub");
    const NAMESPACE: &str = "jdk-release";
    /// A third throwaway key, related to nothing here — what "some other
    /// key" means below. Deliberately NOT the production key: rotating that
    /// one is a find-replace over the pinned value, and it must not reach
    /// into a test whose whole point is that the key is the WRONG one.
    /// `test_support::sshsig::STRANGER` is the same key, for the tests that
    /// live outside this crate.
    const STRANGER: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKWLX33CJQAf9Dobd7asoLeR9l+b5XZAonlRjJHwX2yB not-the-jdk-release-key";

    fn signer() -> PublicKey {
        PublicKey::parse(SIGNER).unwrap()
    }

    #[test]
    fn accepts_what_ssh_keygen_signed() {
        verify(SIG, SUMS.as_bytes(), &signer(), NAMESPACE).unwrap();
    }

    #[test]
    fn refuses_a_message_that_changed() {
        // One flipped hex digit in the checksum of the x64 zip: the shape of
        // the attack the whole module exists to stop.
        let tampered = SUMS.replacen("3b9d5a3a", "3b9d5a3b", 1);
        assert_ne!(tampered, SUMS);
        let err = verify(SIG, tampered.as_bytes(), &signer(), NAMESPACE).unwrap_err();
        assert!(err.to_string().contains("does not verify"), "{err}");
    }

    #[test]
    fn refuses_another_namespace() {
        let err = verify(SIG, SUMS.as_bytes(), &signer(), "git").unwrap_err();
        assert!(err.to_string().contains("namespace"), "{err}");
    }

    #[test]
    fn refuses_a_signature_from_another_key() {
        // A syntactically perfect key that simply did not sign this. It is
        // deliberately NOT the production key: a rotation is a find-replace
        // over the pinned value, and it must not silently rewrite the key a
        // negative test depends on being wrong.
        let other = PublicKey::parse(STRANGER).unwrap();
        let err = verify(SIG, SUMS.as_bytes(), &other, NAMESPACE).unwrap_err();
        assert!(err.to_string().contains("other than the one"), "{err}");
    }

    #[test]
    fn refuses_broken_armor() {
        let err = verify(
            SIG.replace(ARMOR_END, "").as_str(),
            SUMS.as_bytes(),
            &signer(),
            NAMESPACE,
        )
        .unwrap_err();
        assert!(err.to_string().contains("armor"), "{err}");

        // Armor intact, payload corrupt: `!` is not a base64 character.
        let corrupt = SIG.replacen("U1NIU0lH", "U1NIU0l!", 1);
        let err = verify(&corrupt, SUMS.as_bytes(), &signer(), NAMESPACE).unwrap_err();
        assert!(err.to_string().contains("base64"), "{err}");
    }

    /// Every refusal is a security refusal, never an `Http` or a parse error
    /// some caller might decide to continue past.
    #[test]
    fn every_refusal_is_a_security_error() {
        for broken in [
            SIG.replace(ARMOR_END, ""),
            SIG.replacen("U1NIU0lH", "U1NIU0l!", 1),
            String::new(),
        ] {
            let err = verify(&broken, SUMS.as_bytes(), &signer(), NAMESPACE).unwrap_err();
            assert!(matches!(err, Error::Security(_)), "{err}");
        }
    }

    #[test]
    fn refuses_an_unknown_hash_algorithm() {
        // The fixture is sha512; rewrite the field to an algorithm this build
        // does not implement and the digest can no longer be reproduced.
        let blob = dearmor(SIG).unwrap();
        let mut forged = blob.clone();
        let at = find(&blob, b"sha512").expect("the fixture names its hash");
        forged[at..at + 6].copy_from_slice(b"sha333");
        let err = verify(&rearmor(&forged), SUMS.as_bytes(), &signer(), NAMESPACE).unwrap_err();
        assert!(err.to_string().contains("unsupported"), "{err}");
    }

    #[test]
    fn refuses_a_version_it_does_not_speak() {
        let mut forged = dearmor(SIG).unwrap();
        forged[MAGIC.len()..MAGIC.len() + 4].copy_from_slice(&2u32.to_be_bytes());
        let err = verify(&rearmor(&forged), SUMS.as_bytes(), &signer(), NAMESPACE).unwrap_err();
        assert!(err.to_string().contains("version 2"), "{err}");
    }

    /// A blob cut short at every field boundary must refuse, not panic: the
    /// reader is the only thing between a hostile release host and this
    /// process.
    #[test]
    fn refuses_truncation_at_any_length() {
        let blob = dearmor(SIG).unwrap();
        for len in 0..blob.len() {
            let err = verify(
                &rearmor(&blob[..len]),
                SUMS.as_bytes(),
                &signer(),
                NAMESPACE,
            )
            .unwrap_err();
            assert!(matches!(err, Error::Security(_)), "at {len}: {err}");
        }
    }

    #[test]
    fn public_keys_must_be_ed25519_lines() {
        assert!(PublicKey::parse(SIGNER).is_ok());
        // The comment is optional.
        let bare: String = SIGNER
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(PublicKey::parse(&bare).is_ok());

        assert!(PublicKey::parse("").is_err(), "empty");
        assert!(PublicKey::parse("ssh-ed25519").is_err(), "no key material");
        let rsa = SIGNER.replacen("ssh-ed25519", "ssh-rsa", 1);
        assert!(PublicKey::parse(&rsa).is_err(), "wrong type");
        assert!(
            PublicKey::parse("ssh-ed25519 not-base64!!").is_err(),
            "undecodable"
        );
        // Right type, right base64, wrong length for an ed25519 key.
        let short = format!(
            "ssh-ed25519 {}",
            BASE64.encode(b"\0\0\0\x0bssh-ed25519\0\0\0\x02hi")
        );
        assert!(PublicKey::parse(&short).is_err(), "short key");
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn rearmor(blob: &[u8]) -> String {
        format!("{ARMOR_BEGIN}\n{}\n{ARMOR_END}\n", BASE64.encode(blob))
    }
}
