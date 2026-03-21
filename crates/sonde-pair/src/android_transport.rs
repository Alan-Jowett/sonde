// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Android BLE transport via JNI bridge to the Android `BluetoothGatt` API.
//!
//! Requires the companion Java class [`io.sonde.pair.BleHelper`] to be
//! included in the consuming Android app's classpath.  The Java source
//! ships in `crates/sonde-pair/java/io/sonde/pair/BleHelper.java`.
//!
//! # Android permissions
//!
//! The consuming app **must** declare:
//!
//! - `BLUETOOTH_SCAN` (API 31+)
//! - `BLUETOOTH_CONNECT` (API 31+)
//! - `ACCESS_FINE_LOCATION` (for BLE scanning)
//!
//! # Runtime requirements
//!
//! This module requires a [tokio] runtime with the `rt` feature enabled
//! (`spawn_blocking` is used to keep JNI calls off the async executor).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use jni::objects::{JByteArray, JClass, JObject, JObjectArray, JString, JValue};
use jni::refs::Global;
use jni::{jni_sig, jni_str, Env, JavaVM};
use tracing::debug;

use crate::error::PairingError;
use crate::transport::BleTransport;
use crate::types::ScannedDevice;

/// BLE connection + bonding timeout in milliseconds (PT-1002).
///
/// This budget covers GATT connect, LESC Numeric Comparison bonding
/// (which requires operator confirmation on the gateway side), MTU
/// negotiation, and service discovery.
const CONNECT_TIMEOUT_MS: i64 = 10_000;

/// Default write-with-response timeout.
const WRITE_TIMEOUT_MS: i64 = 5_000;

/// Cached JavaVM for creating transports on demand (set in `JNI_OnLoad`).
static CACHED_VM: OnceLock<JavaVM> = OnceLock::new();

/// Cached `BleHelper` class ref, resolved on the main thread which has
/// the application classloader.  Native threads attached via
/// `attach_current_thread()` only see the system classloader, so
/// `find_class()` for app-defined classes fails on them.
static CACHED_HELPER_CLASS: OnceLock<Global<JClass<'static>>> = OnceLock::new();

/// Android BLE transport backed by the companion `BleHelper` Java class.
///
/// Implements [`BleTransport`] for Android via JNI.  Each blocking JNI
/// call is dispatched to [`tokio::task::spawn_blocking`] so the async
/// executor is never blocked.
///
/// # Construction
///
/// ```rust,ignore
/// // Inside a JNI callback or Tauri command handler:
/// let transport = AndroidBleTransport::new(&mut env, &activity_context)?;
/// ```
pub struct AndroidBleTransport {
    inner: Arc<JniState>,
}

struct JniState {
    vm: JavaVM,
    helper: Global<JObject<'static>>,
}

// SAFETY: JavaVM is Send+Sync and GlobalRef is Send.  We only access the
// helper through properly-attached JNIEnv handles obtained from JavaVM.
unsafe impl Send for JniState {}
unsafe impl Sync for JniState {}

impl AndroidBleTransport {
    /// Create a new transport, initialising the Java `BleHelper` via JNI.
    ///
    /// [`cache_helper_class()`] **must** have been called first (typically
    /// from `JNI_OnLoad`) to resolve the `BleHelper` class on a thread
    /// with the application classloader.
    ///
    /// `context` must be an Android `Context` (e.g. `Activity` or
    /// `Application`).  The helper takes `getApplicationContext()` so the
    /// caller does not need to worry about lifecycle.
    pub fn new(env: &mut Env<'_>, context: &JObject<'_>) -> Result<Self, PairingError> {
        let vm = env.get_java_vm().map_err(jni_err)?;

        // Use the cached class ref (resolved on the main thread which has the
        // application classloader).  Natively-attached threads only see the
        // system classloader, so find_class() for app classes would fail.
        let cached = CACHED_HELPER_CLASS.get().ok_or_else(|| {
            PairingError::ConnectionFailed(
                "BleHelper class not cached — call cache_helper_class() \
                 from JNI_OnLoad before using the transport"
                    .into(),
            )
        })?;

        let helper = env
            .new_object(
                &**cached,
                jni_sig!("(Landroid/content/Context;)V"),
                &[JValue::Object(context)],
            )
            .map_err(|e| jni_exception_or(env, "BleHelper()", e))?;

        let helper_ref = env.new_global_ref(&helper).map_err(jni_err)?;

        debug!("AndroidBleTransport initialised");

        Ok(Self {
            inner: Arc::new(JniState {
                vm,
                helper: helper_ref,
            }),
        })
    }

