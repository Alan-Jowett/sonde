<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->

# Android APK Signing Setup

This guide explains how to configure signed release APK builds in the
Sonde CI pipeline.  Once configured, the `Tauri Android APK` workflow
produces both a debug APK (always) and a signed release APK.

## Overview

Android requires all APKs to be signed before installation.  Debug builds
use a shared debug keystore; release builds need a project-specific
keystore.  The CI workflow (`tauri-android.yml`) supports both:

- **Debug APK** — built on every push/PR, signed with the Android SDK debug keystore
- **Release APK** — built only when signing secrets are configured, signed with the project keystore

## Step 1: Generate a keystore

Run once on a trusted machine.  **Back up the `.jks` file securely** —
losing it means you cannot push updates to any app signed with this key.

```sh
keytool -genkey -v \
  -keystore sonde-release.jks \
  -keyalg RSA -keysize 2048 \
  -validity 10000 \
  -alias sonde \
  -storepass <STORE_PASSWORD> \
  -keypass <KEY_PASSWORD> \
  -dname "CN=Sonde, O=sonde contributors"
```

## Step 2: Add GitHub Actions secrets

Go to **Settings → Secrets and variables → Actions** and add:

| Secret name | Value |
|-------------|-------|
| `ANDROID_KEYSTORE` | Base64-encoded keystore: Linux `base64 -w0 sonde-release.jks`; macOS `base64 sonde-release.jks \| tr -d '\n'` |
| `ANDROID_KEYSTORE_PASSWORD` | The `-storepass` value from Step 1 |
| `ANDROID_KEY_ALIAS` | `sonde` (or whatever `-alias` you chose) |
| `ANDROID_KEY_PASSWORD` | The `-keypass` value from Step 1 |

## Step 3: Verify

Push a commit that triggers the `Tauri Android APK` workflow.  Check
the workflow run:

1. The "Decode release keystore" step should run (not skipped).
2. The "Build Android APK (release)" step should produce a signed APK.
3. Two artifacts should appear: `sonde-pair-android-debug` and
   `sonde-pair-android-release`.

## Notes

- **Secrets on PRs:** GitHub does not expose secrets to pull requests
  from forks.  Pull requests from branches in the same repository
  may have access to secrets, subject to repository settings.  When
  signing secrets are unavailable, the release build step is skipped
  gracefully — only the debug APK is produced.
- **Key rotation:** If you need to rotate the signing key, generate
  a new keystore, update the secrets, and consider using Google Play
  App Signing (where Google manages the distribution key and the
  repo key is only an upload key).
- **The keystore file is never committed to the repository.**  It
  exists only as a base64-encoded GitHub secret and is decoded to a
  temporary file during the build, then cleaned up.
