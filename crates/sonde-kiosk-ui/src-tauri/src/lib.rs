// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Tauri v2 backend for the Sonde kiosk dashboard app.
//!
//! This tranche persists the imported kiosk environment JSON, manages the kiosk
//! certificate lifecycle, and exposes the telemetry/auth seams used by the
//! frontend shell.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use p256::pkcs8::DecodePrivateKey;
use p256::pkcs8::EncodePublicKey;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
#[cfg(target_os = "android")]
use std::sync::OnceLock;
use tauri::Manager;
use time::{Duration as TimeDuration, OffsetDateTime};
use tracing::warn;
use uuid::Uuid;
use x509_cert::der::{Decode, Encode};
use x509_cert::Certificate;

#[cfg(target_os = "android")]
use android_native_keyring_store::Store as AndroidKeyringStore;
#[cfg(target_os = "android")]
use keyring_core::Entry;

const ENVIRONMENT_FILE_NAME: &str = "environment.json";
const TELEMETRY_CACHE_FILE_NAME: &str = "telemetry-cache.json";
const IDENTITY_STATE_FILE_NAME: &str = "identity-state.json";
const PRIVATE_KEY_FILE_NAME: &str = "kiosk-private-key.pem";
const SHARED_DASHBOARD_RUNTIME_SOURCE: &str =
    include_str!("../../../../deploy/web-ui/dashboard-runtime.js");
const ACTUAL_STATE_TABLE_NAME: &str = "actualstate";
const SENSOR_DATA_TABLE_NAME: &str = "sensordata";
const AZURE_TABLES_API_VERSION: &str = "2019-02-02";
const STORAGE_TOKEN_SCOPE: &str = "https://storage.azure.com/.default";
// The setup public-client flow patches `keyCredentials` on the shared runtime
// app registration, so it requests the delegated Graph scope set provisioned
// for kiosk certificate lifecycle operations.
const GRAPH_SCOPE: &str = "Application.ReadWrite.All offline_access openid profile";
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const CLIENT_ASSERTION_LIFETIME_SECS: i64 = 600;
const KIOSK_CERTIFICATE_VALIDITY_DAYS: i64 = 730;
const KIOSK_RENEWAL_THRESHOLD_DAYS: i64 = 30;
const GRAPH_APP_SELECT_QUERY: &str = "$select=id,appId,keyCredentials";
const MAX_TABLE_QUERY_PAGES: usize = 10;
const SENSOR_DATA_TOP_PER_PAGE: usize = 1000;
const JS_MAX_SAFE_INTEGER_F64: f64 = 9_007_199_254_740_991.0;
#[cfg(target_os = "android")]
const KEYRING_SERVICE_NAME: &str = "sonde-kiosk-ui";
#[cfg(target_os = "android")]
const KEYRING_USER_NAME: &str = "kiosk-private-key";

struct AppState {
    device_code_sessions: Mutex<HashMap<String, DeviceCodeSession>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DashboardVariableRequest {
    node_id: String,
    reading_type: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct FetchDashboardVariableDataRequest {
    client_id: String,
    tenant_id: String,
    storage_account: String,
    start_ms: i64,
    end_ms: i64,
    variables: Vec<DashboardVariableRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct TelemetryPoint {
    timestamp_ms: i64,
    value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct DashboardVariableSeries {
    node_id: String,
    reading_type: String,
    points: Vec<TelemetryPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct FetchDashboardVariableDataResponse {
    refreshed_at_ms: i64,
    series: Vec<DashboardVariableSeries>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct KioskIdentityStateFile {
    version: u8,
    shared_app_client_id: String,
    tenant_id: String,
    login_endpoint: String,
    setup_client_id: String,
    certificate_pem: String,
    key_id: String,
    certificate_thumbprint: String,
    certificate_display_name: String,
    not_before_ms: i64,
    not_after_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct KioskIdentitySummary {
    shared_app_client_id: String,
    tenant_id: String,
    login_endpoint: String,
    setup_client_id: String,
    key_id: String,
    certificate_thumbprint: String,
    certificate_display_name: String,
    not_before_ms: i64,
    not_after_ms: i64,
    renewal_required: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct StartDeviceCodeSignInRequest {
    purpose: String,
    tenant_id: String,
    login_endpoint: String,
    setup_client_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DeviceCodeSignInSessionResponse {
    session_id: String,
    purpose: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_at_ms: i64,
    poll_interval_seconds: u64,
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PollDeviceCodeSignInRequest {
    session_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PollDeviceCodeSignInResponse {
    status: String,
    poll_interval_seconds: u64,
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct CompleteKioskSetupRequest {
    session_id: String,
    shared_app_client_id: String,
    tenant_id: String,
    login_endpoint: String,
    setup_client_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct KioskSetupResult {
    summary: KioskIdentitySummary,
    message: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RenewKioskCertificateRequest {
    session_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RenewKioskCertificateResult {
    summary: KioskIdentitySummary,
    cleanup_status: String,
    message: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ResetKioskAppRequest {
    session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ResetKioskAppResult {
    remote_cleanup_status: String,
    message: String,
    cleared_key_id: Option<String>,
    cleared_certificate_thumbprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct KioskApplicationSignInResult {
    summary: KioskIdentitySummary,
    message: String,
}

#[derive(Debug, Clone)]
struct DeviceCodeSession {
    purpose: String,
    tenant_id: String,
    login_endpoint: String,
    setup_client_id: String,
    device_code: String,
    expires_at_ms: i64,
    poll_interval_seconds: u64,
    access_token: Option<String>,
}

#[derive(Debug, Clone)]
struct DeviceCodePollResult {
    response: PollDeviceCodeSignInResponse,
    access_token: Option<String>,
    clear_session: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ClientAssertionClaims {
    aud: String,
    iss: String,
    sub: String,
    jti: String,
    nbf: i64,
    iat: i64,
    exp: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OAuthErrorResponse {
    error: String,
    error_description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: i64,
    interval: Option<u64>,
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphApplicationResponse {
    app_id: String,
    #[serde(default)]
    key_credentials: Vec<GraphKeyCredential>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphKeyCredential {
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_key_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    end_date_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    key_id: String,
    start_date_time: String,
    #[serde(rename = "type")]
    credential_type: String,
    usage: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GraphUpdateApplicationRequest {
    key_credentials: Vec<GraphKeyCredential>,
}

#[derive(Debug, Clone)]
struct GeneratedCertificateBundle {
    certificate_pem: String,
    private_key_pem: String,
    certificate_der_base64: String,
    thumbprint: String,
    key_id: String,
    display_name: String,
    not_before_ms: i64,
    not_after_ms: i64,
}

#[derive(Debug, Clone)]
struct RuntimeCredentialState {
    tenant_id: String,
    client_id: String,
    login_endpoint: String,
    certificate_pem: String,
    private_key_pem: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AzureTableQueryResponse<T> {
    value: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
struct ActualStateEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    node_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SensorDataEntity {
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    decoded_readings: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_i64_from_string_or_number"
    )]
    timestamp_ms: Option<i64>,
}

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => Some(value),
        Some(other) => Some(other.to_string()),
    })
}

fn deserialize_optional_i64_from_string_or_number<'de, D>(
    deserializer: D,
) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => value
            .parse::<i64>()
            .map(Some)
            .map_err(serde::de::Error::custom),
        Some(serde_json::Value::Number(value)) => value
            .as_i64()
            .ok_or_else(|| serde::de::Error::custom("expected a signed 64-bit integer"))
            .map(Some),
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected string or number for integer field, got {other}"
        ))),
    }
}

fn app_data_file_path(app: &tauri::AppHandle, file_name: &str) -> Result<PathBuf, String> {
    let mut path = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("failed to resolve kiosk app data directory: {error}"))?;
    path.push(file_name);
    Ok(path)
}

fn environment_file_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app_data_file_path(app, ENVIRONMENT_FILE_NAME)
}

fn telemetry_cache_file_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app_data_file_path(app, TELEMETRY_CACHE_FILE_NAME)
}

fn identity_state_file_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app_data_file_path(app, IDENTITY_STATE_FILE_NAME)
}

fn private_key_file_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app_data_file_path(app, PRIVATE_KEY_FILE_NAME)
}

fn read_optional_file_to_string(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(json) => Ok(Some(json)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "failed to read kiosk app data from {}: {error}",
            path.display()
        )),
    }
}

fn write_string_to_path(path: &Path, contents: &str) -> Result<(), String> {
    create_parent_dir(path)?;
    fs::write(path, contents).map_err(|error| {
        format!(
            "failed to write kiosk app data to {}: {error}",
            path.display()
        )
    })
}

fn create_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create kiosk app data directory {}: {error}",
                parent.display()
            )
        })?;
    }
    Ok(())
}

fn remove_optional_file_at_path(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to remove kiosk app data at {}: {error}",
            path.display()
        )),
    }
}

#[cfg(unix)]
fn write_private_key_file_securely(path: &Path, contents: &str) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    create_parent_dir(path)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to open private key file {}: {error}",
                path.display()
            )
        })?;
    file.write_all(contents.as_bytes()).map_err(|error| {
        format!(
            "failed to write private key file {}: {error}",
            path.display()
        )
    })?;
    file.flush().map_err(|error| {
        format!(
            "failed to flush private key file {}: {error}",
            path.display()
        )
    })?;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|error| {
        format!(
            "failed to tighten private key permissions on {}: {error}",
            path.display()
        )
    })
}

#[cfg(windows)]
fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
unsafe fn null_terminated_wide_to_string(value: std::ptr::NonNull<u16>) -> String {
    use std::slice;

    let value = value.as_ptr();
    let mut len = 0usize;
    while *value.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(slice::from_raw_parts(value, len))
}

