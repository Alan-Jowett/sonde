// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

package io.sonde.pair;

import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCallback;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattDescriptor;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothManager;
import android.bluetooth.BluetoothProfile;
import android.bluetooth.BluetoothStatusCodes;
import android.bluetooth.le.BluetoothLeScanner;
import android.bluetooth.le.ScanCallback;
import android.bluetooth.le.ScanFilter;
import android.bluetooth.le.ScanResult;
import android.bluetooth.le.ScanSettings;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.ParcelUuid;
import android.util.Log;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;

/**
 * JNI-friendly BLE helper for the sonde pairing protocol.
 *
 * <p>All public methods are blocking with bounded timeouts so they can be
 * called directly from Rust via JNI without callback gymnastics.
 *
 * <h3>Required Android permissions</h3>
 * <ul>
 *   <li>{@code BLUETOOTH_SCAN} (API 31+)</li>
 *   <li>{@code BLUETOOTH_CONNECT} (API 31+)</li>
 *   <li>{@code ACCESS_FINE_LOCATION} (for BLE scanning)</li>
 * </ul>
 */
public class BleHelper {

    private static final UUID CCCD_UUID =
            UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");

    private final Context context;
    private final BluetoothAdapter adapter;

    // --- Scan state --------------------------------------------------------
    private final List<ScanResult> discoveredDevices =
            Collections.synchronizedList(new ArrayList<>());
    private volatile ScanCallback activeScanCallback;
    private final List<ScanFilter> activeFilters = new ArrayList<>();

    // --- GATT state --------------------------------------------------------
    private volatile BluetoothGatt gatt;
    private volatile int connectionState = BluetoothProfile.STATE_DISCONNECTED;
    private volatile int negotiatedMtu = 23; // ATT default
    private volatile String lastError;

    // Per-operation latches (recreated before each blocking call)
    private volatile CountDownLatch connectLatch;
    private volatile CountDownLatch mtuLatch;
    private volatile CountDownLatch discoveryLatch;
    private volatile CountDownLatch writeLatch;
    private volatile CountDownLatch descriptorLatch;
    private volatile CountDownLatch bondLatch;

    // --- Bonding state -----------------------------------------------------
    private volatile boolean bonded;
    private volatile boolean bondReceiverRegistered;
    private volatile BluetoothDevice bondTarget;

