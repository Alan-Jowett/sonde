// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Android persistent store via JNI bridge to `EncryptedSharedPreferences`.
//!
//! Requires the companion Java class [`io.sonde.pair.SecureStore`] to be
//! included in the consuming Android app's classpath.  The Java source
//! ships in `crates/sonde-pair/java/io/sonde/pair/SecureStore.java`.
//!
//! # Gradle dependency
//!
//! The consuming app must include `androidx.security:security-crypto:1.1.0-alpha06`
//! (or later) in its Gradle build.
//!
//! # Storage layout
//!
//! Each [`PairingArtifacts`] field is stored as a separate
//! `SharedPreferences` entry:
//!
//! | Key              | Type          | Encoding       |
//! |------------------|---------------|----------------|
//! | `gw_public_key`  | `[u8; 32]`    | hex string     |
//! | `gateway_id`     | `[u8; 16]`    | hex string     |
//! | `phone_psk`      | `[u8; 32]`    | hex string     |
//! | `phone_key_hint` | `u16`         | int            |
//! | `rf_channel`     | `u8`          | int            |
//! | `phone_label`    | `String`      | string         |
//!
//! `node_psk` is **never** persisted (per PT-0801).

use std::sync::OnceLock;

use jni::objects::{JByteArray, JClass, JObject, JString, JValue};
use jni::refs::Global;
use jni::{jni_sig, jni_str, Env, JavaVM};
use tracing::debug;
use zeroize::Zeroizing;

use crate::error::PairingError;
use crate::store::PairingStore;
use crate::types::{GatewayIdentity, PairingArtifacts};

/// Cached JavaVM for creating stores on demand (set in `JNI_OnLoad`).
static CACHED_STORE_VM: OnceLock<JavaVM> = OnceLock::new();

/// Cached `SecureStore` class ref — see [`CACHED_HELPER_CLASS`](super::android_transport)
/// for the rationale.
static CACHED_STORE_CLASS: OnceLock<Global<JClass<'static>>> = OnceLock::new();

// SharedPreferences key constants
const KEY_GW_PUBLIC_KEY: &str = "gw_public_key";
const KEY_GATEWAY_ID: &str = "gateway_id";
const KEY_PHONE_PSK: &str = "phone_psk";
const KEY_PHONE_KEY_HINT: &str = "phone_key_hint";
const KEY_RF_CHANNEL: &str = "rf_channel";
const KEY_PHONE_LABEL: &str = "phone_label";

/// Sentinel value returned by `SecureStore.getInt` when the key is absent.
const INT_ABSENT: i32 = -1;

/// Android pairing store backed by `EncryptedSharedPreferences`.
///
/// Each artifact field maps to a separate SharedPreferences entry
/// (see [module docs](self) for the key layout).
///
/// # Construction
///
/// ```rust,ignore
/// let store = AndroidPairingStore::new(&mut env, &activity_context)?;
/// ```
pub struct AndroidPairingStore {
    vm: JavaVM,
    store: Global<JObject<'static>>,
}

// SAFETY: Same justification as AndroidBleTransport — JavaVM is Send+Sync
// and GlobalRef is Send.
unsafe impl Send for AndroidPairingStore {}
unsafe impl Sync for AndroidPairingStore {}

impl AndroidPairingStore {
    /// Create a new store, initialising the Java `SecureStore` via JNI.
    ///
    /// [`cache_store_class()`] **must** have been called first (typically
    /// from `JNI_OnLoad`) to resolve the `SecureStore` class on a thread
    /// with the application classloader.
    ///
    /// `context` must be an Android `Context`.
    pub fn new(env: &mut Env<'_>, context: &JObject<'_>) -> Result<Self, PairingError> {
        let vm = env.get_java_vm().map_err(store_jni_err)?;

        let cached = CACHED_STORE_CLASS.get().ok_or_else(|| {
            PairingError::StoreSaveFailed(
                "SecureStore class not cached — call cache_store_class() \
                 from JNI_OnLoad before using the store"
                    .into(),
            )
        })?;

        let store_obj = env
            .new_object(
                &**cached,
                jni_sig!("(Landroid/content/Context;)V"),
                &[JValue::Object(context)],
            )
            .map_err(|e| {
                let msg = jni_exception_msg(env).unwrap_or_else(|| e.to_string());
                PairingError::StoreSaveFailed(format!("SecureStore init: {msg}"))
            })?;

        let store_ref = env
            .new_global_ref(&store_obj)
            .map_err(|e| PairingError::StoreSaveFailed(format!("GlobalRef: {e}")))?;

        debug!("AndroidPairingStore initialised");

        Ok(Self {
            vm,
            store: store_ref,
        })
    }