#[cfg(windows)]
fn sid_to_string(sid: windows_sys::Win32::Security::PSID) -> Result<String, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;

    let mut sid_wide = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_wide) } == 0 {
        return Err(format!(
            "failed to convert current user SID to text: {}",
            std::io::Error::last_os_error()
        ));
    }
    let sid_string = unsafe {
        null_terminated_wide_to_string(
            std::ptr::NonNull::new(sid_wide)
                .ok_or_else(|| "ConvertSidToStringSidW returned a null SID string".to_string())?,
        )
    };
    unsafe {
        let _ = LocalFree(sid_wide.cast());
    }
    Ok(sid_string)
}

#[cfg(windows)]
fn current_user_sid_string() -> Result<String, String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(format!(
            "failed to open current process token: {}",
            std::io::Error::last_os_error()
        ));
    }

    let result = {
        let mut required_len = 0;
        let _ = unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required_len)
        };
        if required_len == 0 {
            Err(format!(
                "failed to size current user token information: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            let mut buffer = vec![0u8; required_len as usize];
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    required_len,
                    &mut required_len,
                )
            } == 0
            {
                Err(format!(
                    "failed to read current user token information: {}",
                    std::io::Error::last_os_error()
                ))
            } else {
                let token_user =
                    unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_USER>()) };
                sid_to_string(token_user.User.Sid)
            }
        }
    };

    unsafe {
        let _ = CloseHandle(token);
    }

    result
}

#[cfg(windows)]
fn current_user_private_key_sddl() -> Result<String, String> {
    Ok(format!("D:P(A;;FA;;;{})", current_user_sid_string()?))
}

#[cfg(windows)]
fn write_private_key_file_securely(path: &Path, contents: &str) -> Result<(), String> {
    use std::fs::File;
    use std::io::Write as _;
    use std::os::windows::io::FromRawHandle;

    use windows_sys::Win32::Foundation::{LocalFree, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE,
    };

    create_parent_dir(path)?;
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to replace existing private key file {}: {error}",
                path.display()
            ));
        }
    }
    let private_key_sddl = current_user_private_key_sddl()?;
    let private_key_sddl_wide = wide_null(std::ffi::OsStr::new(&private_key_sddl));
    let mut security_descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            private_key_sddl_wide.as_ptr(),
            1,
            &mut security_descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(format!(
            "failed to build private key security descriptor: {}",
            std::io::Error::last_os_error()
        ));
    }

    let path_wide = wide_null(path.as_os_str());
    let security_attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security_descriptor,
        bInheritHandle: 0,
    };

    let result: Result<(), String> = {
        let handle = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                FILE_GENERIC_WRITE,
                0,
                &security_attributes,
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(format!(
                "failed to open private key file {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ))
        } else {
            let mut file = unsafe { File::from_raw_handle(handle) };
            file.write_all(contents.as_bytes()).map_err(|error| {
                format!(
                    "failed to write private key file {}: {error}",
                    path.display()
                )
            })?;
            file.flush().map_err(|error| {
                format!(
                    "failed to flush private key file {}: {error}",
                    path.display()
                )
            })?;
            Ok(())
        }
    };

    unsafe {
        let _ = LocalFree(security_descriptor.cast());
    }

    if let Err(error) = result {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

#[cfg(all(not(target_os = "android"), not(unix), not(windows)))]
fn write_private_key_file_securely(path: &Path, _contents: &str) -> Result<(), String> {
    Err(format!(
        "secure private key storage is not implemented on this platform for {}",
        path.display()
    ))
}

fn normalize_login_endpoint(value: &str) -> Result<String, String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Login endpoint is required.".into());
    }
    let parsed = reqwest::Url::parse(trimmed)
        .map_err(|error| format!("Login endpoint must be a valid HTTPS authority URL: {error}"))?;
    if parsed.scheme() != "https" {
        return Err("Login endpoint must use HTTPS.".into());
    }
    if parsed.host_str().is_none() {
        return Err("Login endpoint must include a host.".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Login endpoint must not include userinfo.".into());
    }
    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(
            "Login endpoint must be an HTTPS authority URL without path, query, or fragment."
                .into(),
        );
    }
    Ok(trimmed.to_string())
}

fn normalize_guid(value: &str, field_name: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{field_name} is required."));
    }
    Uuid::parse_str(trimmed)
        .map_err(|error| format!("{field_name} must be a valid GUID: {error}"))?;
    Ok(trimmed.to_string())
}

fn normalize_storage_account(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("Storage account is required.".into());
    }
    if !(3..=24).contains(&trimmed.len())
        || !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err("Storage account must be 3–24 lowercase alphanumeric characters.".into());
    }
    Ok(trimmed.to_string())
}

fn normalize_dashboard_variable_request(
    variable: DashboardVariableRequest,
    index: usize,
) -> Result<DashboardVariableRequest, String> {
    let node_id = variable.node_id.trim();
    if node_id.is_empty() {
        return Err(format!(
            "Dashboard variable {} is missing a node ID.",
            index + 1
        ));
    }
    let reading_type = variable.reading_type.trim();
    if reading_type.is_empty() {
        return Err(format!(
            "Dashboard variable {} is missing a reading type.",
            index + 1
        ));
    }
    Ok(DashboardVariableRequest {
        node_id: node_id.to_string(),
        reading_type: reading_type.to_string(),
    })
}

fn normalize_fetch_dashboard_variable_data_request(
    request: FetchDashboardVariableDataRequest,
) -> Result<FetchDashboardVariableDataRequest, String> {
    if request.start_ms < 0 || request.end_ms < 0 {
        return Err("Telemetry refresh time bounds must be non-negative.".into());
    }
    if request.end_ms < request.start_ms {
        return Err(
            "Telemetry refresh end time must be greater than or equal to the start time.".into(),
        );
    }
    if request.variables.is_empty() {
        return Err("Telemetry refresh requires at least one dashboard variable.".into());
    }
    Ok(FetchDashboardVariableDataRequest {
        client_id: normalize_guid(&request.client_id, "Client ID")?,
        tenant_id: normalize_guid(&request.tenant_id, "Tenant ID")?,
        storage_account: normalize_storage_account(&request.storage_account)?,
        start_ms: request.start_ms,
        end_ms: request.end_ms,
        variables: request
            .variables
            .into_iter()
            .enumerate()
            .map(|(index, variable)| normalize_dashboard_variable_request(variable, index))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn renewal_required(not_after_ms: i64, now_ms: i64) -> bool {
    not_after_ms - now_ms <= KIOSK_RENEWAL_THRESHOLD_DAYS * 24 * 60 * 60 * 1000
}

fn to_identity_summary(state: &KioskIdentityStateFile) -> KioskIdentitySummary {
    KioskIdentitySummary {
        shared_app_client_id: state.shared_app_client_id.clone(),
        tenant_id: state.tenant_id.clone(),
        login_endpoint: state.login_endpoint.clone(),
        setup_client_id: state.setup_client_id.clone(),
        key_id: state.key_id.clone(),
        certificate_thumbprint: state.certificate_thumbprint.clone(),
        certificate_display_name: state.certificate_display_name.clone(),
        not_before_ms: state.not_before_ms,
        not_after_ms: state.not_after_ms,
        renewal_required: renewal_required(
            state.not_after_ms,
            OffsetDateTime::now_utc().unix_timestamp() * 1000,
        ),
    }
}

fn parse_identity_state(json: &str) -> Result<KioskIdentityStateFile, String> {
    let mut state = serde_json::from_str::<KioskIdentityStateFile>(json)
        .map_err(|error| format!("failed to parse kiosk identity state: {error}"))?;
    if state.version != 1 {
        return Err(format!(
            "unsupported kiosk identity state version: {}",
            state.version
        ));
    }
    if state.shared_app_client_id.trim().is_empty()
        || state.tenant_id.trim().is_empty()
        || state.login_endpoint.trim().is_empty()
        || state.setup_client_id.trim().is_empty()
        || state.certificate_pem.trim().is_empty()
        || state.key_id.trim().is_empty()
        || state.certificate_thumbprint.trim().is_empty()
    {
        return Err("kiosk identity state is missing required fields".into());
    }
    state.shared_app_client_id =
        normalize_guid(&state.shared_app_client_id, "Shared app client ID")?;
    state.tenant_id = normalize_guid(&state.tenant_id, "Tenant ID")?;
    state.setup_client_id = normalize_guid(&state.setup_client_id, "Setup client ID")?;
    state.key_id = normalize_guid(&state.key_id, "Kiosk key ID")?;
    state.login_endpoint = normalize_login_endpoint(&state.login_endpoint)?;
    Ok(state)
}

fn load_identity_state(app: &tauri::AppHandle) -> Result<Option<KioskIdentityStateFile>, String> {
    let Some(json) = read_optional_file_to_string(&identity_state_file_path(app)?)? else {
        return Ok(None);
    };
    parse_identity_state(&json).map(Some)
}

fn persist_identity_state(
    app: &tauri::AppHandle,
    state: &KioskIdentityStateFile,
) -> Result<(), String> {
    write_string_to_path(
        &identity_state_file_path(app)?,
        &serde_json::to_string_pretty(state)
            .map_err(|error| format!("failed to serialize kiosk identity state: {error}"))?,
    )
}

fn persist_private_key_and_identity_state(
    app: &tauri::AppHandle,
    identity_state: &KioskIdentityStateFile,
    private_key_pem: &str,
    rollback_private_key_pem: Option<&str>,
) -> Result<(), String> {
    store_private_key_secret(app, private_key_pem)?;
    if let Err(error) = persist_identity_state(app, identity_state) {
        match rollback_private_key_pem {
            Some(previous_private_key_pem) => {
                store_private_key_secret(app, previous_private_key_pem).map_err(
                    |rollback_error| {
                        format!(
                            "failed to persist kiosk identity state after updating the private key: {error}; failed to restore previous private key: {rollback_error}"
                        )
                    },
                )?;
            }
            None => {
                clear_private_key_secret(app).map_err(|rollback_error| {
                    format!(
                        "failed to persist kiosk identity state after storing the private key: {error}; failed to clear orphaned private key: {rollback_error}"
                    )
                })?;
            }
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(target_os = "android")]
fn ensure_android_keyring_initialized() -> Result<(), String> {
    static KEYRING_INIT: OnceLock<()> = OnceLock::new();
    if KEYRING_INIT.get().is_some() {
        return Ok(());
    }
    let store = AndroidKeyringStore::new()
        .map_err(|error| format!("failed to initialize Android secure key store: {error}"))?;
    keyring_core::set_default_store(store);
    let _ = KEYRING_INIT.set(());
    Ok(())
}

#[cfg(target_os = "android")]
fn load_private_key_secret(app: &tauri::AppHandle) -> Result<Option<String>, String> {
    let _ = app;
    ensure_android_keyring_initialized()?;
    let entry = Entry::new(KEYRING_SERVICE_NAME, KEYRING_USER_NAME)
        .map_err(|error| format!("failed to open Android secure store entry: {error}"))?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring_core::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!(
            "failed to read Android secure store entry: {error}"
        )),
    }
}

#[cfg(target_os = "android")]
fn store_private_key_secret(app: &tauri::AppHandle, private_key_pem: &str) -> Result<(), String> {
    let _ = app;
    ensure_android_keyring_initialized()?;
    let entry = Entry::new(KEYRING_SERVICE_NAME, KEYRING_USER_NAME)
        .map_err(|error| format!("failed to open Android secure store entry: {error}"))?;
    entry
        .set_password(private_key_pem)
        .map_err(|error| format!("failed to write Android secure store entry: {error}"))
}

#[cfg(target_os = "android")]
fn clear_private_key_secret(app: &tauri::AppHandle) -> Result<(), String> {
    let _ = app;
    ensure_android_keyring_initialized()?;
    let entry = Entry::new(KEYRING_SERVICE_NAME, KEYRING_USER_NAME)
        .map_err(|error| format!("failed to open Android secure store entry: {error}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!(
            "failed to clear Android secure store entry: {error}"
        )),
    }
}

#[cfg(not(target_os = "android"))]
fn load_private_key_secret(app: &tauri::AppHandle) -> Result<Option<String>, String> {
    read_optional_file_to_string(&private_key_file_path(app)?)
}

#[cfg(not(target_os = "android"))]
fn store_private_key_secret(app: &tauri::AppHandle, private_key_pem: &str) -> Result<(), String> {
    write_private_key_file_securely(&private_key_file_path(app)?, private_key_pem)
}

#[cfg(not(target_os = "android"))]
fn clear_private_key_secret(app: &tauri::AppHandle) -> Result<(), String> {
    remove_optional_file_at_path(&private_key_file_path(app)?)
}

fn clear_identity_local_state(app: &tauri::AppHandle) -> Result<(), String> {
    clear_private_key_secret(app)?;
    remove_optional_file_at_path(&identity_state_file_path(app)?)
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))
}

