package com.sonde.kiosk

import android.os.Bundle
import io.crates.keyring.Keyring

class MainActivity : TauriActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Keyring.initializeNdkContext(applicationContext)
    }
}