    /// Cache the `JavaVM` for later use by [`from_cached_vm()`].
    /// Typically called from `JNI_OnLoad`.
    pub fn cache_vm(vm: JavaVM) {
        let _ = CACHED_VM.set(vm);
        debug!("AndroidBleTransport: JavaVM cached");
    }

    /// Resolve and cache the `BleHelper` class reference.
    ///
    /// **Must** be called from a thread that has the application classloader
    /// (e.g. the main thread inside `JNI_OnLoad`).  Natively-attached
    /// threads only see the system classloader, so `find_class()` for
    /// app-defined classes would fail on them.
    pub fn cache_helper_class(env: &mut Env<'_>) -> Result<(), PairingError> {
        let cls = env
            .find_class(jni_str!("io/sonde/pair/BleHelper"))
            .map_err(|e| {
                PairingError::ConnectionFailed(format!(
                    "BleHelper class not found — ensure io.sonde.pair.BleHelper \
                 is compiled into the APK: {e}"
                ))
            })?;
        let global = env.new_global_ref(cls).map_err(jni_err)?;
        let _ = CACHED_HELPER_CLASS.set(global);
        debug!("AndroidBleTransport: BleHelper class cached");
        Ok(())
    }

    /// Create a new transport from the cached `JavaVM`.
    /// [`cache_vm()`] must have been called first (typically from
    /// `JNI_OnLoad`).  The application context is obtained via
    /// `ActivityThread.currentApplication()`.
    pub fn from_cached_vm() -> Result<Self, PairingError> {
        let vm = CACHED_VM.get().ok_or_else(|| {
            PairingError::ConnectionFailed("JavaVM not cached — call cache_vm() first".into())
        })?;
        vm.attach_current_thread(|env| {
            let context = get_application_context(env)?;
            Self::new(env, &context)
        })
    }

    /// Pause BLE operations (e.g. on Android `onPause`).
    ///
    /// Stops any active scan.  Call from the host Activity/Fragment
    /// lifecycle handler.
    pub fn on_pause(&self) -> Result<(), PairingError> {
        self.inner.vm.attach_current_thread(|env| {
            env.call_method(
                self.inner.helper.as_obj(),
                jni_str!("stopScan"),
                jni_sig!("()V"),
                &[],
            )
            .map_err(|e| jni_exception_or(env, "stopScan", e))?;
            Ok(())
        })
    }
}