fn certificate_thumbprint_from_pem(certificate_pem: &str) -> Result<String, String> {
    let mut reader = std::io::BufReader::new(certificate_pem.as_bytes());
    let certificate = rustls_pemfile::certs(&mut reader)
        .next()
        .transpose()
        .map_err(|error| format!("failed to read PEM certificate: {error}"))?
        .ok_or_else(|| "PEM certificate was empty".to_string())?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(certificate.as_ref())))
}

fn load_signing_key_from_pem(private_key_pem: &[u8]) -> Result<(Algorithm, EncodingKey), String> {
    let key = EncodingKey::from_ec_pem(private_key_pem)
        .map_err(|error| format!("failed to parse P-256 private key PEM: {error}"))?;
    p256::SecretKey::from_pkcs8_pem(
        std::str::from_utf8(private_key_pem)
            .map_err(|error| format!("private key PEM was not valid UTF-8: {error}"))?,
    )
    .map_err(|error| format!("private key PEM was not a valid P-256 PKCS#8 key: {error}"))?;
    Ok((Algorithm::ES256, key))
}

fn certificate_public_key_der(certificate_pem: &str) -> Result<Vec<u8>, String> {
    let mut reader = std::io::BufReader::new(certificate_pem.as_bytes());
    let certificate = rustls_pemfile::certs(&mut reader)
        .next()
        .transpose()
        .map_err(|error| format!("failed to read PEM certificate: {error}"))?
        .ok_or_else(|| "PEM certificate was empty".to_string())?;
    let certificate = Certificate::from_der(certificate.as_ref())
        .map_err(|error| format!("failed to parse X.509 certificate: {error}"))?;
    certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|error| format!("failed to encode certificate public key: {error}"))
}

fn private_key_public_key_der(private_key_pem: &str) -> Result<Vec<u8>, String> {
    let private_key = p256::SecretKey::from_pkcs8_pem(private_key_pem)
        .map_err(|error| format!("failed to parse P-256 private key PEM: {error}"))?;
    private_key
        .public_key()
        .to_public_key_der()
        .map(|value| value.as_ref().to_vec())
        .map_err(|error| format!("failed to encode private key public key: {error}"))
}

fn validate_certificate_matches_private_key(
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<(), String> {
    if certificate_public_key_der(certificate_pem)? != private_key_public_key_der(private_key_pem)?
    {
        return Err("stored certificate public key does not match the stored private key".into());
    }
    Ok(())
}

fn build_client_assertion(
    client_id: &str,
    token_endpoint: &str,
    signing_algorithm: Algorithm,
    signing_key: &EncodingKey,
    certificate_thumbprint: &str,
) -> Result<String, String> {
    let now = OffsetDateTime::now_utc();
    let now_unix = now.unix_timestamp();
    let claims = ClientAssertionClaims {
        aud: token_endpoint.to_string(),
        iss: client_id.to_string(),
        sub: client_id.to_string(),
        jti: Uuid::new_v4().to_string(),
        nbf: now_unix,
        iat: now_unix,
        exp: (now + TimeDuration::seconds(CLIENT_ASSERTION_LIFETIME_SECS)).unix_timestamp(),
    };
    let mut header = Header::new(signing_algorithm);
    header.x5t_s256 = Some(certificate_thumbprint.to_string());
    jsonwebtoken::encode(&header, &claims, signing_key)
        .map_err(|error| format!("failed to sign client assertion: {error}"))
}

async fn fetch_application_access_token(
    runtime_state: &RuntimeCredentialState,
) -> Result<String, String> {
    validate_certificate_matches_private_key(
        &runtime_state.certificate_pem,
        &runtime_state.private_key_pem,
    )?;
    let certificate_thumbprint = certificate_thumbprint_from_pem(&runtime_state.certificate_pem)?;
    let (signing_algorithm, signing_key) =
        load_signing_key_from_pem(runtime_state.private_key_pem.as_bytes())?;
    let token_endpoint = format!(
        "{}/{}/oauth2/v2.0/token",
        normalize_login_endpoint(&runtime_state.login_endpoint)?,
        runtime_state.tenant_id
    );
    let client_assertion = build_client_assertion(
        &runtime_state.client_id,
        &token_endpoint,
        signing_algorithm,
        &signing_key,
        &certificate_thumbprint,
    )?;
    let response = http_client()?
        .post(token_endpoint)
        .form(&[
            ("client_id", runtime_state.client_id.as_str()),
            ("scope", STORAGE_TOKEN_SCOPE),
            ("grant_type", "client_credentials"),
            ("client_assertion_type", CLIENT_ASSERTION_TYPE),
            ("client_assertion", client_assertion.as_str()),
        ])
        .send()
        .await
        .map_err(|error| format!("application sign-in failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|error| format!("<failed to read token error response body: {error}>"));
        return Err(format!(
            "application sign-in failed: token endpoint returned {status}: {body}"
        ));
    }
    response
        .json::<OAuthTokenResponse>()
        .await
        .map_err(|error| format!("application sign-in returned an invalid token payload: {error}"))
        .map(|payload| payload.access_token)
}

fn build_runtime_state(
    identity_state: &KioskIdentityStateFile,
    private_key_pem: String,
) -> RuntimeCredentialState {
    RuntimeCredentialState {
        tenant_id: identity_state.tenant_id.clone(),
        client_id: identity_state.shared_app_client_id.clone(),
        login_endpoint: identity_state.login_endpoint.clone(),
        certificate_pem: identity_state.certificate_pem.clone(),
        private_key_pem,
    }
}

fn escape_odata_string_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn reverse_timestamp_hex(timestamp_ms: i64) -> Result<String, String> {
    let timestamp_ms = u64::try_from(timestamp_ms)
        .map_err(|_| "Telemetry refresh time bounds must be non-negative.".to_string())?;
    Ok(format!("{:016x}", u64::MAX - timestamp_ms))
}

fn history_table_filter(partition_key: &str, start_ms: i64, end_ms: i64) -> Result<String, String> {
    let row_key_start = reverse_timestamp_hex(end_ms)?;
    let row_key_end = reverse_timestamp_hex(start_ms)?;
    Ok(format!(
        "PartitionKey eq '{}' and RowKey ge '{}' and RowKey le '{}~'",
        escape_odata_string_literal(partition_key),
        row_key_start,
        row_key_end
    ))
}

fn node_partition_filter() -> &'static str {
    "PartitionKey ge 'n:' and PartitionKey lt 'n;'"
}

fn table_query_url(storage_account: &str, table_name: &str) -> String {
    format!("https://{storage_account}.table.core.windows.net/{table_name}()")
}

async fn query_table_entities<T>(
    access_token: &str,
    storage_account: &str,
    table_name: &str,
    filter: Option<&str>,
    top: Option<usize>,
    max_pages: usize,
) -> Result<Vec<T>, String>
where
    T: DeserializeOwned,
{
    let client = http_client()?;
    let mut entities = Vec::new();
    let mut next_partition_key: Option<String> = None;
    let mut next_row_key: Option<String> = None;
    let mut seen_continuation_tokens = HashSet::new();

    for page in 0..max_pages {
        let mut url = reqwest::Url::parse(&table_query_url(storage_account, table_name))
            .map_err(|error| format!("failed to build Azure Tables URL: {error}"))?;
        if let Some(filter) = filter {
            if !filter.is_empty() {
                url.query_pairs_mut().append_pair("$filter", filter);
            }
        }
        if let Some(top) = top {
            url.query_pairs_mut().append_pair("$top", &top.to_string());
        }
        if let Some(partition_key) = &next_partition_key {
            url.query_pairs_mut()
                .append_pair("NextPartitionKey", partition_key);
            if let Some(row_key) = &next_row_key {
                url.query_pairs_mut().append_pair("NextRowKey", row_key);
            }
        }

        let response = client
            .get(url)
            .header("Accept", "application/json;odata=nometadata")
            .bearer_auth(access_token)
            .header("x-ms-version", AZURE_TABLES_API_VERSION)
            .send()
            .await
            .map_err(|error| format!("Azure Tables query failed for {table_name}: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_else(|error| {
                format!("<failed to read Azure Tables error response body: {error}>")
            });
            return Err(format!(
                "Azure Tables query failed for {table_name}: service returned {status}: {body}"
            ));
        }

        next_partition_key = response
            .headers()
            .get("x-ms-continuation-NextPartitionKey")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        next_row_key = response
            .headers()
            .get("x-ms-continuation-NextRowKey")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        let payload = response
            .json::<AzureTableQueryResponse<T>>()
            .await
            .map_err(|error| {
                format!("Azure Tables returned an invalid {table_name} payload: {error}")
            })?;
        entities.extend(payload.value);

        if next_partition_key.is_none() {
            break;
        }

        let continuation_token = format!(
            "{}\n{}",
            next_partition_key.as_deref().unwrap_or_default(),
            next_row_key.as_deref().unwrap_or_default()
        );
        if !seen_continuation_tokens.insert(continuation_token) {
            return Err(format!(
                "Azure Tables query for {table_name} returned a repeated continuation token on page {}.",
                page + 1
            ));
        }
    }

    if next_partition_key.is_some() {
        return Err(format!(
            "Azure Tables query for {table_name} exceeded the maximum of {max_pages} page(s)."
        ));
    }

    Ok(entities)
}

