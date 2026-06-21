// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Tauri v2 backend for the Sonde kiosk dashboard app.
//!
//! This tranche persists the imported kiosk environment JSON and exposes the
//! shared dashboard runtime source to the frontend shell. Identity bootstrap,
//! secure credential storage, and Azure-backed telemetry refresh remain in later
//! implementation tranches.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use tauri::Manager;

const ENVIRONMENT_FILE_NAME: &str = "environment.json";
const SHARED_DASHBOARD_RUNTIME_SOURCE: &str =
    include_str!("../../../../deploy/web-ui/dashboard-runtime.js");

fn environment_file_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let mut path = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("failed to resolve kiosk app data directory: {error}"))?;
    path.push(ENVIRONMENT_FILE_NAME);
    Ok(path)
}

fn read_environment_json_from_path(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(json) => Ok(Some(json)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "failed to read kiosk environment from {}: {error}",
            path.display()
        )),
    }
}

fn write_environment_json_to_path(path: &Path, json: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create kiosk environment directory {}: {error}",
                parent.display()
            )
        })?;
    }
    fs::write(path, json).map_err(|error| {
        format!(
            "failed to write kiosk environment to {}: {error}",
            path.display()
        )
    })
}

fn remove_environment_json_at_path(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to remove kiosk environment at {}: {error}",
            path.display()
        )),
    }
}

#[tauri::command]
fn shared_dashboard_runtime_source() -> &'static str {
    SHARED_DASHBOARD_RUNTIME_SOURCE
}

#[tauri::command]
fn get_environment_json(app: tauri::AppHandle) -> Result<Option<String>, String> {
    read_environment_json_from_path(&environment_file_path(&app)?)
}

#[tauri::command]
fn save_environment_json(app: tauri::AppHandle, json: String) -> Result<(), String> {
    write_environment_json_to_path(&environment_file_path(&app)?, &json)
}

#[tauri::command]
fn clear_environment_json(app: tauri::AppHandle) -> Result<(), String> {
    remove_environment_json_at_path(&environment_file_path(&app)?)
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
        .invoke_handler(tauri::generate_handler![
            shared_dashboard_runtime_source,
            get_environment_json,
            save_environment_json,
            clear_environment_json,
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
        assert_eq!(read_environment_json_from_path(&path).unwrap(), None);
    }

    #[test]
    fn write_then_read_environment_json_round_trips() {
        let path = temp_path("round-trip.json");
        let _ = fs::remove_file(&path);
        write_environment_json_to_path(&path, "{\"name\":\"prod\"}").unwrap();
        assert_eq!(
            read_environment_json_from_path(&path).unwrap(),
            Some("{\"name\":\"prod\"}".into())
        );
        remove_environment_json_at_path(&path).unwrap();
    }

    #[test]
    fn clear_environment_json_ignores_missing_file() {
        let path = temp_path("clear-missing.json");
        let _ = fs::remove_file(&path);
        assert!(remove_environment_json_at_path(&path).is_ok());
    }
}