impl BleTransport for AndroidBleTransport {
    fn start_scan(
        &mut self,
        service_uuids: &[u128],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let inner = self.inner.clone();
        let uuids: Vec<String> = service_uuids.iter().map(|u| uuid_to_string(*u)).collect();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                inner.vm.attach_current_thread(|env| {
                    for uuid_string in &uuids {
                        let uuid_jstr = env.new_string(uuid_string).map_err(jni_err)?;
                        env.call_method(
                            inner.helper.as_obj(),
                            jni_str!("startScan"),
                            jni_sig!("(Ljava/lang/String;)V"),
                            &[JValue::Object(uuid_jstr.as_ref())],
                        )
                        .map_err(|e| jni_exception_or(env, "startScan", e))?;
                    }
                    debug!(?uuids, "BLE scan started");
                    Ok(())
                })
            })
            .await
            .map_err(join_err)?
        })
    }

    fn stop_scan(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                inner.vm.attach_current_thread(|env| {
                    env.call_method(
                        inner.helper.as_obj(),
                        jni_str!("stopScan"),
                        jni_sig!("()V"),
                        &[],
                    )
                    .map_err(|e| jni_exception_or(env, "stopScan", e))?;
                    debug!("BLE scan stopped");
                    Ok(())
                })
            })
            .await
            .map_err(join_err)?
        })
    }

    fn get_discovered_devices(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ScannedDevice>, PairingError>> + '_>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                inner.vm.attach_current_thread(|env| {
                    let helper = inner.helper.as_obj();

                    let count = env
                        .call_method(
                            helper,
                            jni_str!("getDiscoveredDeviceCount"),
                            jni_sig!("()I"),
                            &[],
                        )
                        .map_err(|e| jni_exception_or(env, "getDiscoveredDeviceCount", e))?
                        .i()
                        .map_err(jni_err)?;

                    let mut devices = Vec::with_capacity(count as usize);

                    for i in 0..count {
                        let idx = JValue::Int(i);

                        // Name
                        let name_obj = env
                            .call_method(
                                helper,
                                jni_str!("getDeviceName"),
                                jni_sig!("(I)Ljava/lang/String;"),
                                &[idx],
                            )
                            .map_err(|e| jni_exception_or(env, "getDeviceName", e))?
                            .l()
                            .map_err(jni_err)?;
                        let name: String = unsafe { JString::from_raw(env, name_obj.into_raw()) }
                            .try_to_string(env)
                            .map_err(jni_err)?;

                        // Address (6 bytes)
                        let addr_obj = env
                            .call_method(
                                helper,
                                jni_str!("getDeviceAddress"),
                                jni_sig!("(I)[B"),
                                &[idx],
                            )
                            .map_err(|e| jni_exception_or(env, "getDeviceAddress", e))?
                            .l()
                            .map_err(jni_err)?;
                        let addr_bytes = env
                            .convert_byte_array(unsafe {
                                JByteArray::from_raw(env, addr_obj.into_raw())
                            })
                            .map_err(jni_err)?;
                        let address: [u8; 6] = addr_bytes.try_into().map_err(|_| {
                            PairingError::ConnectionFailed("bad address length".into())
                        })?;

                        // RSSI
                        let rssi = env
                            .call_method(
                                helper,
                                jni_str!("getDeviceRssi"),
                                jni_sig!("(I)I"),
                                &[idx],
                            )
                            .map_err(|e| jni_exception_or(env, "getDeviceRssi", e))?
                            .i()
                            .map_err(jni_err)? as i8;

                        // Service UUIDs
                        let uuids_obj = env
                            .call_method(
                                helper,
                                jni_str!("getDeviceServiceUuids"),
                                jni_sig!("(I)[Ljava/lang/String;"),
                                &[idx],
                            )
                            .map_err(|e| jni_exception_or(env, "getDeviceServiceUuids", e))?
                            .l()
                            .map_err(jni_err)?;
                        // SAFETY: getDeviceServiceUuids returns String[].
                        let uuids_arr: JObjectArray<JString> =
                            unsafe { JObjectArray::<JString>::from_raw(env, uuids_obj.into_raw()) };
                        let uuid_count = uuids_arr.len(env).map_err(jni_err)?;
                        let mut service_uuids = Vec::with_capacity(uuid_count as usize);
                        for j in 0..uuid_count {
                            let uuid_obj = uuids_arr.get_element(env, j).map_err(jni_err)?;
                            let uuid_str: String = uuid_obj.try_to_string(env).map_err(jni_err)?;
                            if let Some(val) = parse_uuid_string(&uuid_str) {
                                service_uuids.push(val);
                            }
                        }

                        devices.push(ScannedDevice {
                            name,
                            address,
                            rssi,
                            service_uuids,
                        });
                    }

                    Ok(devices)
                })
            })
            .await
            .map_err(join_err)?
        })
    }

    fn connect(
        &mut self,
        address: &[u8; 6],
    ) -> Pin<Box<dyn Future<Output = Result<u16, PairingError>> + '_>> {
        let inner = self.inner.clone();
        let addr = *address;
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                inner.vm.attach_current_thread(|env| {
                    let addr_arr = env.byte_array_from_slice(&addr).map_err(jni_err)?;
                    let mtu = env
                        .call_method(
                            inner.helper.as_obj(),
                            jni_str!("connect"),
                            jni_sig!("([BJ)I"),
                            &[
                                JValue::Object(addr_arr.as_ref()),
                                JValue::Long(CONNECT_TIMEOUT_MS),
                            ],
                        )
                        .map_err(|e| jni_exception_or(env, "connect", e))?
                        .i()
                        .map_err(jni_err)?;

                    debug!(address = ?addr, mtu, "connected");
                    Ok(mtu as u16)
                })
            })
            .await
            .map_err(join_err)?
        })
    }

    fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                inner.vm.attach_current_thread(|env| {
                    env.call_method(
                        inner.helper.as_obj(),
                        jni_str!("disconnect"),
                        jni_sig!("()V"),
                        &[],
                    )
                    .map_err(|e| jni_exception_or(env, "disconnect", e))?;
                    debug!("disconnected");
                    Ok(())
                })
            })
            .await
            .map_err(join_err)?
        })
    }

    fn write_characteristic(
        &mut self,
        service: u128,
        characteristic: u128,
        data: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let inner = self.inner.clone();
        let svc_str = uuid_to_string(service);
        let chr_str = uuid_to_string(characteristic);
        let data = data.to_vec();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                inner.vm.attach_current_thread(|env| {
                    let svc_jstr = env.new_string(&svc_str).map_err(jni_err)?;
                    let chr_jstr = env.new_string(&chr_str).map_err(jni_err)?;
                    let data_arr = env.byte_array_from_slice(&data).map_err(jni_err)?;

                    env.call_method(
                        inner.helper.as_obj(),
                        jni_str!("writeCharacteristic"),
                        jni_sig!("(Ljava/lang/String;Ljava/lang/String;[BJ)V"),
                        &[
                            JValue::Object(svc_jstr.as_ref()),
                            JValue::Object(chr_jstr.as_ref()),
                            JValue::Object(data_arr.as_ref()),
                            JValue::Long(WRITE_TIMEOUT_MS),
                        ],
                    )
                    .map_err(|e| jni_exception_or(env, "writeCharacteristic", e))?;

                    debug!(characteristic = %chr_str, len = data.len(), "GATT write complete");
                    Ok(())
                })
            })
            .await
            .map_err(join_err)?
        })
    }

    fn read_indication(
        &mut self,
        service: u128,
        characteristic: u128,
        timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PairingError>> + '_>> {
        let inner = self.inner.clone();
        let svc_str = uuid_to_string(service);
        let chr_str = uuid_to_string(characteristic);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                inner.vm.attach_current_thread(|env| {
                    let svc_jstr = env.new_string(&svc_str).map_err(jni_err)?;
                    let chr_jstr = env.new_string(&chr_str).map_err(jni_err)?;

                    let result = env
                        .call_method(
                            inner.helper.as_obj(),
                            jni_str!("readIndication"),
                            jni_sig!("(Ljava/lang/String;Ljava/lang/String;J)[B"),
                            &[
                                JValue::Object(svc_jstr.as_ref()),
                                JValue::Object(chr_jstr.as_ref()),
                                JValue::Long(timeout_ms as i64),
                            ],
                        )
                        .map_err(|e| {
                            let pe = jni_exception_or(env, "readIndication", e);
                            if let PairingError::ConnectionFailed(ref msg) = pe {
                                if msg.contains("indication timeout") {
                                    return PairingError::IndicationTimeout;
                                }
                            }
                            pe
                        })?
                        .l()
                        .map_err(jni_err)?;

                    let bytes = env
                        .convert_byte_array(unsafe { JByteArray::from_raw(env, result.into_raw()) })
                        .map_err(jni_err)?;
                    Ok(bytes)
                })
            })
            .await
            .map_err(join_err)?
        })
    }
}

