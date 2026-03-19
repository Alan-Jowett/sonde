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

use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JString, JValue};
use jni::JNIEnv;
use jni::JavaVM;
use tracing::debug;
use zeroize::Zeroizing;

use crate::error::PairingError;
use crate::store::PairingStore;
use crate::types::{GatewayIdentity, PairingArtifacts};

/// Cached JavaVM for creating stores on demand (set in `JNI_OnLoad`).
static CACHED_STORE_VM: OnceLock<JavaVM> = OnceLock::new();

/// Cached `SecureStore` class ref — see [`CACHED_HELPER_CLASS`](super::android_transport)
/// for the rationale.
static CACHED_STORE_CLASS: OnceLock<GlobalRef> = OnceLock::new();

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
    store: GlobalRef,
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
    pub fn new(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Result<Self, PairingError> {
        let vm = env.get_java_vm().map_err(store_jni_err)?;

        let cached = CACHED_STORE_CLASS.get().ok_or_else(|| {
            PairingError::StoreSaveFailed(
                "SecureStore class not cached — call cache_store_class() \
                 from JNI_OnLoad before using the store"
                    .into(),
            )
        })?;

        // SAFETY: The GlobalRef was created from find_class(), which returns
        // a JClass.  We reconstruct a JClass from the raw jobject pointer;
        // the GlobalRef keeps the underlying reference alive.
        let store_class =
            unsafe { JClass::from_raw(cached.as_obj().as_raw()) };

        let store_obj = env
            .new_object(
                store_class,
                "(Landroid/content/Context;)V",
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
    pub fn cache_store_class(env: &mut JNIEnv<'_>) -> Result<(), PairingError> {
        let cls = env.find_class("io/sonde/pair/SecureStore").map_err(|e| {
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
        let mut env = vm.attach_current_thread().map_err(store_jni_err)?;
        let context = get_app_context(&mut env)?;
        Self::new(&mut env, &context)
    }
}

impl PairingStore for AndroidPairingStore {
    fn save_artifacts(&mut self, artifacts: &PairingArtifacts) -> Result<(), PairingError> {
        let mut env = self.vm.attach_current_thread().map_err(store_jni_err)?;
        let store = self.store.as_obj();

        put_bytes(
            &mut env,
            store,
            KEY_GW_PUBLIC_KEY,
            &artifacts.gateway_identity.public_key,
        )?;
        put_bytes(
            &mut env,
            store,
            KEY_GATEWAY_ID,
            &artifacts.gateway_identity.gateway_id,
        )?;
        put_bytes(&mut env, store, KEY_PHONE_PSK, artifacts.phone_psk.as_ref())?;
        put_int(
            &mut env,
            store,
            KEY_PHONE_KEY_HINT,
            artifacts.phone_key_hint as i32,
        )?;
        put_int(&mut env, store, KEY_RF_CHANNEL, artifacts.rf_channel as i32)?;
        put_string(&mut env, store, KEY_PHONE_LABEL, &artifacts.phone_label)?;

        debug!("pairing artifacts saved");
        Ok(())
    }

    fn load_artifacts(&self) -> Result<Option<PairingArtifacts>, PairingError> {
        let mut env = self.vm.attach_current_thread().map_err(store_jni_err)?;
        let store = self.store.as_obj();

        let gw_public_key = match get_bytes(&mut env, store, KEY_GW_PUBLIC_KEY)? {
            Some(b) => b,
            None => return Ok(None),
        };
        let gateway_id = match get_bytes(&mut env, store, KEY_GATEWAY_ID)? {
            Some(b) => b,
            None => return Ok(None),
        };
        let phone_psk = match get_bytes(&mut env, store, KEY_PHONE_PSK)? {
            Some(b) => b,
            None => return Ok(None),
        };
        let phone_key_hint = get_int(&mut env, store, KEY_PHONE_KEY_HINT)?;
        if phone_key_hint == INT_ABSENT {
            return Ok(None);
        }
        let rf_channel = get_int(&mut env, store, KEY_RF_CHANNEL)?;
        if rf_channel == INT_ABSENT {
            return Ok(None);
        }
        let phone_label = get_string(&mut env, store, KEY_PHONE_LABEL)?.unwrap_or_default();

        let gw_pk: [u8; 32] = gw_public_key.try_into().map_err(|_| {
            PairingError::StoreLoadFailed("gw_public_key: expected 32 bytes".into())
        })?;
        let gw_id: [u8; 16] = gateway_id
            .try_into()
            .map_err(|_| PairingError::StoreLoadFailed("gateway_id: expected 16 bytes".into()))?;
        let psk: [u8; 32] = phone_psk
            .try_into()
            .map_err(|_| PairingError::StoreLoadFailed("phone_psk: expected 32 bytes".into()))?;

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
    }

    fn clear(&mut self) -> Result<(), PairingError> {
        let mut env = self.vm.attach_current_thread().map_err(store_jni_err)?;
        env.call_method(self.store.as_obj(), "clear", "()V", &[])
            .map_err(|e| store_jni_exception(&mut env, "clear", e))?;
        debug!("pairing store cleared");
        Ok(())
    }

    fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, PairingError> {
        let mut env = self.vm.attach_current_thread().map_err(store_jni_err)?;
        let store = self.store.as_obj();

        let gw_public_key = match get_bytes(&mut env, store, KEY_GW_PUBLIC_KEY)? {
            Some(b) => b,
            None => return Ok(None),
        };
        let gateway_id = match get_bytes(&mut env, store, KEY_GATEWAY_ID)? {
            Some(b) => b,
            None => return Ok(None),
        };

        let gw_pk: [u8; 32] = gw_public_key.try_into().map_err(|_| {
            PairingError::StoreLoadFailed("gw_public_key: expected 32 bytes".into())
        })?;
        let gw_id: [u8; 16] = gateway_id
            .try_into()
            .map_err(|_| PairingError::StoreLoadFailed("gateway_id: expected 16 bytes".into()))?;

        Ok(Some(GatewayIdentity {
            public_key: gw_pk,
            gateway_id: gw_id,
        }))
    }

    fn save_gateway_identity(&mut self, identity: &GatewayIdentity) -> Result<(), PairingError> {
        let mut env = self.vm.attach_current_thread().map_err(store_jni_err)?;
        let store = self.store.as_obj();

        put_bytes(&mut env, store, KEY_GW_PUBLIC_KEY, &identity.public_key)?;
        put_bytes(&mut env, store, KEY_GATEWAY_ID, &identity.gateway_id)?;

        debug!("gateway identity saved");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// JNI helpers — SecureStore method wrappers
// ---------------------------------------------------------------------------

fn put_bytes(
    env: &mut JNIEnv<'_>,
    store: &JObject<'_>,
    key: &str,
    value: &[u8],
) -> Result<(), PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let val_arr = env.byte_array_from_slice(value).map_err(store_jni_err)?;
    env.call_method(
        store,
        "putBytes",
        "(Ljava/lang/String;[B)V",
        &[JValue::Object(&key_jstr), JValue::Object(&val_arr)],
    )
    .map_err(|e| store_jni_exception(env, "putBytes", e))?;
    Ok(())
}

fn get_bytes(
    env: &mut JNIEnv<'_>,
    store: &JObject<'_>,
    key: &str,
) -> Result<Option<Vec<u8>>, PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let result = env
        .call_method(
            store,
            "getBytes",
            "(Ljava/lang/String;)[B",
            &[JValue::Object(&key_jstr)],
        )
        .map_err(|e| store_jni_exception(env, "getBytes", e))?
        .l()
        .map_err(store_jni_err)?;

    if result.is_null() {
        return Ok(None);
    }

    let bytes = env
        .convert_byte_array(JByteArray::from(result))
        .map_err(store_jni_err)?;
    Ok(Some(bytes))
}

fn put_int(
    env: &mut JNIEnv<'_>,
    store: &JObject<'_>,
    key: &str,
    value: i32,
) -> Result<(), PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    env.call_method(
        store,
        "putInt",
        "(Ljava/lang/String;I)V",
        &[JValue::Object(&key_jstr), JValue::Int(value)],
    )
    .map_err(|e| store_jni_exception(env, "putInt", e))?;
    Ok(())
}

fn get_int(env: &mut JNIEnv<'_>, store: &JObject<'_>, key: &str) -> Result<i32, PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let val = env
        .call_method(
            store,
            "getInt",
            "(Ljava/lang/String;I)I",
            &[JValue::Object(&key_jstr), JValue::Int(INT_ABSENT)],
        )
        .map_err(|e| store_jni_exception(env, "getInt", e))?
        .i()
        .map_err(store_jni_err)?;
    Ok(val)
}

fn put_string(
    env: &mut JNIEnv<'_>,
    store: &JObject<'_>,
    key: &str,
    value: &str,
) -> Result<(), PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let val_jstr = env.new_string(value).map_err(store_jni_err)?;
    env.call_method(
        store,
        "putString",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[JValue::Object(&key_jstr), JValue::Object(&val_jstr)],
    )
    .map_err(|e| store_jni_exception(env, "putString", e))?;
    Ok(())
}

fn get_string(
    env: &mut JNIEnv<'_>,
    store: &JObject<'_>,
    key: &str,
) -> Result<Option<String>, PairingError> {
    let key_jstr = env.new_string(key).map_err(store_jni_err)?;
    let result = env
        .call_method(
            store,
            "getString",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&key_jstr)],
        )
        .map_err(|e| store_jni_exception(env, "getString", e))?
        .l()
        .map_err(store_jni_err)?;

    if result.is_null() {
        return Ok(None);
    }

    let s: String = env
        .get_string(&JString::from(result))
        .map_err(store_jni_err)?
        .into();
    Ok(Some(s))
}

