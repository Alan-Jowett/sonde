// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

package io.sonde.pair;

import android.app.Activity;
import android.content.pm.PackageManager;
import android.os.Bundle;

import java.util.concurrent.CompletableFuture;
import java.util.concurrent.atomic.AtomicLong;

/**
 * Transparent, headless Activity that requests Android runtime permissions
 * and reports the result via a static {@link CompletableFuture}.
 *
 * <p>Each request carries a monotonic {@code requestNonce} so that a stale
 * Activity (e.g. one still open after a timeout) cannot complete a newer
 * caller's future.
 */
public class PermissionActivity extends Activity {

    /** Future completed when the user responds to the permission dialog. */
    static CompletableFuture<Boolean> pendingResult;

    /** Nonce of the current in-flight request; only the matching Activity may complete the future. */
    static long activeNonce;

    /** Monotonic counter for request nonces. */
    private static final AtomicLong NONCE_GEN = new AtomicLong();

    static final String EXTRA_PERMISSIONS = "permissions";
    static final String EXTRA_NONCE = "nonce";

    private static final int REQUEST_CODE = 29301;

    private long myNonce;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        myNonce = getIntent().getLongExtra(EXTRA_NONCE, -1);

        String[] perms = getIntent().getStringArrayExtra(EXTRA_PERMISSIONS);
        if (perms == null || perms.length == 0) {
            completeIfActive(false);
            finish();
            return;
        }
        requestPermissions(perms, REQUEST_CODE);
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] grantResults) {
        if (requestCode != REQUEST_CODE) {
            super.onRequestPermissionsResult(requestCode, permissions, grantResults);
            completeIfActive(false);
            finish();
            return;
        }
        // An empty grantResults array means the request was cancelled or
        // interrupted — treat as denied.
        boolean allGranted = grantResults.length > 0;
        for (int result : grantResults) {
            if (result != PackageManager.PERMISSION_GRANTED) {
                allGranted = false;
                break;
            }
        }
        completeIfActive(allGranted);
        finish();
    }

    /** Complete the pending future only if this Activity's nonce still matches. */
    private void completeIfActive(boolean granted) {
        synchronized (PermissionActivity.class) {
            if (pendingResult != null && myNonce == activeNonce) {
                pendingResult.complete(granted);
                pendingResult = null;
            }
        }
    }

    /** Allocate a fresh nonce for a new permission request. */
    static long nextNonce() {
        return NONCE_GEN.incrementAndGet();
    }
}