impl Drop for AndroidBleTransport {
    fn drop(&mut self) {
        // Best-effort disconnect — attach to JVM if possible.
        let _ = self.inner.vm.attach_current_thread(|env| {
            let _ = env.call_method(
                self.inner.helper.as_obj(),
                jni_str!("disconnect"),
                jni_sig!("()V"),
                &[],
            );
            Ok::<(), PairingError>(())
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a `u128` UUID to the standard string format expected by Java's
/// `UUID.fromString`.
fn uuid_to_string(uuid: u128) -> String {
    let b = uuid.to_be_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15],
    )
}

/// Get the Application context via `ActivityThread.currentApplication()`.
/// Works from any JNI-attached thread without needing an Activity reference.
fn get_application_context<'a>(env: &mut Env<'a>) -> Result<JObject<'a>, PairingError> {
    let activity_thread = env
        .find_class(jni_str!("android/app/ActivityThread"))
        .map_err(jni_err)?;
    let app = env
        .call_static_method(
            activity_thread,
            jni_str!("currentApplication"),
            jni_sig!("()Landroid/app/Application;"),
            &[],
        )
        .and_then(|v| v.l())
        .map_err(|e| jni_exception_or(env, "currentApplication", e))?;
    if app.is_null() {
        return Err(PairingError::ConnectionFailed(
            "ActivityThread.currentApplication() returned null".into(),
        ));
    }
    Ok(app)
}

