//! Compatibility reader for OpenVPN Connect desktop 3.8.2's saved passwords.
//! Reads one generic Keychain item; never writes, exports or enumerates secrets.
use anyhow::Result;
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
pub(super) fn read(profile_id: &str) -> Result<Option<Zeroizing<String>>> {
    use security_framework::os::macos::passwords::find_generic_password;

    // keytar's generic-password service/account, not the VPN username. Respect
    // Keychain ACLs: macOS can ask permission or refuse access to this process.
    match find_generic_password(None, "org.openvpn.client.", profile_id) {
        Ok((encoded, _item)) => decode(encoded.as_ref(), profile_id).map(Some),
        Err(error) if error.code() == -25300 => Ok(None), // errSecItemNotFound
        Err(error) => {
            // OSStatus is safe to report; never log native error payloads/items.
            tracing::debug!(
                os_status = error.code(),
                "OpenVPN Connect Keychain access failed"
            );
            anyhow::bail!("OpenVPN Connect Keychain access failed or was denied")
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn read(_profile_id: &str) -> Result<Option<Zeroizing<String>>> {
    anyhow::bail!("OpenVPN Connect password retrieval is only supported on macOS")
}

#[cfg(any(target_os = "macos", test))]
fn decode(encoded: &[u8], profile_id: &str) -> Result<Zeroizing<String>> {
    use aes_gcm::{Aes128Gcm, KeyInit, Nonce, Tag, aead::AeadInPlace};
    use anyhow::ensure;
    use base64::{Engine, engine::general_purpose::STANDARD};

    // Connect's format (not a new password-storage scheme):
    // base64(salt[16] || nonce[12] || ciphertext || GCM tag[16]).
    // AES-128 key = PBKDF2-HMAC-SHA1(profile ID UTF-8, salt, 32767, 16).
    // No AAD. Authentication MUST succeed before using any plaintext.
    ensure!(
        encoded.len() <= 64 * 1024,
        "OpenVPN Connect credential is too large"
    );
    let mut bytes = Zeroizing::new(
        STANDARD
            .decode(encoded)
            .map_err(|_| anyhow::anyhow!("invalid OpenVPN Connect credential encoding"))?,
    );
    ensure!(bytes.len() >= 44, "truncated OpenVPN Connect credential");
    let (salt, rest) = bytes.split_at_mut(16);
    let (nonce, rest) = rest.split_at_mut(12);
    let (ciphertext, tag) = rest.split_at_mut(rest.len() - 16);
    let mut key = Zeroizing::new([0u8; 16]);
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(profile_id.as_bytes(), salt, 32767, &mut *key);
    let cipher = Aes128Gcm::new_from_slice(key.as_ref()).expect("AES-128 key has 16 bytes");
    cipher
        .decrypt_in_place_detached(
            Nonce::from_slice(nonce),
            &[],
            ciphertext,
            Tag::from_slice(tag),
        )
        .map_err(|_| anyhow::anyhow!("could not authenticate OpenVPN Connect credential"))?;
    // Avoid FromUtf8Error: its Debug representation can contain plaintext bytes.
    let password = std::str::from_utf8(ciphertext)
        .map_err(|_| anyhow::anyhow!("OpenVPN Connect credential is not UTF-8"))?;
    Ok(Zeroizing::new(password.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    // Independent Node.js crypto vectors matching Connect's wire format.
    // ID: 1700000000123, salt: 00..0f, nonce: 10..1b. All data is synthetic.
    const ID: &str = "1700000000123";
    const UNICODE: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaG4c/lRI2Dv2TSqYjLwxoN3Dl8M+nK3NkZ/kwMF/hSse2ngek3035YonDIg==";

    #[test]
    fn decodes_independent_unicode_and_empty_password_vectors() {
        assert_eq!(
            decode(UNICODE.as_bytes(), ID).unwrap().as_str(),
            "synthetic-password-è-🔐"
        );
        assert!(
            decode(
                b"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaG1AdmTIiHenWCB2YLkBy6ZQ=",
                ID
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn rejects_wrong_profile_and_tampering_in_every_component() {
        assert!(decode(UNICODE.as_bytes(), "different-profile").is_err());
        let original = STANDARD.decode(UNICODE).unwrap();
        for offset in [0, 16, 28, original.len() - 1] {
            let mut bytes = original.clone();
            bytes[offset] ^= 1;
            assert!(decode(STANDARD.encode(bytes).as_bytes(), ID).is_err());
        }
    }

    #[test]
    fn rejects_malformed_truncated_and_non_utf8_values_without_echoing_data() {
        for encoded in [
            "not-base64-password-secret",
            "",
            "AA==",
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGwsspi9KINEEbQFbZkYu7/qL",
        ] {
            let error = decode(encoded.as_bytes(), ID).unwrap_err();
            assert!(!format!("{error:?}").contains("password-secret"));
        }
        assert!(decode(&vec![b'A'; 64 * 1024 + 1], ID).is_err());
    }
}
