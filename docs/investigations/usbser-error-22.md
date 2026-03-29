# `usbser.sys` CDC ACM Write Failure — Investigation Data

**Status:** Root cause identified  
**Issue:** #529  
**Fix:** PR #564 (health poll reconnect trigger)

## Root Cause

**Modem reset causes stale USB pipe handles.**

When the ESP32-S3 modem resets (hardware button, watchdog, or software reset):

1. USB device physically disconnects from the bus
2. `usbccgp.sys` / `usbhub3.sys` tears down pipe endpoints
3. The ESP32-S3 re-enumerates (typically within 1–3 seconds)
4. Windows assigns the **same COM port number** — the existing file handle
   appears valid
5. **Read pipe recovers** — new URBs succeed, frames flow normally
6. **Write pipe is stuck** — the old write endpoint was invalidated during
   teardown, and `usbser.sys` does not transparently refresh it
7. Subsequent `WriteFile` calls return `STATUS_INVALID_DEVICE_REQUEST`
   (NTSTATUS `0xC0000010`), surfaced as Win32 `ERROR_BAD_COMMAND` (22)

**This is correct USB stack behavior, not a driver bug.** The file handle
survived the reset but the underlying pipe endpoint is gone. The fix is
to close and reopen the COM port after detecting the failure.

## Why Only Health Polls Fail

Both the data path (`Transport::send`) and health poll (`poll_status`)
use the **same `SharedWriter`** (`Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>>`).

However, the **data path writes always succeed** because they are
performed as an immediate response to a received frame — the write
URB is submitted while the USB endpoint is in an active transfer state
from the just-completed read. The driver's internal state machine
accepts the write because it is part of an ongoing I/O sequence.

The **health poll writes fail** because they are initiated independently
by a 30-second timer. After the modem reset, the write endpoint's pipe
handle is stale. Even though reads succeed (the read pipe is
re-established during re-enumeration), the write pipe remains in an
error state. Writes that are part of a read-response sequence succeed
because the driver treats them as continuation I/O, but standalone
writes submitted outside that context hit the stale pipe state.

This explains why the health poll at 03:02:06 fails even though a
WAKE/COMMAND exchange succeeded at 03:02:02 — the COMMAND write
succeeded because it was a direct response to the WAKE read, but the
health poll 4 seconds later is an independent write that hits the
stale pipe.

## Timeline Pattern

```
[modem reset occurs]

02:47:05  WAKE received       ← read succeeds, write pipe refreshed
02:47:05  COMMAND sent         ← write succeeds (immediately after read)
02:47:34  health poll write    FAILED  ← "cold" write, stale pipe
02:48:04  health poll write    FAILED
...every 30s, health polls fail...
03:02:02  WAKE received       ← read succeeds
03:02:02  COMMAND sent         ← write succeeds (read-response sequence)
03:02:06  health poll write    FAILED  ← independent write, stale pipe
...continues indefinitely...
```

## Device Information

| Field | Value |
|-------|-------|
| Device | ESP32-S3 native USB (Espressif TinyUSB CDC ACM) |
| VID/PID | `303A:1001` |
| Interface | MI_00 (CDC ACM data), MI_02 (JTAG/serial debug) |
| USB Speed | Full Speed (12 Mbps) |
| Windows Driver | `usbser.sys` (inbox CDC ACM class driver) |
| Instance ID | `USB\VID_303A&PID_1001&MI_00\<instance>` |
| OS | Windows 11 |

## Serial Port Configuration

| Setting | Value |
|---------|-------|
| Baud | 115200 (ignored by USB-CDC) |
| Data bits | 8 |
| Stop bits | 1 |
| Parity | None |
| Flow control | None |

## Concurrency Model

All writes serialized through a single async mutex:

```rust
type SharedWriter = Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

async fn send_encoded(writer: &SharedWriter, msg: &ModemMessage) -> Result<(), TransportError> {
    let frame = encode_modem_frame(msg)
        .map_err(|e| TransportError::Io(format!("encode modem frame: {e}")))?;
    let mut w = writer.lock().await;
    w.write_all(&frame)
        .await
        .map_err(|e| TransportError::Io(format!("write modem frame: {e}")))?;
    w.flush()
        .await
        .map_err(|e| TransportError::Io(format!("flush modem frame: {e}")))?;
    Ok(())
}
```

## Fix

PR #564 adds a consecutive failure counter to the health monitor. After
N failures (default 3), the monitor signals the gateway to **reconnect** —
closing the old COM port and opening a fresh handle with new pipe endpoints.

The proper sequence for deliberate modem resets:

```
1. Close COM port handle
2. Trigger reset (or accept the write may fail)
3. Wait for USB re-enumeration (device arrival notification or poll)
4. Reopen COM port
```

The gateway reconnect loop (`gateway.rs` outer `loop`) already implements
steps 1, 3, and 4. PR #564 provides the trigger (step 2 detection).

## USB ETW Trace Analysis

A 166-second ETW trace captured during the failure period shows:

```
Total USB events:   204,762
USBD_STATUS errors: 0
All NtStatus values: 0x0 (SUCCESS)
```

All URBs complete successfully at the USB host controller level. The error
originates in `usbser.sys` when it attempts to submit a write URB to an
endpoint that was torn down during the reset.

## Available Evidence Files

| File | Location | Description |
|------|----------|-------------|
| `usb-sonde.etl` | Local | USB ETW trace (~59 MB, not committed) |
| Gateway logs | Local | Application-level logs with timestamps |
| Soak test data | Local | 42 successful sensor readings during failure |