/// Map a plain JNI error (not a Java exception) to [`PairingError`].
fn jni_err(e: jni::errors::Error) -> PairingError {
    PairingError::ConnectionFailed(format!("JNI error: {e}"))
}

/// Map a [`tokio::task::JoinError`] to [`PairingError`].
fn join_err(e: tokio::task::JoinError) -> PairingError {
    PairingError::ConnectionFailed(format!("blocking task panicked: {e}"))
}

/// Attempt to extract the Java exception message; fall back to the raw
/// JNI error string if the exception cannot be read.
fn jni_exception_or(env: &mut Env<'_>, context: &str, err: jni::errors::Error) -> PairingError {
    let detail = match err {
        jni::errors::Error::JavaException => {
            get_exception_message(env).unwrap_or_else(|| "(unknown Java exception)".into())
        }
        other => other.to_string(),
    };
    PairingError::ConnectionFailed(format!("{context}: {detail}"))
}

/// Read and clear the pending Java exception message, if any.
fn get_exception_message(env: &mut Env<'_>) -> Option<String> {
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

/// Parse a UUID string (e.g. `"0000fe60-0000-1000-8000-00805f9b34fb"`) into
/// a `u128`.  Returns `None` on malformed input.
fn parse_uuid_string(s: &str) -> Option<u128> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_to_string_gateway_service() {
        let s = uuid_to_string(crate::types::GATEWAY_SERVICE_UUID);
        assert_eq!(s, "0000fe60-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn uuid_to_string_node_command() {
        let s = uuid_to_string(crate::types::NODE_COMMAND_UUID);
        assert_eq!(s, "0000fe51-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn uuid_round_trip() {
        let uuid = crate::types::GATEWAY_SERVICE_UUID;
        let s = uuid_to_string(uuid);
        assert_eq!(parse_uuid_string(&s), Some(uuid));
    }

    #[test]
    fn parse_uuid_string_invalid() {
        assert_eq!(parse_uuid_string("not-a-uuid"), None);
        assert_eq!(parse_uuid_string(""), None);
    }
}
