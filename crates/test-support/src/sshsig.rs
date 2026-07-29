//! SSHSIG signer for release fixtures — the counterpart of
//! `jdk_core::sshsig`'s verifier, so a test can sign a `SHA256SUMS` it just
//! computed instead of only replaying a static one.
//!
//! The key is a hardcoded seed rather than a generated one, and it is the
//! same key that signed `crates/jdk-core/fixtures/sshsig/`: ed25519 signing
//! is deterministic, so [`sign`] reproduces `ssh-keygen -Y sign` byte for
//! byte over the same message — which is what pins this encoder to OpenSSH
//! instead of to the verifier it feeds. Only the 32-byte seed lives here,
//! never an OpenSSH private key file: this crate is never published, and a
//! seed in a byte array is not what a secret scanner trips on.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha512};

/// Seed of the throwaway pair generated with
/// `ssh-keygen -t ed25519 -N "" -C "<COMMENT>" -f signer`, read out of the
/// resulting private key. Regenerating it means regenerating the committed
/// fixtures too.
const SEED: [u8; 32] = [
    0x9d, 0x33, 0xf6, 0xcd, 0x7b, 0xf2, 0x1c, 0xc4, 0x99, 0x29, 0xe7, 0x51, 0x02, 0xc7, 0xac, 0x8b,
    0xbc, 0x17, 0xf8, 0x49, 0x4b, 0xd8, 0x88, 0x72, 0x71, 0xfb, 0xdc, 0xe8, 0x0d, 0x60, 0x4c, 0xf4,
];
/// The comment `ssh-keygen` wrote into the committed `signer.pub`, repeated
/// so [`pubkey`] reproduces that line exactly.
const COMMENT: &str = "jdk-release TEST KEY - not the production key";
/// `ssh-keygen` wraps the armored base64 at 70 columns.
const ARMOR_WIDTH: usize = 70;

/// A key unrelated to everything else here: what "signed by someone else"
/// means in a negative test. Deliberately NOT the production key — rotating
/// that one is a find-replace over the pinned value, and it must not reach
/// into tests whose whole point is that the key is the WRONG one. Only the
/// public half exists, here and in `jdk_core::sshsig`'s own tests; nothing
/// can sign with it.
pub const STRANGER: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKWLX33CJQAf9Dobd7asoLeR9l+b5XZAonlRjJHwX2yB not-the-jdk-release-key";

/// The signer's OpenSSH public-key line — what a test pins as the key to
/// trust, in the same `.pub` format the production key is pinned in.
pub fn pubkey() -> String {
    let blob = key_blob();
    format!("ssh-ed25519 {} {COMMENT}\n", BASE64.encode(&blob))
}

/// An armored SSHSIG signature over `message` under `namespace`, hashed with
/// sha512 — the shape `ssh-keygen -Y sign` emits by default.
pub fn sign(message: &[u8], namespace: &str) -> String {
    let key = SigningKey::from_bytes(&SEED);

    let mut signed = Vec::new();
    signed.extend_from_slice(b"SSHSIG");
    put_string(&mut signed, namespace.as_bytes());
    put_string(&mut signed, b"");
    put_string(&mut signed, b"sha512");
    put_string(&mut signed, &Sha512::digest(message));

    let mut signature = Vec::new();
    put_string(&mut signature, b"ssh-ed25519");
    put_string(&mut signature, &key.sign(&signed).to_bytes());

    let mut blob = Vec::new();
    blob.extend_from_slice(b"SSHSIG");
    blob.extend_from_slice(&1u32.to_be_bytes());
    put_string(&mut blob, &key_blob());
    put_string(&mut blob, namespace.as_bytes());
    put_string(&mut blob, b"");
    put_string(&mut blob, b"sha512");
    put_string(&mut blob, &signature);

    let encoded = BASE64.encode(&blob);
    let mut armored = String::from("-----BEGIN SSH SIGNATURE-----\n");
    for line in encoded.as_bytes().chunks(ARMOR_WIDTH) {
        armored.push_str(std::str::from_utf8(line).expect("base64 is ascii"));
        armored.push('\n');
    }
    armored.push_str("-----END SSH SIGNATURE-----\n");
    armored
}

/// `string "ssh-ed25519" ‖ string <32-byte key>` — the blob a `.pub` line
/// base64s and a signature embeds.
fn key_blob() -> Vec<u8> {
    let mut blob = Vec::new();
    put_string(&mut blob, b"ssh-ed25519");
    put_string(
        &mut blob,
        SigningKey::from_bytes(&SEED).verifying_key().as_bytes(),
    );
    blob
}

fn put_string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}