fn is_node_partition_key(partition_key: &str) -> bool {
    partition_key.starts_with("n:")
}

fn latest_node_partition_map(rows: Vec<ActualStateEntity>) -> HashMap<String, String> {
    let mut latest_by_partition = HashMap::<String, ActualStateEntity>::new();
    for row in rows {
        if !is_node_partition_key(&row.partition_key) {
            continue;
        }
        let Some(node_id) = row.node_id.as_ref().map(|value| value.trim()) else {
            continue;
        };
        if node_id.is_empty() {
            continue;
        }
        match latest_by_partition.get(&row.partition_key) {
            Some(existing) if existing.row_key <= row.row_key => {}
            _ => {
                latest_by_partition.insert(row.partition_key.clone(), row);
            }
        }
    }

    latest_by_partition
        .into_values()
        .filter_map(|row| {
            row.node_id
                .map(|node_id| (node_id.trim().to_string(), row.partition_key))
        })
        .filter(|(node_id, _)| !node_id.is_empty())
        .collect()
}

fn parse_sensor_readings(
    decoded_readings: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    serde_json::from_str::<serde_json::Value>(decoded_readings)
        .ok()?
        .as_object()
        .cloned()
}

fn to_plottable_number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(value) => value.as_f64().filter(|value| value.is_finite()),
        serde_json::Value::String(value) => {
            let parsed = value.parse::<f64>().ok()?;
            if parsed.is_finite() && parsed.abs() <= JS_MAX_SAFE_INTEGER_F64 {
                Some(parsed)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn extract_series_for_partition(
    rows: Vec<SensorDataEntity>,
    variables: &[DashboardVariableRequest],
) -> HashMap<(String, String), Vec<TelemetryPoint>> {
    let mut series = HashMap::<(String, String), Vec<TelemetryPoint>>::new();
    for row in rows {
        let Some(timestamp_ms) = row.timestamp_ms else {
            continue;
        };
        let Some(decoded_readings) = row.decoded_readings.as_deref() else {
            continue;
        };
        let Some(readings) = parse_sensor_readings(decoded_readings) else {
            continue;
        };

        for variable in variables {
            let Some(value) = readings
                .get(&variable.reading_type)
                .and_then(to_plottable_number)
            else {
                continue;
            };
            series
                .entry((variable.node_id.clone(), variable.reading_type.clone()))
                .or_default()
                .push(TelemetryPoint {
                    timestamp_ms,
                    value,
                });
        }
    }

    for points in series.values_mut() {
        points.sort_by_key(|point| point.timestamp_ms);
    }
    series
}

async fn fetch_live_dashboard_variable_series(
    access_token: &str,
    request: &FetchDashboardVariableDataRequest,
) -> Result<Vec<DashboardVariableSeries>, String> {
    let actual_state_rows = query_table_entities::<ActualStateEntity>(
        access_token,
        &request.storage_account,
        ACTUAL_STATE_TABLE_NAME,
        Some(node_partition_filter()),
        None,
        MAX_TABLE_QUERY_PAGES,
    )
    .await?;
    let node_partitions = latest_node_partition_map(actual_state_rows);

    let mut variables_by_partition = HashMap::<String, Vec<DashboardVariableRequest>>::new();
    let mut missing_node_ids = Vec::new();
    for variable in &request.variables {
        let Some(partition_key) = node_partitions.get(&variable.node_id) else {
            missing_node_ids.push(variable.node_id.clone());
            continue;
        };
        variables_by_partition
            .entry(partition_key.clone())
            .or_default()
            .push(variable.clone());
    }

    if !missing_node_ids.is_empty() {
        missing_node_ids.sort();
        missing_node_ids.dedup();
        return Err(format!(
            "Telemetry refresh could not resolve node ID(s) from actualstate: {}.",
            missing_node_ids.join(", ")
        ));
    }

    let mut points_by_series = HashMap::<(String, String), Vec<TelemetryPoint>>::new();
    for (partition_key, variables) in variables_by_partition {
        let rows = query_table_entities::<SensorDataEntity>(
            access_token,
            &request.storage_account,
            SENSOR_DATA_TABLE_NAME,
            Some(&history_table_filter(
                &partition_key,
                request.start_ms,
                request.end_ms,
            )?),
            Some(SENSOR_DATA_TOP_PER_PAGE),
            MAX_TABLE_QUERY_PAGES,
        )
        .await?;

        for (series_key, points) in extract_series_for_partition(rows, &variables) {
            points_by_series.insert(series_key, points);
        }
    }

    Ok(request
        .variables
        .iter()
        .map(|variable| DashboardVariableSeries {
            node_id: variable.node_id.clone(),
            reading_type: variable.reading_type.clone(),
            points: points_by_series
                .get(&(variable.node_id.clone(), variable.reading_type.clone()))
                .cloned()
                .unwrap_or_default(),
        })
        .collect())
}

fn generate_certificate_bundle() -> Result<GeneratedCertificateBundle, String> {
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|error| format!("failed to generate kiosk certificate key pair: {error}"))?;
    let not_before = OffsetDateTime::now_utc();
    let not_after = not_before + TimeDuration::days(KIOSK_CERTIFICATE_VALIDITY_DAYS);
    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|error| format!("failed to create kiosk certificate parameters: {error}"))?;
    let mut distinguished_name = DistinguishedName::new();
    let key_id = Uuid::new_v4().to_string();
    let display_name = format!("sonde-kiosk-{key_id}");
    distinguished_name.push(DnType::CommonName, display_name.clone());
    params.distinguished_name = distinguished_name;
    params.not_before = not_before;
    params.not_after = not_after;
    let certificate = params
        .self_signed(&key_pair)
        .map_err(|error| format!("failed to generate self-signed kiosk certificate: {error}"))?;
    let certificate_pem = certificate.pem();
    let private_key_pem = key_pair.serialize_pem();
    let certificate_der_base64 =
        base64::engine::general_purpose::STANDARD.encode(certificate.der().as_ref());
    let thumbprint = certificate_thumbprint_from_pem(&certificate_pem)?;
    Ok(GeneratedCertificateBundle {
        certificate_pem,
        private_key_pem,
        certificate_der_base64,
        thumbprint,
        key_id,
        display_name,
        not_before_ms: not_before.unix_timestamp() * 1000,
        not_after_ms: not_after.unix_timestamp() * 1000,
    })
}

async fn fetch_graph_application(
    access_token: &str,
    shared_app_client_id: &str,
) -> Result<GraphApplicationResponse, String> {
    let response = http_client()?
        .get(format!(
            "https://graph.microsoft.com/v1.0/applications(appId='{}')?{}",
            shared_app_client_id, GRAPH_APP_SELECT_QUERY
        ))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| format!("failed to query shared Entra app: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|error| format!("<failed to read Graph error response body: {error}>"));
        return Err(format!(
            "failed to query shared Entra app: Graph returned {status}: {body}"
        ));
    }
    response
        .json::<GraphApplicationResponse>()
        .await
        .map_err(|error| format!("shared Entra app response was invalid: {error}"))
}

