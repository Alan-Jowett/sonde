// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

package com.sonde.pair

import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.result.contract.ActivityResultContracts

/**
 * Custom [TauriActivity] subclass that requests BLE runtime permissions
 * on first launch.
 *
 * Android 12+ (API 31) requires explicit user consent for
 * [BLUETOOTH_SCAN][android.Manifest.permission.BLUETOOTH_SCAN] and
 * [BLUETOOTH_CONNECT][android.Manifest.permission.BLUETOOTH_CONNECT].
 * Earlier versions need
 * [ACCESS_FINE_LOCATION][android.Manifest.permission.ACCESS_FINE_LOCATION]
 * for BLE scanning.
 *
 * The consent dialog is shown once; subsequent launches skip the prompt
 * if permissions are already granted.  [BleHelper.requireBlePermissions]
 * still validates at call-time so a clear error is raised if the user
 * later revokes permissions through Settings.
 *
 * Fixes: https://github.com/Alan-Jowett/sonde/issues/323
 */
class MainActivity : TauriActivity() {

    private val requestPermissions = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) {
        // No action needed — BleHelper.requireBlePermissions() re-checks at
        // call-time.  If the user denies, the scan/connect attempt will
        // surface a descriptive error via the existing check.
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        requestBlePermissionsIfNeeded()
    }

    /**
     * Request the minimum set of BLE permissions that the current API level
     * requires, but only if any are still missing.
     */
    private fun requestBlePermissionsIfNeeded() {
        val needed = buildList {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                // Android 12+: BLUETOOTH_SCAN and BLUETOOTH_CONNECT are
                // runtime-gated dangerous permissions.
                if (!granted("android.permission.BLUETOOTH_SCAN")) {
                    add("android.permission.BLUETOOTH_SCAN")
                }
                if (!granted("android.permission.BLUETOOTH_CONNECT")) {
                    add("android.permission.BLUETOOTH_CONNECT")
                }
            } else {
                // Android 6–11: BLE scanning requires fine location.
                if (!granted("android.permission.ACCESS_FINE_LOCATION")) {
                    add("android.permission.ACCESS_FINE_LOCATION")
                }
            }
        }

        if (needed.isNotEmpty()) {
            requestPermissions.launch(needed.toTypedArray())
        }
    }

    private fun granted(permission: String): Boolean =
        checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED
}