// ---------------------------------------------------------------------------
/// Get the Application context via `ActivityThread.currentApplication()`.
fn get_app_context<'a>(env: &mut JNIEnv<'a>) -> Result<JObject<'a>, PairingError> {
    let activity_thread = env
        .find_class("android/app/ActivityThread")
        .map_err(store_jni_err)?;
    let app = env
        .call_static_method(
            activity_thread,
            "currentApplication",
            "()Landroid/app/Application;",
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

fn store_jni_exception(
    env: &mut JNIEnv<'_>,
    context: &str,
    err: jni::errors::Error,
) -> PairingError {
    let detail = match err {
        jni::errors::Error::JavaException => {
            jni_exception_msg(env).unwrap_or_else(|| "(unknown Java exception)".into())
        }
        other => other.to_string(),
    };
    PairingError::StoreSaveFailed(format!("{context}: {detail}"))
}

fn jni_exception_msg(env: &mut JNIEnv<'_>) -> Option<String> {
    if !env.exception_check().ok()? {
        return None;
    }
    let exc = env.exception_occurred().ok()?;
    env.exception_clear().ok()?;

    let msg_obj = env
        .call_method(&exc, "getMessage", "()Ljava/lang/String;", &[])
        .ok()?
        .l()
        .ok()?;
    if msg_obj.is_null() {
        return Some("(no message)".into());
    }
    let msg = env
        .get_string(&JString::from(msg_obj))
        .ok()?
        .to_string_lossy()
        .into_owned();
    Some(msg)
}