async fn patch_graph_application_keys(
    access_token: &str,
    shared_app_client_id: &str,
    key_credentials: Vec<GraphKeyCredential>,
) -> Result<(), String> {
    let response = http_client()?
        .patch(format!(
            "https://graph.microsoft.com/v1.0/applications(appId='{}')",
            shared_app_client_id
        ))
        .bearer_auth(access_token)
        .json(&GraphUpdateApplicationRequest { key_credentials })
        .send()
        .await
        .map_err(|error| format!("failed to update shared Entra app credentials: {error}"))?;
    if response.status() == StatusCode::NO_CONTENT {
        return Ok(());
    }
    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|error| format!("<failed to read Graph error response body: {error}>"));
        return Err(format!(
            "failed to update shared Entra app credentials: Graph returned {status}: {body}"
        ));
    }
    Ok(())
}

fn append_certificate_key(
    mut existing_keys: Vec<GraphKeyCredential>,
    bundle: &GeneratedCertificateBundle,
) -> Vec<GraphKeyCredential> {
    existing_keys.push(GraphKeyCredential {
        custom_key_identifier: None,
        display_name: Some(bundle.display_name.clone()),
        end_date_time: OffsetDateTime::from_unix_timestamp(bundle.not_after_ms / 1000)
            .expect("valid certificate end timestamp")
            .format(&time::format_description::well_known::Rfc3339)
            .expect("RFC3339 formatting works"),
        key: Some(bundle.certificate_der_base64.clone()),
        key_id: bundle.key_id.clone(),
        start_date_time: OffsetDateTime::from_unix_timestamp(bundle.not_before_ms / 1000)
            .expect("valid certificate start timestamp")
            .format(&time::format_description::well_known::Rfc3339)
            .expect("RFC3339 formatting works"),
        credential_type: "AsymmetricX509Cert".into(),
        usage: "Verify".into(),
    });
    existing_keys
}

fn remove_certificate_key(
    existing_keys: Vec<GraphKeyCredential>,
    key_id: &str,
) -> Vec<GraphKeyCredential> {
    existing_keys
        .into_iter()
        .filter(|key| key.key_id != key_id)
        .collect()
}

async fn begin_device_code_sign_in_with_client(
    request: &StartDeviceCodeSignInRequest,
    client: &reqwest::Client,
) -> Result<DeviceCodeResponse, String> {
    let login_endpoint = normalize_login_endpoint(&request.login_endpoint)?;
    let response = client
        .post(format!(
            "{}/{}/oauth2/v2.0/devicecode",
            login_endpoint, request.tenant_id
        ))
        .form(&[
            ("client_id", request.setup_client_id.as_str()),
            ("scope", GRAPH_SCOPE),
        ])
        .send()
        .await
        .map_err(|error| format!("failed to start device-code sign-in: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|error| format!("<failed to read token error response body: {error}>"));
        return Err(format!(
            "failed to start device-code sign-in: token endpoint returned {status}: {body}"
        ));
    }
    response
        .json::<DeviceCodeResponse>()
        .await
        .map_err(|error| format!("device-code sign-in response was invalid: {error}"))
}

async fn poll_device_code_sign_in_with_client(
    session: &DeviceCodeSession,
    client: &reqwest::Client,
) -> Result<DeviceCodePollResult, String> {
    if session.access_token.is_some() {
        return Ok(DeviceCodePollResult {
            response: PollDeviceCodeSignInResponse {
                status: "complete".into(),
                poll_interval_seconds: session.poll_interval_seconds,
                message: Some("Operator sign-in complete.".into()),
            },
            access_token: None,
            clear_session: false,
        });
    }
    if session.expires_at_ms <= current_time_ms() {
        return Ok(DeviceCodePollResult {
            response: PollDeviceCodeSignInResponse {
                status: "error".into(),
                poll_interval_seconds: session.poll_interval_seconds,
                message: Some("The device-code sign-in request has expired.".into()),
            },
            access_token: None,
            clear_session: true,
        });
    }
    let response = client
        .post(format!(
            "{}/{}/oauth2/v2.0/token",
            normalize_login_endpoint(&session.login_endpoint)?,
            session.tenant_id
        ))
        .form(&[
            ("grant_type", DEVICE_CODE_GRANT_TYPE),
            ("client_id", session.setup_client_id.as_str()),
            ("device_code", session.device_code.as_str()),
        ])
        .send()
        .await
        .map_err(|error| format!("failed to poll device-code sign-in: {error}"))?;
    let status = response.status();
    if status.is_success() {
        let access_token = response
            .json::<OAuthTokenResponse>()
            .await
            .map_err(|error| format!("device-code token response was invalid: {error}"))?
            .access_token;
        return Ok(DeviceCodePollResult {
            response: PollDeviceCodeSignInResponse {
                status: "complete".into(),
                poll_interval_seconds: session.poll_interval_seconds,
                message: Some("Operator sign-in complete.".into()),
            },
            access_token: Some(access_token),
            clear_session: false,
        });
    }
    let error_payload = response
        .json::<OAuthErrorResponse>()
        .await
        .map_err(|error| format!("device-code error response was invalid: {error}"))?;
    match error_payload.error.as_str() {
        "authorization_pending" => Ok(DeviceCodePollResult {
            response: PollDeviceCodeSignInResponse {
                status: "pending".into(),
                poll_interval_seconds: session.poll_interval_seconds,
                message: error_payload.error_description,
            },
            access_token: None,
            clear_session: false,
        }),
        "slow_down" => Ok(DeviceCodePollResult {
            response: PollDeviceCodeSignInResponse {
                status: "pending".into(),
                poll_interval_seconds: session.poll_interval_seconds.saturating_add(5),
                message: error_payload.error_description,
            },
            access_token: None,
            clear_session: false,
        }),
        "expired_token" => Ok(DeviceCodePollResult {
            response: PollDeviceCodeSignInResponse {
                status: "error".into(),
                poll_interval_seconds: session.poll_interval_seconds,
                message: Some("The device-code sign-in request has expired.".into()),
            },
            access_token: None,
            clear_session: true,
        }),
        _ => Ok(DeviceCodePollResult {
            response: PollDeviceCodeSignInResponse {
                status: "error".into(),
                poll_interval_seconds: session.poll_interval_seconds,
                message: Some(error_payload.error_description.unwrap_or_else(|| {
                    format!("Device-code sign-in failed: {}", error_payload.error)
                })),
            },
            access_token: None,
            clear_session: true,
        }),
    }
}

fn ensure_device_code_session_purpose(
    session: &DeviceCodeSession,
    expected_purpose: &str,
) -> Result<(), String> {
    if session.purpose != expected_purpose {
        return Err(format!(
            "device-code session purpose mismatch: expected {expected_purpose}, got {}",
            session.purpose
        ));
    }
    Ok(())
}

fn validate_device_code_purpose(purpose: &str) -> Result<(), String> {
    match purpose {
        "initial" | "renew" | "reset" => Ok(()),
        _ => Err(format!(
            "unsupported device-code sign-in purpose: {purpose}"
        )),
    }
}

fn prune_expired_device_code_sessions(sessions: &mut HashMap<String, DeviceCodeSession>) {
    let now_ms = current_time_ms();
    sessions.retain(|_, session| session.expires_at_ms > now_ms);
}

fn current_time_ms() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp() * 1000
}

#[tauri::command]
fn shared_dashboard_runtime_source() -> &'static str {
    SHARED_DASHBOARD_RUNTIME_SOURCE
}

#[tauri::command]
fn get_environment_json(app: tauri::AppHandle) -> Result<Option<String>, String> {
    read_optional_file_to_string(&environment_file_path(&app)?)
}

#[tauri::command]
fn save_environment_json(app: tauri::AppHandle, json: String) -> Result<(), String> {
    write_string_to_path(&environment_file_path(&app)?, &json)
}

#[tauri::command]
fn clear_environment_json(app: tauri::AppHandle) -> Result<(), String> {
    remove_optional_file_at_path(&environment_file_path(&app)?)
}

#[tauri::command]
fn get_telemetry_cache_json(app: tauri::AppHandle) -> Result<Option<String>, String> {
    read_optional_file_to_string(&telemetry_cache_file_path(&app)?)
}

#[tauri::command]
fn save_telemetry_cache_json(app: tauri::AppHandle, json: String) -> Result<(), String> {
    write_string_to_path(&telemetry_cache_file_path(&app)?, &json)
}

#[tauri::command]
fn clear_telemetry_cache_json(app: tauri::AppHandle) -> Result<(), String> {
    remove_optional_file_at_path(&telemetry_cache_file_path(&app)?)
}

#[tauri::command]
fn get_kiosk_identity_summary(
    app: tauri::AppHandle,
) -> Result<Option<KioskIdentitySummary>, String> {
    match load_identity_state(&app) {
        Ok(state) => Ok(state.as_ref().map(to_identity_summary)),
        Err(error) => {
            warn!("clearing corrupted kiosk identity state: {error}");
            clear_identity_local_state(&app).map_err(|clear_error| {
                format!(
                    "failed to clear corrupted kiosk identity state after {error}: {clear_error}"
                )
            })?;
            Ok(None)
        }
    }
}

#[tauri::command]
fn clear_kiosk_identity_local_state(app: tauri::AppHandle) -> Result<(), String> {
    clear_identity_local_state(&app)
}