    /// Cache the `JavaVM` for later use by [`from_cached_vm()`].
    pub fn cache_vm(vm: JavaVM) {
        let _ = CACHED_STORE_VM.set(vm);
        debug!("AndroidPairingStore: JavaVM cached");
    }

    /// Resolve and cache the `SecureStore` class reference.
    ///
    /// Must be called from a thread with the application classloader
    /// (e.g. the main thread inside `JNI_OnLoad`).
    pub fn cache_store_class(env: &mut Env<'_>) -> Result<(), PairingError> {
        let cls = env
            .find_class(jni_str!("io/sonde/pair/SecureStore"))
            .map_err(|e| {
                PairingError::StoreSaveFailed(format!(
                    "SecureStore class not found — ensure io.sonde.pair.SecureStore \
                 is compiled into the APK and androidx.security:security-crypto \
                 is in the Gradle dependencies: {e}"
                ))
            })?;
        let global = env.new_global_ref(cls).map_err(store_jni_err)?;
        let _ = CACHED_STORE_CLASS.set(global);
        debug!("AndroidPairingStore: SecureStore class cached");
        Ok(())
    }

    /// Create a new store from the cached `JavaVM`.
    /// [`cache_vm()`] must have been called first.
    pub fn from_cached_vm() -> Result<Self, PairingError> {
        let vm = CACHED_STORE_VM.get().ok_or_else(|| {
            PairingError::StoreSaveFailed("JavaVM not cached — call cache_vm() first".into())
        })?;
        vm.attach_current_thread(|env| {
            let context = get_app_context(env)?;
            Self::new(env, &context)
        })
    }
}

impl PairingStore for AndroidPairingStore {
    fn save_artifacts(&mut self, artifacts: &PairingArtifacts) -> Result<(), PairingError> {
        self.vm.attach_current_thread(|env| {
            let store = self.store.as_obj();

            put_bytes(
                env,
                store,
                KEY_GW_PUBLIC_KEY,
                &artifacts.gateway_identity.public_key,
            )?;
            put_bytes(
                env,
                store,
                KEY_GATEWAY_ID,
                &artifacts.gateway_identity.gateway_id,
            )?;
            put_bytes(env, store, KEY_PHONE_PSK, artifacts.phone_psk.as_ref())?;
            put_int(
                env,
                store,
                KEY_PHONE_KEY_HINT,
                artifacts.phone_key_hint as i32,
            )?;
            put_int(env, store, KEY_RF_CHANNEL, artifacts.rf_channel as i32)?;
            put_string(env, store, KEY_PHONE_LABEL, &artifacts.phone_label)?;

            debug!("pairing artifacts saved");
            Ok(())
        })
    }

