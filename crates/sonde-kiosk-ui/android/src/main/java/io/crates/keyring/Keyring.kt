package io.crates.keyring

import android.content.Context

class Keyring {
    companion object {
        init {
            System.loadLibrary("sonde_kiosk_ui_backend")
        }

        external fun initializeNdkContext(context: Context)
    }
}