#[tauri::command]
async fn start_device_code_sign_in(
    state: tauri::State<'_, AppState>,
    request: StartDeviceCodeSignInRequest,
) -> Result<DeviceCodeSignInSessionResponse, String> {
    validate_device_code_purpose(&request.purpose)?;
    let tenant_id = normalize_guid(&request.tenant_id, "Tenant ID")?;
    let login_endpoint = normalize_login_endpoint(&request.login_endpoint)?;
    let setup_client_id = normalize_guid(&request.setup_client_id, "Setup client ID")?;
    let validated_request = StartDeviceCodeSignInRequest {
        purpose: request.purpose.clone(),
        tenant_id: tenant_id.clone(),
        login_endpoint: login_endpoint.clone(),
        setup_client_id: setup_client_id.clone(),
    };
    let response =
        begin_device_code_sign_in_with_client(&validated_request, &http_client()?).await?;
    let session_id = Uuid::new_v4().to_string();
    let session = DeviceCodeSession {
        purpose: request.purpose.clone(),
        tenant_id,
        login_endpoint,
        setup_client_id,
        device_code: response.device_code,
        expires_at_ms: current_time_ms() + response.expires_in * 1000,
        poll_interval_seconds: response.interval.unwrap_or(5),
        access_token: None,
    };
    state
        .device_code_sessions
        .lock()
        .map_err(|_| "device-code session store was poisoned".to_string())
        .map(|mut sessions| {
            prune_expired_device_code_sessions(&mut sessions);
            sessions.insert(session_id.clone(), session);
        })?;
    Ok(DeviceCodeSignInSessionResponse {
        session_id,
        purpose: request.purpose,
        user_code: response.user_code,
        verification_uri: response.verification_uri,
        verification_uri_complete: response.verification_uri_complete,
        expires_at_ms: current_time_ms() + response.expires_in * 1000,
        poll_interval_seconds: response.interval.unwrap_or(5),
        message: response.message,
    })
}

#[tauri::command]
async fn poll_device_code_sign_in(
    state: tauri::State<'_, AppState>,
    request: PollDeviceCodeSignInRequest,
) -> Result<PollDeviceCodeSignInResponse, String> {
    let session = {
        state
            .device_code_sessions
            .lock()
            .map_err(|_| "device-code session store was poisoned".to_string())?
            .get(&request.session_id)
            .cloned()
            .ok_or_else(|| "device-code session not found".to_string())?
    };
    let client = http_client()?;
    let result = poll_device_code_sign_in_with_client(&session, &client).await?;
    if let Some(access_token) = result.access_token.clone() {
        if let Some(stored) = state
            .device_code_sessions
            .lock()
            .map_err(|_| "device-code session store was poisoned".to_string())?
            .get_mut(&request.session_id)
        {
            stored.access_token = Some(access_token);
        }
    } else if result.response.status == "pending"
        && result.response.poll_interval_seconds != session.poll_interval_seconds
    {
        if let Some(stored) = state
            .device_code_sessions
            .lock()
            .map_err(|_| "device-code session store was poisoned".to_string())?
            .get_mut(&request.session_id)
        {
            stored.poll_interval_seconds = result.response.poll_interval_seconds;
        }
    }
    if result.clear_session {
        state
            .device_code_sessions
            .lock()
            .map_err(|_| "device-code session store was poisoned".to_string())?
            .remove(&request.session_id);
    }
    Ok(result.response)
}

#[tauri::command]
async fn complete_kiosk_setup(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    request: CompleteKioskSetupRequest,
) -> Result<KioskSetupResult, String> {
    let shared_app_client_id =
        normalize_guid(&request.shared_app_client_id, "Shared app client ID")?;
    let request_tenant_id = normalize_guid(&request.tenant_id, "Tenant ID")?;
    let request_login_endpoint = normalize_login_endpoint(&request.login_endpoint)?;
    let request_setup_client_id = normalize_guid(&request.setup_client_id, "Setup client ID")?;
    let session = {
        state
            .device_code_sessions
            .lock()
            .map_err(|_| "device-code session store was poisoned".to_string())?
            .get(&request.session_id)
            .cloned()
            .ok_or_else(|| "device-code session not found".to_string())?
    };
    ensure_device_code_session_purpose(&session, "initial")?;
    if request_tenant_id != session.tenant_id {
        return Err("setup tenant ID does not match the active device-code session".into());
    }
    if request_login_endpoint != session.login_endpoint {
        return Err("setup login endpoint does not match the active device-code session".into());
    }
    if request_setup_client_id != session.setup_client_id {
        return Err("setup client ID does not match the active device-code session".into());
    }
    let access_token = session
        .access_token
        .clone()
        .ok_or_else(|| "operator sign-in is not complete yet".to_string())?;
    let app_state = fetch_graph_application(&access_token, &shared_app_client_id).await?;
    if app_state.app_id != shared_app_client_id {
        return Err("shared Entra app lookup returned the wrong application".into());
    }
    let bundle = generate_certificate_bundle()?;
    let key_credentials = append_certificate_key(app_state.key_credentials, &bundle);
    patch_graph_application_keys(&access_token, &shared_app_client_id, key_credentials).await?;

    let identity_state = KioskIdentityStateFile {
        version: 1,
        shared_app_client_id,
        tenant_id: session.tenant_id.clone(),
        login_endpoint: session.login_endpoint.clone(),
        setup_client_id: session.setup_client_id.clone(),
        certificate_pem: bundle.certificate_pem.clone(),
        key_id: bundle.key_id.clone(),
        certificate_thumbprint: bundle.thumbprint.clone(),
        certificate_display_name: bundle.display_name.clone(),
        not_before_ms: bundle.not_before_ms,
        not_after_ms: bundle.not_after_ms,
    };
    if let Err(error) =
        persist_private_key_and_identity_state(&app, &identity_state, &bundle.private_key_pem, None)
    {
        let rollback_keys = remove_certificate_key(
            fetch_graph_application(&access_token, &identity_state.shared_app_client_id)
                .await?
                .key_credentials,
            &bundle.key_id,
        );
        if let Err(rollback_error) = patch_graph_application_keys(
            &access_token,
            &identity_state.shared_app_client_id,
            rollback_keys,
        )
        .await
        {
            return Err(format!(
                "failed to persist kiosk identity locally after adding the remote credential: {error}; best-effort remote rollback for key {} also failed: {rollback_error}",
                bundle.key_id
            ));
        }
        return Err(format!(
            "failed to persist kiosk identity locally after adding the remote credential: {error}; the remote credential was rolled back"
        ));
    }
    fetch_application_access_token(&build_runtime_state(
        &identity_state,
        bundle.private_key_pem,
    ))
    .await?;

    state
        .device_code_sessions
        .lock()
        .map_err(|_| "device-code session store was poisoned".to_string())?
        .remove(&request.session_id);

    Ok(KioskSetupResult {
        summary: to_identity_summary(&identity_state),
        message: "Kiosk certificate provisioned and application sign-in succeeded.".into(),
    })
}

#[tauri::command]
async fn sign_in_kiosk_application(
    app: tauri::AppHandle,
) -> Result<KioskApplicationSignInResult, String> {
    let identity_state = load_identity_state(&app)?
        .ok_or_else(|| "kiosk identity is not configured yet".to_string())?;
    let private_key_pem = load_private_key_secret(&app)?
        .ok_or_else(|| "kiosk private key is missing from secure storage".to_string())?;
    let _ = fetch_application_access_token(&build_runtime_state(&identity_state, private_key_pem))
        .await?;
    Ok(KioskApplicationSignInResult {
        summary: to_identity_summary(&identity_state),
        message: if renewal_required(identity_state.not_after_ms, current_time_ms()) {
            "Application sign-in succeeded. The kiosk certificate should be renewed soon.".into()
        } else {
            "Application sign-in succeeded.".into()
        },
    })
}

#[tauri::command]
async fn renew_kiosk_certificate(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    request: RenewKioskCertificateRequest,
) -> Result<RenewKioskCertificateResult, String> {
    let session = {
        state
            .device_code_sessions
            .lock()
            .map_err(|_| "device-code session store was poisoned".to_string())?
            .get(&request.session_id)
            .cloned()
            .ok_or_else(|| "device-code session not found".to_string())?
    };
    ensure_device_code_session_purpose(&session, "renew")?;
    let access_token = session
        .access_token
        .clone()
        .ok_or_else(|| "operator sign-in is not complete yet".to_string())?;
    let previous_state = load_identity_state(&app)?
        .ok_or_else(|| "kiosk identity is not configured yet".to_string())?;
    let previous_private_key_pem = load_private_key_secret(&app)?
        .ok_or_else(|| "kiosk private key is not configured yet".to_string())?;
    let app_state =
        fetch_graph_application(&access_token, &previous_state.shared_app_client_id).await?;
    let bundle = generate_certificate_bundle()?;
    let added_keys = append_certificate_key(app_state.key_credentials.clone(), &bundle);
    patch_graph_application_keys(
        &access_token,
        &previous_state.shared_app_client_id,
        added_keys,
    )
    .await?;

    let next_state = KioskIdentityStateFile {
        version: 1,
        shared_app_client_id: previous_state.shared_app_client_id.clone(),
        tenant_id: previous_state.tenant_id.clone(),
        login_endpoint: previous_state.login_endpoint.clone(),
        setup_client_id: previous_state.setup_client_id.clone(),
        certificate_pem: bundle.certificate_pem.clone(),
        key_id: bundle.key_id.clone(),
        certificate_thumbprint: bundle.thumbprint.clone(),
        certificate_display_name: bundle.display_name.clone(),
        not_before_ms: bundle.not_before_ms,
        not_after_ms: bundle.not_after_ms,
    };
    persist_private_key_and_identity_state(
        &app,
        &next_state,
        &bundle.private_key_pem,
        Some(&previous_private_key_pem),
    )?;
    fetch_application_access_token(&build_runtime_state(&next_state, bundle.private_key_pem))
        .await?;

    let refreshed_app_state =
        fetch_graph_application(&access_token, &previous_state.shared_app_client_id).await?;
    let retained_keys =
        remove_certificate_key(refreshed_app_state.key_credentials, &previous_state.key_id);
    let cleanup_status = match patch_graph_application_keys(
        &access_token,
        &previous_state.shared_app_client_id,
        retained_keys,
    )
    .await
    {
        Ok(()) => "removed_previous".to_string(),
        Err(_) => "manual_follow_up".to_string(),
    };

    state
        .device_code_sessions
        .lock()
        .map_err(|_| "device-code session store was poisoned".to_string())?
        .remove(&request.session_id);

    Ok(RenewKioskCertificateResult {
        summary: to_identity_summary(&next_state),
        cleanup_status: cleanup_status.clone(),
        message: if cleanup_status == "removed_previous" {
            "Kiosk certificate renewed and the previous remote credential was removed.".into()
        } else {
            format!(
                "Kiosk certificate renewed, but remote cleanup for prior key {} may require manual follow-up.",
                previous_state.key_id
            )
        },
    })
}

