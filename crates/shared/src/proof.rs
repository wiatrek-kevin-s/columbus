//! Proof that both ends of a local request hold the same key, without the
//! key ever being sent: the Columbus app and a session's local page each
//! answer a fresh random challenge with HMAC-SHA256(key, purpose || challenge).
//! A program that only listens on a port the app expected (another account
//! on the same computer, say) can neither answer nor learn the key.

use sha2::{Digest, Sha256};

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The proof for `purpose` and `parts`, as lowercase hex.
pub fn prove(key: &str, purpose: &str, parts: &[&[u8]]) -> String {
    let mut msg = Vec::new();
    msg.extend_from_slice(purpose.as_bytes());
    for p in parts {
        msg.push(0);
        msg.extend_from_slice(&(p.len() as u64).to_le_bytes());
        msg.extend_from_slice(p);
    }
    hex(&hmac_sha256(key.as_bytes(), &msg))
}

/// Compare two strings in time that does not depend on where they differ.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// The server's answer to the app's challenge.
pub const SERVER: &str = "columbus-test-server";
/// The app's proof for one test request (over the server's challenge and
/// the request body's SHA-256).
pub const APP: &str = "columbus-test-app";

pub fn body_hash(body: &[u8]) -> String {
    hex(&Sha256::digest(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than the block.
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn proofs_depend_on_purpose_key_and_every_part() {
        let p = prove("k", APP, &[b"a", b"b"]);
        assert_ne!(p, prove("k", SERVER, &[b"a", b"b"]));
        assert_ne!(p, prove("j", APP, &[b"a", b"b"]));
        assert_ne!(p, prove("k", APP, &[b"ab"]));
        assert!(same(&p, &prove("k", APP, &[b"a", b"b"])));
    }
}
