// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Windows DPAPI-based [`PskProtector`] implementation.
//!
//! Encrypts and decrypts PSK material using Windows DPAPI
//! (`CryptProtectData` / `CryptUnprotectData`).  The encrypted blob is tied
//! to the current Windows user account — it cannot be decrypted without the
//! account credentials, even with direct file-system access.
//!
//! Enabled by the `dpapi` cargo feature.

use zeroize::Zeroizing;

use crate::error::PairingError;
use crate::file_store::PskProtector;

/// Protects PSK material using Windows DPAPI.
///
/// No configuration is needed — DPAPI derives encryption keys from the
/// Windows user account.
pub struct DpapiPskProtector;

impl DpapiPskProtector {
    /// Create a new DPAPI-backed protector.
    pub fn new() -> Self {
        Self
    }
}

impl Default for DpapiPskProtector {
    fn default() -> Self {
        Self::new()
    }
}

impl PskProtector for DpapiPskProtector {
    fn protect(&self, psk: &[u8; 32]) -> Result<Vec<u8>, PairingError> {
        dpapi_ffi::encrypt(psk)
            .map_err(|e| PairingError::EncryptionFailed(format!("DPAPI encrypt: {e}")))
    }

    fn unprotect(&self, protected: &[u8]) -> Result<Zeroizing<[u8; 32]>, PairingError> {
        let plaintext = dpapi_ffi::decrypt(protected)
            .map_err(|e| PairingError::EncryptionFailed(format!("DPAPI decrypt: {e}")))?;
        let plaintext = Zeroizing::new(plaintext);

        if plaintext.len() != 32 {
            return Err(PairingError::StoreCorrupted(format!(
                "DPAPI decrypted to {} bytes, expected 32",
                plaintext.len()
            )));
        }

        let mut psk = Zeroizing::new([0u8; 32]);
        psk.copy_from_slice(&plaintext);
        Ok(psk)
    }
}

mod dpapi_ffi {
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    /// Decrypt a DPAPI-protected blob, returning the plaintext bytes.
    pub fn decrypt(encrypted_data: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: encrypted_data.len() as u32,
            pbData: encrypted_data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(), // description (out)
                std::ptr::null_mut(), // optional entropy
                std::ptr::null_mut(), // reserved
                std::ptr::null_mut(), // prompt struct
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };

        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(format!(
                "CryptUnprotectData failed: error code {code:#010x}"
            ));
        }

        let plaintext =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        unsafe { LocalFree(output.pbData as *mut _) };
        Ok(plaintext)
    }

    /// Encrypt plaintext bytes with DPAPI, returning the blob.
    pub fn encrypt(plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null_mut(), // description
                std::ptr::null_mut(), // optional entropy
                std::ptr::null_mut(), // reserved
                std::ptr::null_mut(), // prompt struct
                0,                    // flags (user-account scope by default)
                &mut output,
            )
        };

        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(format!("CryptProtectData failed: error code {code:#010x}"));
        }

        let encrypted =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        unsafe { LocalFree(output.pbData as *mut _) };
        Ok(encrypted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_store::PskProtector;

    /// Validates: PT-0801 (T-PT-605)
    ///
    /// DPAPI protect/unprotect round-trip: a 32-byte PSK encrypted via DPAPI
    /// must decrypt back to the original value.
    #[test]
    fn t_pt_605_dpapi_round_trip() {
        let protector = DpapiPskProtector::new();
        let psk = [0x42u8; 32];

        let protected = protector.protect(&psk).expect("DPAPI protect failed");
        assert_ne!(protected, psk, "protected bytes must differ from plaintext");
        assert!(
            protected.len() > 32,
            "DPAPI blob must be larger than the plaintext"
        );

        let recovered = protector
            .unprotect(&protected)
            .expect("DPAPI unprotect failed");
        assert_eq!(*recovered, psk, "round-trip must recover original PSK");
    }

    /// Validates: PT-0801 (T-PT-605)
    ///
    /// DPAPI unprotect with tampered data must fail.
    #[test]
    fn t_pt_605_dpapi_tampered_data_fails() {
        let protector = DpapiPskProtector::new();
        let psk = [0x42u8; 32];

        let mut protected = protector.protect(&psk).expect("DPAPI protect failed");
        if let Some(byte) = protected.last_mut() {
            *byte ^= 0xFF;
        }

        let result = protector.unprotect(&protected);
        assert!(result.is_err(), "tampered DPAPI blob must fail to decrypt");
    }

    /// Validates: PT-0801 (T-PT-605)
    ///
    /// Different PSKs must produce different DPAPI blobs.
    #[test]
    fn t_pt_605_dpapi_different_keys_differ() {
        let protector = DpapiPskProtector::new();

        let blob_a = protector.protect(&[0x42u8; 32]).unwrap();
        let blob_b = protector.protect(&[0x43u8; 32]).unwrap();
        assert_ne!(
            blob_a, blob_b,
            "different PSKs must produce different blobs"
        );
    }
}