#[tauri::command]
async fn reset_kiosk_app_state(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    request: ResetKioskAppRequest,
) -> Result<ResetKioskAppResult, String> {
    let existing_state = load_identity_state(&app)?;
    let mut remote_cleanup_status = "not_configured".to_string();
    let mut message = "Kiosk state cleared.".to_string();
    let mut cleared_key_id = None;
    let mut cleared_thumbprint = None;

    if let Some(identity_state) = existing_state.as_ref() {
        cleared_key_id = Some(identity_state.key_id.clone());
        cleared_thumbprint = Some(identity_state.certificate_thumbprint.clone());
        if let Some(session_id) = request.session_id.as_ref() {
            let session = {
                state
                    .device_code_sessions
                    .lock()
                    .map_err(|_| "device-code session store was poisoned".to_string())?
                    .get(session_id)
                    .cloned()
                    .ok_or_else(|| "device-code session not found".to_string())?
            };
            ensure_device_code_session_purpose(&session, "reset")?;
            let access_token = session
                .access_token
                .clone()
                .ok_or_else(|| "operator sign-in is not complete yet".to_string())?;
            let app_state =
                fetch_graph_application(&access_token, &identity_state.shared_app_client_id)
                    .await?;
            let retained_keys =
                remove_certificate_key(app_state.key_credentials, &identity_state.key_id);
            match patch_graph_application_keys(
                &access_token,
                &identity_state.shared_app_client_id,
                retained_keys,
            )
            .await
            {
                Ok(()) => {
                    remote_cleanup_status = "removed".into();
                    message =
                        "Kiosk state cleared and the remote kiosk certificate was removed.".into();
                }
                Err(error) => {
                    remote_cleanup_status = "failed".into();
                    message = format!(
                        "Kiosk state cleared, but remote cleanup for key {} failed: {error}",
                        identity_state.key_id
                    );
                }
            }
            state
                .device_code_sessions
                .lock()
                .map_err(|_| "device-code session store was poisoned".to_string())?
                .remove(session_id);
        } else {
            remote_cleanup_status = "skipped".into();
            message = format!(
                "Kiosk state cleared. Remote cleanup for key {} may require manual follow-up.",
                identity_state.key_id
            );
        }
    }

    clear_identity_local_state(&app)?;
    clear_environment_json(app.clone())?;
    clear_telemetry_cache_json(app)?;

    Ok(ResetKioskAppResult {
        remote_cleanup_status,
        message,
        cleared_key_id,
        cleared_certificate_thumbprint: cleared_thumbprint,
    })
}