    private final BroadcastReceiver bondReceiver = new BroadcastReceiver() {
        @Override
        public void onReceive(Context ctx, Intent intent) {
            if (!BluetoothDevice.ACTION_BOND_STATE_CHANGED.equals(intent.getAction())) {
                return;
            }
            BluetoothDevice device = intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE);
            if (device == null || bondTarget == null
                    || !device.getAddress().equals(bondTarget.getAddress())) {
                return;
            }
            int state = intent.getIntExtra(
                    BluetoothDevice.EXTRA_BOND_STATE, BluetoothDevice.BOND_NONE);
            Log.i("BleHelper", "bond state changed: " + state);
            if (state == BluetoothDevice.BOND_BONDED) {
                bonded = true;
                CountDownLatch l = bondLatch;
                if (l != null) l.countDown();
            } else if (state == BluetoothDevice.BOND_NONE) {
                // Pairing failed or was rejected
                int reason = intent.getIntExtra(
                        "android.bluetooth.device.extra.REASON", -1);
                lastError = "bonding failed (reason=" + reason + ")";
                bonded = false;
                CountDownLatch l = bondLatch;
                if (l != null) l.countDown();
            }
        }
    };

    // --- Indication / notification state -----------------------------------
    private final Set<UUID> subscribedChars =
            Collections.newSetFromMap(new ConcurrentHashMap<>());
    private final Map<UUID, LinkedBlockingQueue<byte[]>> indicationQueues =
            new ConcurrentHashMap<>();

    // --- GATT callback -----------------------------------------------------
    private final BluetoothGattCallback gattCallback = new BluetoothGattCallback() {

        @Override
        public void onConnectionStateChange(BluetoothGatt g, int status, int newState) {
            connectionState = newState;
            if (status != BluetoothGatt.GATT_SUCCESS) {
                lastError = "GATT status " + status;
            }
            CountDownLatch l = connectLatch;
            if (l != null) l.countDown();
        }

        @Override
        public void onMtuChanged(BluetoothGatt g, int mtu, int status) {
            if (status == BluetoothGatt.GATT_SUCCESS) {
                negotiatedMtu = mtu;
            } else {
                lastError = "MTU negotiation failed: status=" + status;
            }
            CountDownLatch l = mtuLatch;
            if (l != null) l.countDown();
        }

        @Override
        public void onServicesDiscovered(BluetoothGatt g, int status) {
            if (status != BluetoothGatt.GATT_SUCCESS) {
                lastError = "service discovery failed: status=" + status;
            }
            CountDownLatch l = discoveryLatch;
            if (l != null) l.countDown();
        }

        @Override
        public void onCharacteristicWrite(BluetoothGatt g,
                BluetoothGattCharacteristic c, int status) {
            if (status != BluetoothGatt.GATT_SUCCESS) {
                lastError = "write failed: status=" + status;
            }
            CountDownLatch l = writeLatch;
            if (l != null) l.countDown();
        }

        @Override
        public void onDescriptorWrite(BluetoothGatt g,
                BluetoothGattDescriptor d, int status) {
            if (status != BluetoothGatt.GATT_SUCCESS) {
                lastError = "descriptor write failed: status=" + status;
            }
            CountDownLatch l = descriptorLatch;
            if (l != null) l.countDown();
        }

        @Override
        @SuppressWarnings("deprecation")
        public void onCharacteristicChanged(BluetoothGatt g,
                BluetoothGattCharacteristic c) {
            // API < 33 path (deprecated but still called on older devices)
            enqueueIndication(c.getUuid(), c.getValue());
        }

        // API 33+ overload — value is passed directly instead of via
        // characteristic.getValue() which is no longer populated.
        @Override
        public void onCharacteristicChanged(BluetoothGatt g,
                BluetoothGattCharacteristic c, byte[] value) {
            enqueueIndication(c.getUuid(), value);
        }
    };

    private void enqueueIndication(UUID charUuid, byte[] value) {
        LinkedBlockingQueue<byte[]> q = indicationQueues.get(charUuid);
        if (q != null && value != null) {
            q.offer(value.clone());
        }
    }

    // --- Constructor -------------------------------------------------------

    /**
     * Create a new BLE helper bound to the application context.
     *
     * @throws Exception if Bluetooth is unavailable or disabled
     */
    public BleHelper(Context context) throws Exception {
        this.context = context.getApplicationContext();
        BluetoothManager mgr = (BluetoothManager)
                this.context.getSystemService(Context.BLUETOOTH_SERVICE);
        if (mgr == null) throw new Exception("BluetoothManager unavailable");
        this.adapter = mgr.getAdapter();
        if (this.adapter == null) throw new Exception("no Bluetooth adapter");
        if (!this.adapter.isEnabled()) throw new Exception("Bluetooth is disabled");
    }

    // --- Permission checks -------------------------------------------------

    /**
     * Verify that all BLE permissions required by the current API level have
     * been granted.  Throws a descriptive exception listing the missing
     * permissions so the Rust layer can surface an actionable error message.
     */
    private void requireBlePermissions() throws Exception {
        List<String> missing = new ArrayList<>();

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            // Android 12+ (API 31): need BLUETOOTH_SCAN and BLUETOOTH_CONNECT
            if (context.checkSelfPermission("android.permission.BLUETOOTH_SCAN")
                    != PackageManager.PERMISSION_GRANTED) {
                missing.add("BLUETOOTH_SCAN");
            }
            if (context.checkSelfPermission("android.permission.BLUETOOTH_CONNECT")
                    != PackageManager.PERMISSION_GRANTED) {
                missing.add("BLUETOOTH_CONNECT");
            }
        } else {
            // Android 6–11: need ACCESS_FINE_LOCATION for BLE scanning
            if (context.checkSelfPermission("android.permission.ACCESS_FINE_LOCATION")
                    != PackageManager.PERMISSION_GRANTED) {
                missing.add("ACCESS_FINE_LOCATION");
            }
        }

        if (!missing.isEmpty()) {
            throw new Exception(
                "BLE permissions not granted — request these at runtime: "
                + String.join(", ", missing));
        }
    }

    // --- Scanning ----------------------------------------------------------

    /**
     * Start or extend a BLE scan for the given service UUID.
     *
     * <p>If a scan is already active, the new UUID is added to the filter
     * set and the scan is restarted with all accumulated filters.  This
     * supports {@code DeviceScanner.start()}, which calls {@code start_scan}
     * once for the gateway UUID and once for the node UUID.
     *
     * @param serviceUuidStr UUID in standard string form
     *                       (e.g. {@code "0000fe60-0000-1000-8000-00805f9b34fb"})
     */
    public void startScan(String serviceUuidStr) throws Exception {
        requireBlePermissions();

        BluetoothLeScanner scanner = adapter.getBluetoothLeScanner();
        if (scanner == null) throw new Exception("BLE scanner unavailable");

        UUID serviceUuid = UUID.fromString(serviceUuidStr);

        ScanFilter filter = new ScanFilter.Builder()
                .setServiceUuid(new ParcelUuid(serviceUuid))
                .build();

        // If a scan is already active, stop it so we can restart with the
        // expanded filter set.  Preserve discovered devices across restarts.
        if (activeScanCallback != null) {
            try {
                scanner.stopScan(activeScanCallback);
            } catch (Exception ignored) { }
            activeScanCallback = null;
        } else {
            // Fresh scan — clear previous results and filters.
            discoveredDevices.clear();
            activeFilters.clear();
        }

        activeFilters.add(filter);

        ScanSettings settings = new ScanSettings.Builder()
                .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
                .build();

        ScanCallback cb = new ScanCallback() {
            @Override
            public void onScanResult(int callbackType, ScanResult result) {
                String addr = result.getDevice().getAddress();
                synchronized (discoveredDevices) {
                    for (int i = 0; i < discoveredDevices.size(); i++) {
                        if (discoveredDevices.get(i).getDevice()
                                .getAddress().equals(addr)) {
                            discoveredDevices.set(i, result);
                            return;
                        }
                    }
                    discoveredDevices.add(result);
                }
            }

            @Override
            public void onScanFailed(int errorCode) {
                lastError = "scan failed: error=" + errorCode;
            }
        };

        activeScanCallback = cb;
        scanner.startScan(activeFilters, settings, cb);
    }

    /** Stop the active BLE scan (no-op if not scanning). */
    public void stopScan() {
        ScanCallback cb = activeScanCallback;
        if (cb == null) return;
        activeScanCallback = null;
        activeFilters.clear();
        try {
            BluetoothLeScanner scanner = adapter.getBluetoothLeScanner();
            if (scanner != null) scanner.stopScan(cb);
        } catch (Exception ignored) { }
    }

    /** Number of devices found since the last {@link #startScan}. */
    public int getDiscoveredDeviceCount() {
        return discoveredDevices.size();
    }

    /** Local name of the device at {@code index}, or {@code ""} if absent. */
    public String getDeviceName(int index) {
        String name = discoveredDevices.get(index).getDevice().getName();
        return name != null ? name : "";
    }

    /** 6-byte BLE address of the device at {@code index}. */
    public byte[] getDeviceAddress(int index) {
        return macToBytes(discoveredDevices.get(index).getDevice().getAddress());
    }

    /** RSSI (dBm) of the device at {@code index}. */
    public int getDeviceRssi(int index) {
        return discoveredDevices.get(index).getRssi();
    }

    /**
     * Advertised service UUIDs for the device at {@code index}.
     *
     * @return array of UUID strings, or empty array if no scan record
     */
    public String[] getDeviceServiceUuids(int index) {
        ScanResult result = discoveredDevices.get(index);
        android.bluetooth.le.ScanRecord record = result.getScanRecord();
        if (record == null || record.getServiceUuids() == null) {
            return new String[0];
        }
        List<ParcelUuid> uuids = record.getServiceUuids();
        String[] out = new String[uuids.size()];
        for (int i = 0; i < uuids.size(); i++) {
            out[i] = uuids.get(i).getUuid().toString();
        }
        return out;
    }

    // --- Connection --------------------------------------------------------

    /**
     * Connect to the device, bond, negotiate MTU, and discover services.
     *
     * <p>Blocks until all four steps complete or {@code timeoutMs} elapses.
     * On failure or timeout the connection is cleaned up before returning.
     *
     * <p>Step 2 (bonding) initiates LESC pairing if not already bonded.
     * On the modem, this triggers Numeric Comparison — the operator must
     * confirm the passkey before the modem will accept GATT writes.
     *
     * @param address   6-byte BLE device address
     * @param timeoutMs overall deadline in milliseconds
     * @return the negotiated ATT MTU
     */
    public int connect(byte[] address, long timeoutMs) throws Exception {
        requireBlePermissions();
        disconnectInner();

        String addrStr = bytesToMac(address);
        BluetoothDevice device = adapter.getRemoteDevice(addrStr);

        lastError = null;
        long deadline = System.currentTimeMillis() + timeoutMs;

        // Step 0 — remove stale bond (must happen before GATT connect)
        // The modem does NOT persist bonds across reboots, so any existing
        // Android bond is stale and causes "encryption_change:key_missing"
        // failures (GATT status 133) on connection.
        if (device.getBondState() == BluetoothDevice.BOND_BONDED) {
            Log.i("BleHelper", "removing stale bond (modem does not persist bonds)");
            removeBond(device);
            // Give the stack a moment to process the removal
            Thread.sleep(500);
        }

        // Step 1 — connect
        connectLatch = new CountDownLatch(1);
        gatt = device.connectGatt(context, false, gattCallback,
                BluetoothDevice.TRANSPORT_LE);
        if (gatt == null) throw new Exception("connectGatt returned null");

        long remaining = deadline - System.currentTimeMillis();
        if (remaining <= 0
                || !connectLatch.await(remaining, TimeUnit.MILLISECONDS)) {
            disconnectInner();
            throw new Exception("connect timed out");
        }
        if (connectionState != BluetoothProfile.STATE_CONNECTED) {
            String err = lastError;
            disconnectInner();
            throw new Exception(err != null ? err : "connection failed");
        }

        // Step 2 — initiate LESC bonding (Numeric Comparison)
        // The modem requires a bonded link before it will accept GATT writes.
        {
            bonded = false;
            bondTarget = device;
            bondLatch = new CountDownLatch(1);
            lastError = null;

            // Register receiver before calling createBond to avoid races
            if (!bondReceiverRegistered) {
                IntentFilter filter = new IntentFilter(
                        BluetoothDevice.ACTION_BOND_STATE_CHANGED);
                context.registerReceiver(bondReceiver, filter);
                bondReceiverRegistered = true;
            }

            if (!device.createBond()) {
                Log.w("BleHelper", "createBond() returned false — bonding may already be in progress");
            }

            remaining = deadline - System.currentTimeMillis();
            if (remaining <= 0
                    || !bondLatch.await(remaining, TimeUnit.MILLISECONDS)) {
                disconnectInner();
                throw new Exception("bonding timed out");
            }
            if (!bonded) {
                String err = lastError;
                disconnectInner();
                throw new Exception(err != null ? err : "bonding failed");
            }
        }

        // Step 3 — request MTU (best effort; proceed even if request fails)
        mtuLatch = new CountDownLatch(1);
        if (gatt.requestMtu(517)) {
            remaining = deadline - System.currentTimeMillis();
            if (remaining > 0) mtuLatch.await(remaining, TimeUnit.MILLISECONDS);
        }
        // Clear any MTU error so it doesn't abort service discovery.
        lastError = null;

        // Step 4 — discover services
        discoveryLatch = new CountDownLatch(1);
        if (!gatt.discoverServices()) {
            disconnectInner();
            throw new Exception("discoverServices initiation failed");
        }
        remaining = deadline - System.currentTimeMillis();
        if (remaining <= 0
                || !discoveryLatch.await(remaining, TimeUnit.MILLISECONDS)) {
            disconnectInner();
            throw new Exception("service discovery timed out");
        }
        if (lastError != null) {
            String err = lastError;
            disconnectInner();
            throw new Exception(err);
        }

        return negotiatedMtu;
    }

    /** Disconnect and release GATT resources. */
    public void disconnect() {
        disconnectInner();
    }

    private void disconnectInner() {
        subscribedChars.clear();
        indicationQueues.clear();
        bondTarget = null;
        if (bondReceiverRegistered) {
            try { context.unregisterReceiver(bondReceiver); }
            catch (Exception ignored) { }
            bondReceiverRegistered = false;
        }
        BluetoothGatt g = gatt;
        gatt = null;
        connectionState = BluetoothProfile.STATE_DISCONNECTED;
        if (g != null) {
            try { g.disconnect(); } catch (Exception ignored) { }
            try { g.close(); } catch (Exception ignored) { }
        }
    }

    // --- GATT operations ---------------------------------------------------

    /**
     * Remove an existing bond (paired device entry) via reflection.
     *
     * <p>The modem does not persist bonds across reboots
     * ({@code CONFIG_BT_NIMBLE_NVS_PERSIST=n}), so any Android-side bond
     * from a previous session is stale and will cause
     * "encryption_change:key_missing" failures.  The public Android API
     * does not expose {@code removeBond()}, so we call it reflectively.
     */
    @SuppressWarnings("JavaReflectionMemberAccess")
    private static void removeBond(BluetoothDevice device) {
        try {
            device.getClass().getMethod("removeBond").invoke(device);
        } catch (Exception e) {
            Log.w("BleHelper", "removeBond failed: " + e.getMessage());
        }
    }

    /**
     * Write data to a characteristic (write-with-response).
     *
     * @param serviceUuidStr service UUID string
     * @param charUuidStr    characteristic UUID string
     * @param data           payload bytes
     * @param timeoutMs      write timeout in milliseconds
     */
    @SuppressWarnings("deprecation")
    public void writeCharacteristic(String serviceUuidStr,
            String charUuidStr, byte[] data, long timeoutMs) throws Exception {
        BluetoothGatt g = requireGatt();

        BluetoothGattCharacteristic chr =
                findCharacteristic(g, serviceUuidStr, charUuidStr);

        lastError = null;
        writeLatch = new CountDownLatch(1);

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            int rc = g.writeCharacteristic(chr, data,
                    BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT);
            if (rc != BluetoothStatusCodes.SUCCESS) {
                throw new Exception("writeCharacteristic failed: rc=" + rc);
            }
        } else {
            chr.setValue(data);
            chr.setWriteType(BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT);
            if (!g.writeCharacteristic(chr)) {
                throw new Exception("writeCharacteristic initiation failed");
            }
        }

        if (!writeLatch.await(timeoutMs, TimeUnit.MILLISECONDS)) {
            throw new Exception("write timed out");
        }
        if (lastError != null) throw new Exception(lastError);
    }

    /**
     * Wait for an indication/notification on the given characteristic.
     *
     * <p>Subscribes lazily on the first call for a given characteristic.
     *
     * @param serviceUuidStr service UUID string
     * @param charUuidStr    characteristic UUID string
     * @param timeoutMs      indication timeout in milliseconds
     * @return the indication payload
     */
    @SuppressWarnings("deprecation")
    public byte[] readIndication(String serviceUuidStr,
            String charUuidStr, long timeoutMs) throws Exception {
        BluetoothGatt g = requireGatt();
        UUID charUuid = UUID.fromString(charUuidStr);

        // Subscribe lazily
        if (!subscribedChars.contains(charUuid)) {
            BluetoothGattCharacteristic chr =
                    findCharacteristic(g, serviceUuidStr, charUuidStr);

            if (!g.setCharacteristicNotification(chr, true)) {
                throw new Exception("setCharacteristicNotification failed");
            }

            BluetoothGattDescriptor cccd = chr.getDescriptor(CCCD_UUID);
            if (cccd != null) {
                lastError = null;
                descriptorLatch = new CountDownLatch(1);

                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    int rc = g.writeDescriptor(cccd,
                            BluetoothGattDescriptor.ENABLE_INDICATION_VALUE);
                    if (rc != BluetoothStatusCodes.SUCCESS) {
                        throw new Exception("CCCD write failed: rc=" + rc);
                    }
                } else {
                    cccd.setValue(
                            BluetoothGattDescriptor.ENABLE_INDICATION_VALUE);
                    if (!g.writeDescriptor(cccd)) {
                        throw new Exception("CCCD write initiation failed");
                    }
                }

                if (!descriptorLatch.await(5000, TimeUnit.MILLISECONDS)) {
                    throw new Exception("CCCD write timed out");
                }
                if (lastError != null) throw new Exception(lastError);
            }

            indicationQueues.put(charUuid, new LinkedBlockingQueue<>());
            subscribedChars.add(charUuid);
        }

        LinkedBlockingQueue<byte[]> queue = indicationQueues.get(charUuid);
        if (queue == null) throw new Exception("indication queue missing");

        byte[] value = queue.poll(timeoutMs, TimeUnit.MILLISECONDS);
        if (value == null) throw new Exception("indication timeout");
        return value;
    }

    // --- Helpers -----------------------------------------------------------

    private BluetoothGatt requireGatt() throws Exception {
        BluetoothGatt g = gatt;
        if (g == null) throw new Exception("not connected");
        return g;
    }

    private static BluetoothGattCharacteristic findCharacteristic(
            BluetoothGatt g, String serviceUuidStr, String charUuidStr)
            throws Exception {
        BluetoothGattService svc =
                g.getService(UUID.fromString(serviceUuidStr));
        if (svc == null) {
            throw new Exception("service not found: " + serviceUuidStr);
        }
        BluetoothGattCharacteristic chr =
                svc.getCharacteristic(UUID.fromString(charUuidStr));
        if (chr == null) {
            throw new Exception("characteristic not found: " + charUuidStr);
        }
        return chr;
    }

    private static byte[] macToBytes(String mac) {
        String[] parts = mac.split(":");
        byte[] out = new byte[6];
        for (int i = 0; i < 6; i++) {
            out[i] = (byte) Integer.parseInt(parts[i], 16);
        }
        return out;
    }

    private static String bytesToMac(byte[] bytes) {
        return String.format(Locale.US,
                "%02X:%02X:%02X:%02X:%02X:%02X",
                bytes[0] & 0xFF, bytes[1] & 0xFF, bytes[2] & 0xFF,
                bytes[3] & 0xFF, bytes[4] & 0xFF, bytes[5] & 0xFF);
    }
}