    fn load_artifacts(&self) -> Result<Option<PairingArtifacts>, PairingError> {
        self.vm.attach_current_thread(|env| {
            let store = self.store.as_obj();

            let gw_public_key = match get_bytes(env, store, KEY_GW_PUBLIC_KEY)? {
                Some(b) => b,
                None => return Ok(None),
            };
            let gateway_id = match get_bytes(env, store, KEY_GATEWAY_ID)? {
                Some(b) => b,
                None => return Ok(None),
            };
            let phone_psk = match get_bytes(env, store, KEY_PHONE_PSK)? {
                Some(b) => b,
                None => return Ok(None),
            };
            let phone_key_hint = get_int(env, store, KEY_PHONE_KEY_HINT)?;
            if phone_key_hint == INT_ABSENT {
                return Ok(None);
            }
            let rf_channel = get_int(env, store, KEY_RF_CHANNEL)?;
            if rf_channel == INT_ABSENT {
                return Ok(None);
            }
            let phone_label = get_string(env, store, KEY_PHONE_LABEL)?.unwrap_or_default();

            let gw_pk: [u8; 32] = gw_public_key.try_into().map_err(|_| {
                PairingError::StoreLoadFailed("gw_public_key: expected 32 bytes".into())
            })?;
            let gw_id: [u8; 16] = gateway_id.try_into().map_err(|_| {
                PairingError::StoreLoadFailed("gateway_id: expected 16 bytes".into())
            })?;
            let psk: [u8; 32] = phone_psk.try_into().map_err(|_| {
                PairingError::StoreLoadFailed("phone_psk: expected 32 bytes".into())
            })?;

            Ok(Some(PairingArtifacts {
                gateway_identity: GatewayIdentity {
                    public_key: gw_pk,
                    gateway_id: gw_id,
                },
                phone_psk: Zeroizing::new(psk),
                phone_key_hint: phone_key_hint as u16,
                rf_channel: rf_channel as u8,
                phone_label,
            }))
        })
    }

    fn clear(&mut self) -> Result<(), PairingError> {
        self.vm.attach_current_thread(|env| {
            env.call_method(self.store.as_obj(), jni_str!("clear"), jni_sig!("()V"), &[])
                .map_err(|e| store_jni_exception(env, "clear", e))?;
            debug!("pairing store cleared");
            Ok(())
        })
    }

    fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, PairingError> {
        self.vm.attach_current_thread(|env| {
            let store = self.store.as_obj();

            let gw_public_key = match get_bytes(env, store, KEY_GW_PUBLIC_KEY)? {
                Some(b) => b,
                None => return Ok(None),
            };
            let gateway_id = match get_bytes(env, store, KEY_GATEWAY_ID)? {
                Some(b) => b,
                None => return Ok(None),
            };

            let gw_pk: [u8; 32] = gw_public_key.try_into().map_err(|_| {
                PairingError::StoreLoadFailed("gw_public_key: expected 32 bytes".into())
            })?;
            let gw_id: [u8; 16] = gateway_id.try_into().map_err(|_| {
                PairingError::StoreLoadFailed("gateway_id: expected 16 bytes".into())
            })?;

            Ok(Some(GatewayIdentity {
                public_key: gw_pk,
                gateway_id: gw_id,
            }))
        })
    }

    fn save_gateway_identity(&mut self, identity: &GatewayIdentity) -> Result<(), PairingError> {
        self.vm.attach_current_thread(|env| {
            let store = self.store.as_obj();

            put_bytes(env, store, KEY_GW_PUBLIC_KEY, &identity.public_key)?;
            put_bytes(env, store, KEY_GATEWAY_ID, &identity.gateway_id)?;

            debug!("gateway identity saved");
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// JNI helpers — SecureStore method wrappers
// ---------------------------------------------------------------------------

fn put_bytes(
    env: &mut Env<'_>,
    store: &JObject<'_>,
    key: &str,
    value: &[u8],
) -> Result<(), PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let val_arr = env.byte_array_from_slice(value).map_err(store_jni_err)?;
    env.call_method(
        store,
        jni_str!("putBytes"),
        jni_sig!("(Ljava/lang/String;[B)V"),
        &[
            JValue::Object(key_jstr.as_ref()),
            JValue::Object(val_arr.as_ref()),
        ],
    )
    .map_err(|e| store_jni_exception(env, "putBytes", e))?;
    Ok(())
}

fn get_bytes(
    env: &mut Env<'_>,
    store: &JObject<'_>,
    key: &str,
) -> Result<Option<Vec<u8>>, PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let result = env
        .call_method(
            store,
            jni_str!("getBytes"),
            jni_sig!("(Ljava/lang/String;)[B"),
            &[JValue::Object(key_jstr.as_ref())],
        )
        .map_err(|e| store_jni_exception(env, "getBytes", e))?
        .l()
        .map_err(store_jni_err)?;

    if result.is_null() {
        return Ok(None);
    }

    let bytes = env
        .convert_byte_array(unsafe { JByteArray::from_raw(env, result.into_raw()) })
        .map_err(store_jni_err)?;
    Ok(Some(bytes))
}

fn put_int(
    env: &mut Env<'_>,
    store: &JObject<'_>,
    key: &str,
    value: i32,
) -> Result<(), PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    env.call_method(
        store,
        jni_str!("putInt"),
        jni_sig!("(Ljava/lang/String;I)V"),
        &[JValue::Object(key_jstr.as_ref()), JValue::Int(value)],
    )
    .map_err(|e| store_jni_exception(env, "putInt", e))?;
    Ok(())
}

fn get_int(env: &mut Env<'_>, store: &JObject<'_>, key: &str) -> Result<i32, PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let val = env
        .call_method(
            store,
            jni_str!("getInt"),
            jni_sig!("(Ljava/lang/String;I)I"),
            &[JValue::Object(key_jstr.as_ref()), JValue::Int(INT_ABSENT)],
        )
        .map_err(|e| store_jni_exception(env, "getInt", e))?
        .i()
        .map_err(store_jni_err)?;
    Ok(val)
}

