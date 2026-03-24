//! Self-encryption using lair's crypto_box with Ed25519 signing keys.
//!
//! Uses `crypto_box_xsalsa_by_sign_pub_key` for encryption and
//! `crypto_box_xsalsa_open_by_sign_pub_key` for decryption.
//! Lair internally converts Ed25519 → X25519, so we pass the agent's
//! signing key directly. For self-encryption, the agent is both sender
//! and recipient.

use lair_keystore_api::prelude::*;
use std::sync::Arc;

/// Encrypt data to self using the agent's Ed25519 signing key.
/// Returns (24-byte nonce, ciphertext).
pub async fn encrypt_to_self(
    lair_client: &LairClient,
    agent_ed25519_bytes: [u8; 32],
    plaintext: &[u8],
) -> Result<([u8; 24], Vec<u8>), String> {
    let pub_key: Ed25519PubKey = BinDataSized(Arc::new(agent_ed25519_bytes));
    let data: Arc<[u8]> = plaintext.into();

    let (nonce, cipher) = lair_client
        .crypto_box_xsalsa_by_sign_pub_key(
            pub_key.clone(), // sender = self
            pub_key,         // recipient = self
            None,            // no deep lock passphrase
            data,
        )
        .await
        .map_err(|e| format!("Encryption failed: {}", e))?;

    Ok((nonce, cipher.to_vec()))
}

/// Decrypt data from self using the agent's Ed25519 signing key.
pub async fn decrypt_from_self(
    lair_client: &LairClient,
    agent_ed25519_bytes: [u8; 32],
    nonce: [u8; 24],
    cipher: &[u8],
) -> Result<Vec<u8>, String> {
    let pub_key: Ed25519PubKey = BinDataSized(Arc::new(agent_ed25519_bytes));
    let cipher_arc: Arc<[u8]> = cipher.into();

    let plaintext = lair_client
        .crypto_box_xsalsa_open_by_sign_pub_key(
            pub_key.clone(), // sender = self
            pub_key,         // recipient = self
            None,            // no deep lock passphrase
            nonce,
            cipher_arc,
        )
        .await
        .map_err(|e| format!("Decryption failed: {}", e))?;

    Ok(plaintext.to_vec())
}