#[tauri::command]
async fn fetch_dashboard_variable_data(
    app: tauri::AppHandle,
    request: FetchDashboardVariableDataRequest,
) -> Result<FetchDashboardVariableDataResponse, String> {
    let request = normalize_fetch_dashboard_variable_data_request(request)?;
    let identity_state = load_identity_state(&app)?
        .ok_or_else(|| "kiosk identity is not configured yet".to_string())?;
    if request.client_id != identity_state.shared_app_client_id {
        return Err(
            "Telemetry refresh client ID does not match the configured kiosk identity.".into(),
        );
    }
    if request.tenant_id != identity_state.tenant_id {
        return Err(
            "Telemetry refresh tenant ID does not match the configured kiosk identity.".into(),
        );
    }
    let private_key_pem = load_private_key_secret(&app)?
        .ok_or_else(|| "kiosk private key is missing from secure storage".to_string())?;
    let access_token =
        fetch_application_access_token(&build_runtime_state(&identity_state, private_key_pem))
            .await?;
    let series = fetch_live_dashboard_variable_series(&access_token, &request).await?;
    Ok(FetchDashboardVariableDataResponse {
        refreshed_at_ms: current_time_ms(),
        series,
    })
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter({
            #[cfg(debug_assertions)]
            const DEFAULT_FILTER: &str = "sonde_kiosk_ui=info,sonde_kiosk_ui_backend=info";
            #[cfg(not(debug_assertions))]
            const DEFAULT_FILTER: &str = "sonde_kiosk_ui=warn,sonde_kiosk_ui_backend=warn";

            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| DEFAULT_FILTER.into())
        })
        .init();

    tauri::Builder::default()
        .manage(AppState {
            device_code_sessions: Mutex::new(HashMap::new()),
        })
        .invoke_handler(tauri::generate_handler![
            shared_dashboard_runtime_source,
            get_environment_json,
            save_environment_json,
            clear_environment_json,
            get_telemetry_cache_json,
            save_telemetry_cache_json,
            clear_telemetry_cache_json,
            get_kiosk_identity_summary,
            clear_kiosk_identity_local_state,
            start_device_code_sign_in,
            poll_device_code_sign_in,
            complete_kiosk_setup,
            sign_in_kiosk_application,
            renew_kiosk_certificate,
            reset_kiosk_app_state,
            fetch_dashboard_variable_data,
        ])
        .run(tauri::generate_context!())
        .expect("error running Sonde Dashboard Kiosk");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(file_name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "sonde-kiosk-ui-tests-{}-{file_name}",
            std::process::id()
        ));
        path
    }

    #[test]
    fn read_environment_json_missing_file_returns_none() {
        let path = temp_path("missing.json");
        let _ = fs::remove_file(&path);
        assert_eq!(read_optional_file_to_string(&path).unwrap(), None);
    }

    #[test]
    fn write_then_read_environment_json_round_trips() {
        let path = temp_path("round-trip.json");
        let _ = fs::remove_file(&path);
        write_string_to_path(&path, "{\"name\":\"prod\"}").unwrap();
        assert_eq!(
            read_optional_file_to_string(&path).unwrap(),
            Some("{\"name\":\"prod\"}".into())
        );
        remove_optional_file_at_path(&path).unwrap();
    }

    #[test]
    fn clear_environment_json_ignores_missing_file() {
        let path = temp_path("clear-missing.json");
        let _ = fs::remove_file(&path);
        assert!(remove_optional_file_at_path(&path).is_ok());
    }

    #[test]
    fn telemetry_cache_json_round_trips() {
        let path = temp_path("telemetry-cache.json");
        let _ = fs::remove_file(&path);
        write_string_to_path(&path, "{\"version\":1,\"entries\":[]}").unwrap();
        assert_eq!(
            read_optional_file_to_string(&path).unwrap(),
            Some("{\"version\":1,\"entries\":[]}".into())
        );
        remove_optional_file_at_path(&path).unwrap();
    }

    #[test]
    fn parse_identity_state_rejects_missing_required_fields() {
        let error = parse_identity_state(
            r#"{"version":1,"sharedAppClientId":"","tenantId":"","loginEndpoint":"","setupClientId":"","certificatePem":"","keyId":"","certificateThumbprint":"","certificateDisplayName":"","notBeforeMs":0,"notAfterMs":0}"#,
        )
        .unwrap_err();
        assert!(error.contains("missing required fields"));
    }

    #[test]
    fn parse_identity_state_rejects_invalid_guid_and_login_endpoint() {
        let error = parse_identity_state(
            r#"{"version":1,"sharedAppClientId":"not-a-guid","tenantId":"22222222-2222-2222-2222-222222222222","loginEndpoint":"https://login.microsoftonline.com/common","setupClientId":"33333333-3333-3333-3333-333333333333","certificatePem":"pem","keyId":"44444444-4444-4444-4444-444444444444","certificateThumbprint":"thumb","certificateDisplayName":"display","notBeforeMs":0,"notAfterMs":0}"#,
        )
        .unwrap_err();
        assert!(error.contains("Shared app client ID must be a valid GUID"));
    }

    #[test]
    fn renewal_required_uses_thirty_day_threshold() {
        let now_ms = 1_000_000;
        assert!(renewal_required(
            now_ms + (KIOSK_RENEWAL_THRESHOLD_DAYS * 24 * 60 * 60 * 1000),
            now_ms,
        ));
        assert!(!renewal_required(
            now_ms + ((KIOSK_RENEWAL_THRESHOLD_DAYS + 1) * 24 * 60 * 60 * 1000),
            now_ms,
        ));
    }

    #[test]
    fn generate_certificate_bundle_produces_matching_material() {
        let bundle = generate_certificate_bundle().unwrap();
        assert!(bundle.certificate_pem.contains("BEGIN CERTIFICATE"));
        assert!(bundle.private_key_pem.contains("BEGIN PRIVATE KEY"));
        validate_certificate_matches_private_key(&bundle.certificate_pem, &bundle.private_key_pem)
            .unwrap();
    }

    #[test]
    fn append_certificate_key_adds_new_graph_key_credential() {
        let bundle = generate_certificate_bundle().unwrap();
        let keys = append_certificate_key(Vec::new(), &bundle);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key_id, bundle.key_id);
        assert_eq!(keys[0].credential_type, "AsymmetricX509Cert");
        assert_eq!(keys[0].usage, "Verify");
        assert_eq!(
            keys[0].key.as_deref(),
            Some(bundle.certificate_der_base64.as_str())
        );
    }

    #[test]
    fn remove_certificate_key_filters_matching_key_id() {
        let keys = vec![
            GraphKeyCredential {
                custom_key_identifier: None,
                display_name: Some("a".into()),
                end_date_time: "2026-01-01T00:00:00Z".into(),
                key: Some("A".into()),
                key_id: "keep".into(),
                start_date_time: "2025-01-01T00:00:00Z".into(),
                credential_type: "AsymmetricX509Cert".into(),
                usage: "Verify".into(),
            },
            GraphKeyCredential {
                custom_key_identifier: None,
                display_name: Some("b".into()),
                end_date_time: "2026-01-01T00:00:00Z".into(),
                key: Some("B".into()),
                key_id: "drop".into(),
                start_date_time: "2025-01-01T00:00:00Z".into(),
                credential_type: "AsymmetricX509Cert".into(),
                usage: "Verify".into(),
            },
        ];
        let filtered = remove_certificate_key(keys, "drop");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].key_id, "keep");
    }

    #[test]
    fn graph_key_credential_uses_graph_type_field_name() {
        let value = serde_json::to_value(GraphKeyCredential {
            custom_key_identifier: None,
            display_name: Some("display".into()),
            end_date_time: "2026-01-01T00:00:00Z".into(),
            key: Some("ABC".into()),
            key_id: "11111111-1111-1111-1111-111111111111".into(),
            start_date_time: "2025-01-01T00:00:00Z".into(),
            credential_type: "AsymmetricX509Cert".into(),
            usage: "Verify".into(),
        })
        .unwrap();
        assert_eq!(
            value.get("type").and_then(|field| field.as_str()),
            Some("AsymmetricX509Cert")
        );
        assert!(value.get("credentialType").is_none());
    }

    #[test]
    fn build_client_assertion_signs_expected_header_and_claims() {
        let bundle = generate_certificate_bundle().unwrap();
        let thumbprint = certificate_thumbprint_from_pem(&bundle.certificate_pem).unwrap();
        let token_endpoint = "https://login.microsoftonline.com/tenant/oauth2/v2.0/token";
        let client_id = "11111111-1111-1111-1111-111111111111";
        let (algorithm, signing_key) =
            load_signing_key_from_pem(bundle.private_key_pem.as_bytes()).unwrap();
        let assertion = build_client_assertion(
            client_id,
            token_endpoint,
            algorithm,
            &signing_key,
            &thumbprint,
        )
        .unwrap();

        let mut parts = assertion.split('.');
        let header_segment = parts.next().unwrap();
        let claims_segment = parts.next().unwrap();
        assert!(parts.next().is_some());

        let header: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(header_segment)
                .unwrap(),
        )
        .unwrap();
        let claims: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(claims_segment)
                .unwrap(),
        )
        .unwrap();

        assert_eq!(
            header.get("alg").and_then(|value| value.as_str()),
            Some("ES256")
        );
        assert_eq!(
            header.get("x5t#S256").and_then(|value| value.as_str()),
            Some(thumbprint.as_str())
        );
        assert_eq!(
            claims.get("iss").and_then(|value| value.as_str()),
            Some(client_id)
        );
        assert_eq!(
            claims.get("sub").and_then(|value| value.as_str()),
            Some(client_id)
        );
        assert_eq!(
            claims.get("aud").and_then(|value| value.as_str()),
            Some(token_endpoint)
        );
        assert!(claims.get("jti").and_then(|value| value.as_str()).is_some());
    }

    #[test]
    fn normalize_fetch_dashboard_variable_data_request_validates_fields() {
        let error =
            normalize_fetch_dashboard_variable_data_request(FetchDashboardVariableDataRequest {
                client_id: "11111111-1111-1111-1111-111111111111".into(),
                tenant_id: "22222222-2222-2222-2222-222222222222".into(),
                storage_account: "ProdStorage".into(),
                start_ms: 100,
                end_ms: 90,
                variables: vec![DashboardVariableRequest {
                    node_id: "NODE_001".into(),
                    reading_type: "temp_mc".into(),
                }],
            })
            .unwrap_err();
        assert!(error.contains("greater than or equal"));

        let error =
            normalize_fetch_dashboard_variable_data_request(FetchDashboardVariableDataRequest {
                client_id: "11111111-1111-1111-1111-111111111111".into(),
                tenant_id: "22222222-2222-2222-2222-222222222222".into(),
                storage_account: "ProdStorage".into(),
                start_ms: 90,
                end_ms: 100,
                variables: vec![DashboardVariableRequest {
                    node_id: "NODE_001".into(),
                    reading_type: "temp_mc".into(),
                }],
            })
            .unwrap_err();
        assert!(error.contains("lowercase alphanumeric"));
    }

    #[test]
    fn history_table_filter_matches_spa_row_key_range_shape() {
        assert_eq!(
            history_table_filter("n:abc", 0, 15).unwrap(),
            "PartitionKey eq 'n:abc' and RowKey ge 'fffffffffffffff0' and RowKey le 'ffffffffffffffff~'"
        );
    }

    #[test]
    fn node_partition_filter_matches_prefix_range_contract() {
        assert_eq!(
            node_partition_filter(),
            "PartitionKey ge 'n:' and PartitionKey lt 'n;'"
        );
    }

    #[test]
    fn latest_node_partition_map_keeps_latest_row_per_partition() {
        let node_partitions = latest_node_partition_map(vec![
            ActualStateEntity {
                partition_key: "n:001".into(),
                row_key: "fff5".into(),
                node_id: Some("NODE_A".into()),
            },
            ActualStateEntity {
                partition_key: "n:001".into(),
                row_key: "fff0".into(),
                node_id: Some("NODE_A_NEW".into()),
            },
            ActualStateEntity {
                partition_key: "g:001".into(),
                row_key: "0001".into(),
                node_id: Some("GATEWAY".into()),
            },
            ActualStateEntity {
                partition_key: "n:002".into(),
                row_key: "fff1".into(),
                node_id: Some("NODE_B".into()),
            },
        ]);
        assert_eq!(
            node_partitions.get("NODE_A_NEW"),
            Some(&"n:001".to_string())
        );
        assert_eq!(node_partitions.get("NODE_B"), Some(&"n:002".to_string()));
        assert!(!node_partitions.contains_key("NODE_A"));
        assert!(!node_partitions.contains_key("GATEWAY"));
    }

    #[test]
    fn extract_series_for_partition_matches_spa_numeric_rules() {
        let rows = vec![
            SensorDataEntity {
                decoded_readings: Some(
                    r#"{"temp_mc":21.5,"humidity_pct":"48.2","too_big":"9007199254740992"}"#.into(),
                ),
                timestamp_ms: Some(2_000),
            },
            SensorDataEntity {
                decoded_readings: Some(r#"{"temp_mc":"20.5","humidity_pct":"bad"}"#.into()),
                timestamp_ms: Some(1_000),
            },
            SensorDataEntity {
                decoded_readings: Some("not-json".into()),
                timestamp_ms: Some(500),
            },
        ];
        let variables = vec![
            DashboardVariableRequest {
                node_id: "NODE_001".into(),
                reading_type: "temp_mc".into(),
            },
            DashboardVariableRequest {
                node_id: "NODE_001".into(),
                reading_type: "humidity_pct".into(),
            },
            DashboardVariableRequest {
                node_id: "NODE_001".into(),
                reading_type: "too_big".into(),
            },
        ];

        let series = extract_series_for_partition(rows, &variables);
        assert_eq!(
            series
                .get(&("NODE_001".to_string(), "temp_mc".to_string()))
                .cloned()
                .unwrap(),
            vec![
                TelemetryPoint {
                    timestamp_ms: 1_000,
                    value: 20.5,
                },
                TelemetryPoint {
                    timestamp_ms: 2_000,
                    value: 21.5,
                },
            ]
        );
        assert_eq!(
            series
                .get(&("NODE_001".to_string(), "humidity_pct".to_string()))
                .cloned()
                .unwrap(),
            vec![TelemetryPoint {
                timestamp_ms: 2_000,
                value: 48.2,
            }]
        );
        assert!(!series.contains_key(&("NODE_001".to_string(), "too_big".to_string())));
    }

    #[test]
    fn normalize_login_endpoint_trims_trailing_slash() {
        assert_eq!(
            normalize_login_endpoint("https://login.microsoftonline.com/").unwrap(),
            "https://login.microsoftonline.com"
        );
    }

    #[test]
    fn normalize_login_endpoint_rejects_paths() {
        let error =
            normalize_login_endpoint("https://login.microsoftonline.com/common").unwrap_err();
        assert!(error.contains("authority URL"));
    }

    #[test]
    fn normalize_guid_rejects_invalid_values() {
        let error = normalize_guid("not-a-guid", "Tenant ID").unwrap_err();
        assert!(error.contains("Tenant ID must be a valid GUID"));
    }

    #[test]
    fn validate_device_code_purpose_rejects_unknown_values() {
        let error = validate_device_code_purpose("unexpected").unwrap_err();
        assert!(error.contains("unsupported device-code sign-in purpose"));
    }
}