fn put_string(
    env: &mut Env<'_>,
    store: &JObject<'_>,
    key: &str,
    value: &str,
) -> Result<(), PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let val_jstr = env.new_string(value).map_err(store_jni_err)?;
    env.call_method(
        store,
        jni_str!("putString"),
        jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
        &[
            JValue::Object(key_jstr.as_ref()),
            JValue::Object(val_jstr.as_ref()),
        ],
    )
    .map_err(|e| store_jni_exception(env, "putString", e))?;
    Ok(())
}

fn get_string(
    env: &mut Env<'_>,
    store: &JObject<'_>,
    key: &str,
) -> Result<Option<String>, PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let result = env
        .call_method(
            store,
            jni_str!("getString"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(key_jstr.as_ref())],
        )
        .map_err(|e| store_jni_exception(env, "getString", e))?
        .l()
        .map_err(store_jni_err)?;

    if result.is_null() {
        return Ok(None);
    }

    let s: String = unsafe { JString::from_raw(env, result.into_raw()) }
        .try_to_string(env)
        .map_err(store_jni_err)?;
    Ok(Some(s))
}

// ---------------------------------------------------------------------------
/// Get the Application context via `ActivityThread.currentApplication()`.
fn get_app_context<'a>(env: &mut Env<'a>) -> Result<JObject<'a>, PairingError> {
    let activity_thread = env
        .find_class(jni_str!("android/app/ActivityThread"))
        .map_err(store_jni_err)?;
    let app = env
        .call_static_method(
            activity_thread,
            jni_str!("currentApplication"),
            jni_sig!("()Landroid/app/Application;"),
            &[],
        )
        .and_then(|v| v.l())
        .map_err(|e| {
            let msg = jni_exception_msg(env).unwrap_or_else(|| e.to_string());
            PairingError::StoreSaveFailed(format!("currentApplication: {msg}"))
        })?;
    if app.is_null() {
        return Err(PairingError::StoreSaveFailed(
            "ActivityThread.currentApplication() returned null".into(),
        ));
    }
    Ok(app)
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

fn store_jni_err(e: jni::errors::Error) -> PairingError {
    PairingError::StoreSaveFailed(format!("JNI error: {e}"))
}

fn store_jni_exception(env: &mut Env<'_>, context: &str, err: jni::errors::Error) -> PairingError {
    let detail = match err {
        jni::errors::Error::JavaException => {
            jni_exception_msg(env).unwrap_or_else(|| "(unknown Java exception)".into())
        }
        other => other.to_string(),
    };
    PairingError::StoreSaveFailed(format!("{context}: {detail}"))
}

fn jni_exception_msg(env: &mut Env<'_>) -> Option<String> {
    if !env.exception_check() {
        return None;
    }
    let exc = env.exception_occurred()?;
    env.exception_clear();

    let msg_obj = env
        .call_method(
            &exc,
            jni_str!("getMessage"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )
        .ok()?
        .l()
        .ok()?;
    if msg_obj.is_null() {
        return Some("(no message)".into());
    }
    let msg = unsafe { JString::from_raw(env, msg_obj.into_raw()) }
        .try_to_string(env)
        .ok()?;
    Some(msg)
}
