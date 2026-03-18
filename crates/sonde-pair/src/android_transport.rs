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
use std::sync::Arc;

use jni::objects::{GlobalRef, JByteArray, JObject, JString, JValue};
use jni::JNIEnv;
use jni::JavaVM;
use tracing::debug;

use crate::error::PairingError;
use crate::transport::BleTransport;
use crate::types::ScannedDevice;

/// BLE connection timeout in milliseconds (PT-1002).
const CONNECT_TIMEOUT_MS: i64 = 10_000;

/// Default write-with-response timeout.
const WRITE_TIMEOUT_MS: i64 = 5_000;

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
    helper: GlobalRef,
}

// SAFETY: JavaVM is Send+Sync and GlobalRef is Send.  We only access the
// helper through properly-attached JNIEnv handles obtained from JavaVM.
unsafe impl Send for JniState {}
unsafe impl Sync for JniState {}

impl AndroidBleTransport {
    /// Create a new transport, initialising the Java `BleHelper` via JNI.
    ///
    /// `context` must be an Android `Context` (e.g. `Activity` or
    /// `Application`).  The helper takes `getApplicationContext()` so the
    /// caller does not need to worry about lifecycle.
    pub fn new(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Result<Self, PairingError> {
        let vm = env.get_java_vm().map_err(jni_err)?;

        let helper_class = env.find_class("io/sonde/pair/BleHelper").map_err(|e| {
            PairingError::ConnectionFailed(format!(
                "BleHelper class not found — ensure io.sonde.pair.BleHelper \
                 is compiled into the APK: {e}"
            ))
        })?;

        let helper = env
            .new_object(
                helper_class,
                "(Landroid/content/Context;)V",
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

    /// Pause BLE operations (e.g. on Android `onPause`).
    ///
    /// Stops any active scan.  Call from the host Activity/Fragment
    /// lifecycle handler.
    pub fn on_pause(&self) -> Result<(), PairingError> {
        let mut env = self.inner.vm.attach_current_thread().map_err(jni_err)?;
        env.call_method(self.inner.helper.as_obj(), "stopScan", "()V", &[])
            .map_err(|e| jni_exception_or(&mut env, "stopScan", e))?;
        Ok(())
    }
}

impl BleTransport for AndroidBleTransport {
    fn start_scan(
        &mut self,
        service_uuids: &[u128],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let inner = self.inner.clone();
        // Android BLE scan uses the first UUID; filtering happens in refresh().
        let uuid_string = if service_uuids.is_empty() {
            String::new()
        } else {
            uuid_to_string(service_uuids[0])
        };
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut env = inner.vm.attach_current_thread().map_err(jni_err)?;
                let uuid_jstr = env.new_string(&uuid_string).map_err(jni_err)?;
                env.call_method(
                    inner.helper.as_obj(),
                    "startScan",
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(&uuid_jstr)],
                )
                .map_err(|e| jni_exception_or(&mut env, "startScan", e))?;
                debug!(service = %uuid_string, "BLE scan started");
                Ok(())
            })
            .await
            .map_err(join_err)?
        })
    }

    fn stop_scan(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut env = inner.vm.attach_current_thread().map_err(jni_err)?;
                env.call_method(inner.helper.as_obj(), "stopScan", "()V", &[])
                    .map_err(|e| jni_exception_or(&mut env, "stopScan", e))?;
                debug!("BLE scan stopped");
                Ok(())
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
                let mut env = inner.vm.attach_current_thread().map_err(jni_err)?;
                let helper = inner.helper.as_obj();

                let count = env
                    .call_method(helper, "getDiscoveredDeviceCount", "()I", &[])
                    .map_err(|e| jni_exception_or(&mut env, "getDiscoveredDeviceCount", e))?
                    .i()
                    .map_err(jni_err)?;

                let mut devices = Vec::with_capacity(count as usize);

                for i in 0..count {
                    let idx = JValue::Int(i);

                    // Name
                    let name_obj = env
                        .call_method(helper, "getDeviceName", "(I)Ljava/lang/String;", &[idx])
                        .map_err(|e| jni_exception_or(&mut env, "getDeviceName", e))?
                        .l()
                        .map_err(jni_err)?;
                    let name: String = env
                        .get_string(&JString::from(name_obj))
                        .map_err(jni_err)?
                        .into();

                    // Address (6 bytes)
                    let addr_obj = env
                        .call_method(helper, "getDeviceAddress", "(I)[B", &[idx])
                        .map_err(|e| jni_exception_or(&mut env, "getDeviceAddress", e))?
                        .l()
                        .map_err(jni_err)?;
                    let addr_bytes = env
                        .convert_byte_array(JByteArray::from(addr_obj))
                        .map_err(jni_err)?;
                    let address: [u8; 6] = addr_bytes
                        .try_into()
                        .map_err(|_| PairingError::ConnectionFailed("bad address length".into()))?;

                    // RSSI
                    let rssi = env
                        .call_method(helper, "getDeviceRssi", "(I)I", &[idx])
                        .map_err(|e| jni_exception_or(&mut env, "getDeviceRssi", e))?
                        .i()
                        .map_err(jni_err)? as i8;

                    // Service UUIDs
                    let uuids_obj = env
                        .call_method(
                            helper,
                            "getDeviceServiceUuids",
                            "(I)[Ljava/lang/String;",
                            &[idx],
                        )
                        .map_err(|e| jni_exception_or(&mut env, "getDeviceServiceUuids", e))?
                        .l()
                        .map_err(jni_err)?;
                    let uuids_arr = jni::objects::JObjectArray::from(uuids_obj);
                    let uuid_count = env.get_array_length(&uuids_arr).map_err(jni_err)?;
                    let mut service_uuids = Vec::with_capacity(uuid_count as usize);
                    for j in 0..uuid_count {
                        let uuid_obj = env
                            .get_object_array_element(&uuids_arr, j)
                            .map_err(jni_err)?;
                        let uuid_str: String = env
                            .get_string(&JString::from(uuid_obj))
                            .map_err(jni_err)?
                            .into();
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
                let mut env = inner.vm.attach_current_thread().map_err(jni_err)?;
                let addr_arr = env.byte_array_from_slice(&addr).map_err(jni_err)?;
                let mtu = env
                    .call_method(
                        inner.helper.as_obj(),
                        "connect",
                        "([BJ)I",
                        &[JValue::Object(&addr_arr), JValue::Long(CONNECT_TIMEOUT_MS)],
                    )
                    .map_err(|e| jni_exception_or(&mut env, "connect", e))?
                    .i()
                    .map_err(jni_err)?;

                debug!(address = ?addr, mtu, "connected");
                Ok(mtu as u16)
            })
            .await
            .map_err(join_err)?
        })
    }

    fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut env = inner.vm.attach_current_thread().map_err(jni_err)?;
                env.call_method(inner.helper.as_obj(), "disconnect", "()V", &[])
                    .map_err(|e| jni_exception_or(&mut env, "disconnect", e))?;
                debug!("disconnected");
                Ok(())
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
                let mut env = inner.vm.attach_current_thread().map_err(jni_err)?;
                let svc_jstr = env.new_string(&svc_str).map_err(jni_err)?;
                let chr_jstr = env.new_string(&chr_str).map_err(jni_err)?;
                let data_arr = env.byte_array_from_slice(&data).map_err(jni_err)?;

                env.call_method(
                    inner.helper.as_obj(),
                    "writeCharacteristic",
                    "(Ljava/lang/String;Ljava/lang/String;[BJ)V",
                    &[
                        JValue::Object(&svc_jstr),
                        JValue::Object(&chr_jstr),
                        JValue::Object(&data_arr),
                        JValue::Long(WRITE_TIMEOUT_MS),
                    ],
                )
                .map_err(|e| jni_exception_or(&mut env, "writeCharacteristic", e))?;

                debug!(characteristic = %chr_str, len = data.len(), "GATT write complete");
                Ok(())
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
                let mut env = inner.vm.attach_current_thread().map_err(jni_err)?;
                let svc_jstr = env.new_string(&svc_str).map_err(jni_err)?;
                let chr_jstr = env.new_string(&chr_str).map_err(jni_err)?;

                let result = env
                    .call_method(
                        inner.helper.as_obj(),
                        "readIndication",
                        "(Ljava/lang/String;Ljava/lang/String;J)[B",
                        &[
                            JValue::Object(&svc_jstr),
                            JValue::Object(&chr_jstr),
                            JValue::Long(timeout_ms as i64),
                        ],
                    )
                    .map_err(|e| {
                        let pe = jni_exception_or(&mut env, "readIndication", e);
                        // Map Java "indication timeout" to the dedicated error variant
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
                    .convert_byte_array(JByteArray::from(result))
                    .map_err(jni_err)?;
                Ok(bytes)
            })
            .await
            .map_err(join_err)?
        })
    }
}

impl Drop for AndroidBleTransport {
    fn drop(&mut self) {
        // Best-effort disconnect — attach to JVM if possible.
        if let Ok(mut env) = self.inner.vm.attach_current_thread() {
            let _ = env.call_method(self.inner.helper.as_obj(), "disconnect", "()V", &[]);
        }
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
fn jni_exception_or(env: &mut JNIEnv<'_>, context: &str, err: jni::errors::Error) -> PairingError {
    let detail = match err {
        jni::errors::Error::JavaException => {
            get_exception_message(env).unwrap_or_else(|| "(unknown Java exception)".into())
        }
        other => other.to_string(),
    };
    PairingError::ConnectionFailed(format!("{context}: {detail}"))
}

/// Read and clear the pending Java exception message, if any.
fn get_exception_message(env: &mut JNIEnv<'_>) -> Option<String> {
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
