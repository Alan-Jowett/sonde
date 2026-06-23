// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

package com.sonde.kiosk

import android.os.Bundle
import io.crates.keyring.Keyring

class MainActivity : TauriActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Keyring.initializeNdkContext(applicationContext)
    }
}
