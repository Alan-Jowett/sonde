// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

package io.sonde.pair;

import android.content.Context;
import android.content.SharedPreferences;

import androidx.security.crypto.EncryptedSharedPreferences;
import androidx.security.crypto.MasterKeys;

/**
 * JNI-friendly persistent store backed by {@code EncryptedSharedPreferences}.
 *
 * <p>All values are encrypted at rest using AES-256-GCM (value encryption)
 * and AES-256-SIV (key encryption), with the master key managed by the
 * Android Keystore.
 *
 * <h3>Gradle dependency</h3>
 * The consuming app must include:
 * <pre>{@code
 * implementation "androidx.security:security-crypto:1.1.0-alpha06"
 * }</pre>
 *
 * <p>Byte arrays are stored as hex-encoded strings to stay within the
 * {@code SharedPreferences} type system.
 */
public class SecureStore {

    private static final String PREFS_NAME = "sonde_pairing_store";

    private final SharedPreferences prefs;

    /**
     * Open (or create) the encrypted preference store.
     *
     * @throws Exception if the Android Keystore or crypto initialisation fails
     */
    public SecureStore(Context context) throws Exception {
        String masterKey = MasterKeys.getOrCreate(MasterKeys.AES256_GCM_SPEC);
        this.prefs = EncryptedSharedPreferences.create(
                PREFS_NAME,
                masterKey,
                context.getApplicationContext(),
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM);
    }

    // --- Byte array storage (hex-encoded) ----------------------------------

    /** Store a byte array under {@code key}. */
    public void putBytes(String key, byte[] value) {
        prefs.edit().putString(key, bytesToHex(value)).apply();
    }

    /**
     * Retrieve a byte array previously stored with {@link #putBytes}.
     *
     * @return the bytes, or {@code null} if the key does not exist
     */
    public byte[] getBytes(String key) {
        String hex = prefs.getString(key, null);
        if (hex == null) return null;
        return hexToBytes(hex);
    }

    // --- String storage ----------------------------------------------------

    public void putString(String key, String value) {
        prefs.edit().putString(key, value).apply();
    }

    /** @return the string value, or {@code null} if absent */
    public String getString(String key) {
        return prefs.getString(key, null);
    }

    // --- Integer storage ---------------------------------------------------

    public void putInt(String key, int value) {
        prefs.edit().putInt(key, value).apply();
    }

    /**
     * @param defaultValue returned when the key does not exist
     */
    public int getInt(String key, int defaultValue) {
        return prefs.getInt(key, defaultValue);
    }

    // --- Deletion ----------------------------------------------------------

    /** Remove a single key. */
    public void remove(String key) {
        prefs.edit().remove(key).apply();
    }

    /** Wipe all entries in the store. */
    public void clear() {
        prefs.edit().clear().apply();
    }

    // --- Hex helpers -------------------------------------------------------

    private static String bytesToHex(byte[] bytes) {
        StringBuilder sb = new StringBuilder(bytes.length * 2);
        for (byte b : bytes) {
            sb.append(String.format("%02x", b & 0xFF));
        }
        return sb.toString();
    }

    private static byte[] hexToBytes(String hex) {
        int len = hex.length();
        byte[] out = new byte[len / 2];
        for (int i = 0; i < len; i += 2) {
            out[i / 2] = (byte) ((Character.digit(hex.charAt(i), 16) << 4)
                    + Character.digit(hex.charAt(i + 1), 16));
        }
        return out;
    }
}
