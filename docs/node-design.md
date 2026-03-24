<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Node Design Specification

> **Document status:** Draft  
> **Scope:** Architecture and internal design of the Sonde node firmware.  
> **Audience:** Implementers (human or LLM agent) building the node firmware.  
> **Related:** [node-requirements.md](node-requirements.md), [protocol.md](protocol.md), [security.md](security.md), [bpf-environment.md](bpf-environment.md), [node-bom.md](node-bom.md)

---

## 1  Overview

The node firmware is a single Rust binary targeting ESP32-C3 (RISC-V) and ESP32-S3 (Xtensa) via ESP-IDF bindings. It implements a simple cyclic state machine:

On each power-on or deep-sleep wake the node moves through the following stages in order: boot, wake-up hardware initialization, WAKE/COMMAND radio exchange with the gateway, execution of the received command (e.g., program update or schedule change), BPF program execution, and finally deep sleep until the next scheduled wake time.

```
boot → wake → WAKE/COMMAND exchange → execute command → BPF execution → sleep
```

The firmware is **uniform across all nodes** — application behavior is defined entirely by the BPF program, not the firmware. The firmware's job is protocol, transport, security, BPF execution, and power management.

---

## 2  Technology choices

| Decision | Choice | Rationale |
|---|---|---|
| Language | Rust | Same language as gateway; memory safety on bare metal |
| Protocol crate | `sonde-protocol` (shared with gateway) | `no_std`-compatible; frame codec, CBOR messages, constants |
| Platform bindings | `esp-idf-hal` + `esp-idf-svc` | Full ESP-IDF feature access (ESP-NOW, deep sleep, hardware crypto, flash partitions) |
| BPF interpreter | `sonde-bpf` — custom RFC 9669 interpreter with tagged registers and zero heap allocation |
| CBOR | Via `sonde-protocol` (`ciborium`) | serde-compatible; matches protocol crate implementation |
| HMAC | ESP-IDF hardware HMAC peripheral (implements `sonde-protocol::HmacProvider` trait) | Hardware-accelerated; ~10x faster than software |
| SHA-256 | ESP-IDF hardware SHA peripheral | Hardware-accelerated; used for program hash verification |
| RNG | ESP-IDF hardware TRNG | True random number generator; used for WAKE nonce |
| Toolchain | Upstream Rust (C3) / `espup` (S3) | C3 is RISC-V (upstream); S3 is Xtensa (custom toolchain) |

---

## 3  Module architecture

The node firmware is divided into eleven functional modules arranged in two tiers. The upper tier handles the data path: Transport (ESP-NOW radio), Protocol Codec (frame encode/decode), Wake Cycle Engine (session state machine), and BPF Runtime (program execution). The lower tier provides platform services: HAL (I2C/SPI/GPIO/ADC buses), Key Store (PSK in dedicated flash partition), Program Store (A/B flash partitions), Map Storage (RTC SRAM), Auth (HMAC verification and key-hint derivation), and BLE Pairing (LESC Just Works provisioning and PEER_REQUEST registration). A horizontal Sleep Manager spans the bottom of the firmware, managing deep sleep, wake intervals, and RTC memory. Data flows left-to-right in the upper tier; the Wake Cycle Engine coordinates all lower-tier modules.

```
┌──────────────────────────────────────────────────────────────┐
│                      node firmware                           │
│                                                              │
│  ┌──────────┐  ┌──────────┐  ┌───────────┐  ┌────────────┐   │
│  │Transport │  │ Protocol │  │   Wake    │  │   BPF      │   │
│  │ (ESP-NOW)│──│  Codec   │──│  Cycle    │──│  Runtime   │   │
│  └──────────┘  └──────────┘  │  Engine   │  └────────────┘   │
│                              └───────────┘       │           │
│  ┌──────────┐  ┌──────────┐  ┌───────────┐  ┌────────────┐   │
│  │  HAL     │  │  Key     │  │ Program   │  │   Map      │   │
│  │ (buses)  │  │  Store   │  │ Store     │  │  Storage   │   │
│  └──────────┘  └──────────┘  └───────────┘  └────────────┘   │
│                                                              │
│  ┌──────────────────────────────────────────────────────────┐│
│  │  Sleep Manager (deep sleep, wake interval, RTC memory)   ││
│  └──────────────────────────────────────────────────────────┘│
└──────────────────────────────────────────────────────────────┘
```

### 3.1  Module responsibilities

| Module | Responsibility | Requirements covered |
|---|---|---|
| **Transport** | ESP-NOW send/receive, frame size enforcement | ND-0100, ND-0103 |
| **Protocol Codec** | Frame serialization/deserialization, CBOR encoding | ND-0101, ND-0102 |
| **Wake Cycle Engine** | State machine: WAKE → COMMAND → transfer/execute → sleep | ND-0200–0203, ND-0700–0702 |
| **Key Store** | PSK storage in dedicated flash partition, pairing, factory reset | ND-0400, ND-0402 |
| **Program Store** | A/B flash partitions, program image decoding, LDDW resolution | ND-0500–0503, ND-0501a |
| **BPF Runtime** | `sonde-bpf` interpreter, helper dispatch, execution constraints | ND-0504–0506, ND-0600–0606 |
| **Map Storage** | Sleep-persistent maps in RTC slow SRAM | ND-0603, ND-0606 |
| **HAL** | I2C, SPI, GPIO, ADC bus access for BPF helpers | ND-0601 |
| **Sleep Manager** | Deep sleep entry, wake interval, RTC memory management | ND-0203 |
| **Auth** | HMAC-SHA256 (hardware), nonce generation, response verification | ND-0300–0304 |
| **BLE Pairing** | NimBLE stack, GATT provisioning service, PEER_REQUEST registration | ND-0900–0918 |

---

## 4  Wake cycle engine

The wake cycle engine is the central state machine. It runs once per wake and then the node sleeps.

### 4.1  State machine

The state machine has five main states plus two alternate boot paths. Starting from BOOT, the node reads credentials from the `key` flash partition (§6.1) and the `reg_complete` flag from NVS (§6.1a) to determine the boot path (ND-0900): (1) no PSK in key partition or pairing button held ≥ 500 ms → enter BLE pairing mode (§15); (2) PSK present but `reg_complete` not set → enter PEER_REQUEST registration (§15.7); (3) PSK present and `reg_complete` set → enter WAKE SEND. WAKE SEND transmits a WAKE frame and waits for a COMMAND response (retrying up to 3 times); if all retries fail it goes directly to SLEEP. On receiving a COMMAND, the node enters DISPATCH COMMAND, which branches on the command type: NOP proceeds to BPF execution; UPDATE_PROGRAM or RUN_EPHEMERAL initiates chunked transfer before BPF execution; UPDATE_SCHEDULE stores the new interval and proceeds to BPF execution; REBOOT restarts the firmware. After BPF execution — which may perform APP_DATA exchanges with the gateway — the node enters SLEEP.

```
┌─────────┐
│  BOOT   │
└────┬────┘
     │ check PSK + reg_complete (ND-0900)
     │
     ├── no PSK OR button held → BLE pairing mode (§15)
     ├── PSK + no reg_complete → PEER_REQUEST (§15.7)
     │
     ▼ PSK + reg_complete
┌─────────┐     no response     ┌─────────┐
│  WAKE   │────(retry ≤ 3)────►│  SLEEP  │
│  SEND   │                     └─────────┘
└────┬────┘
     │ COMMAND received
     ▼
┌─────────────┐
│  DISPATCH   │
│  COMMAND    │
└──┬──┬──┬──┬─┘
   │  │  │  │
   │  │  │  └── REBOOT → restart
   │  │  └───── UPDATE_SCHEDULE → store interval
   │  └──────── UPDATE_PROGRAM / RUN_EPHEMERAL → chunked transfer
   └─────────── NOP → proceed
     │
     ▼
┌─────────────┐
│  BPF EXEC   │──── send() / send_recv() → APP_DATA exchanges
└────┬────────┘
     │
     ▼
┌─────────┐
│  SLEEP  │
└─────────┘
```

### 4.2  Wake sequence (detailed)

1. **Boot/wake**: Initialize hardware. Determine boot path per ND-0900: (1) no PSK or button held → BLE pairing mode, (2) PSK + no `reg_complete` → PEER_REQUEST registration, (3) PSK + `reg_complete` → proceed to step 2.
2. **Generate nonce**: Hardware RNG produces a 64-bit random nonce.
3. **Send WAKE**: Construct WAKE frame (`firmware_abi_version`, `program_hash`, `battery_mv`). HMAC-sign. Transmit via ESP-NOW.
4. **Await COMMAND**: Wait up to 50 ms. If no response, retry (up to 3 times, 100 ms between). If all retries fail, sleep.
5. **Verify COMMAND**: Check HMAC. Verify echoed nonce matches. Decode CBOR. Extract `starting_seq` and `timestamp_ms`.
6. **Dispatch command**:
   - `NOP` → proceed to BPF execution.
   - `UPDATE_PROGRAM` / `RUN_EPHEMERAL` → enter chunked transfer.
   - `UPDATE_SCHEDULE` → store new base interval, proceed to BPF execution.
   - `REBOOT` → restart firmware.
   - Unknown → treat as NOP.
7. **BPF execution**: Execute resident (or newly installed/ephemeral) program.
8. **Sleep**: Enter deep sleep for `min(set_next_wake_value, base_interval)`.

### 4.3  Chunked transfer sub-state

The chunked transfer loop iterates over each chunk index from 0 to `chunk_count − 1`. For each chunk: compute the sequence number (`starting_seq + chunk_index`), send `GET_CHUNK` with that sequence number, await the `CHUNK` response (50 ms timeout, up to 3 retries per chunk); if all retries fail, abort and sleep. After collecting all chunks, reassemble the program image, verify its SHA-256 hash against the expected value (if mismatched, discard and sleep), decode the CBOR program image (bytecode and map definitions), resolve `LDDW src=1` instructions to runtime map pointers, install the program (flash for resident programs, RAM for ephemeral), and send `PROGRAM_ACK`.

```
for chunk_index in 0..chunk_count:
    seq = starting_seq + chunk_index       (GET_CHUNK #0 uses starting_seq)
    send GET_CHUNK { chunk_index } with seq
    await CHUNK response (50 ms timeout, 3 retries per chunk)
    if all retries fail → abort, sleep
    verify echoed seq, HMAC
    store chunk data

reassemble program image
verify SHA-256 hash
if mismatch → discard, sleep
decode CBOR program image (extract bytecode + maps)
resolve LDDW src=1 → runtime map pointers
install program (flash for resident, RAM for ephemeral)
send PROGRAM_ACK { program_hash }
```

---

## 5  Protocol codec

The protocol codec is provided by the shared `sonde-protocol` crate (see § Shared protocol crate below). The node uses the same frame format, CBOR message types, and constants as the gateway. Platform-specific behavior (HMAC computation) is injected via a trait.

### 5.1  Frame construction

Uses `sonde_protocol::encode_frame()` with a constructed `FrameHeader`, the node's PSK, and the node's HMAC implementation:

```rust
let header = sonde_protocol::FrameHeader {
    key_hint,
    msg_type,
    nonce: nonce_or_seq,
};
let frame = sonde_protocol::encode_frame(
    &header, &payload_cbor, psk, &hmac_impl,
);
```

The hardware HMAC implementation wraps the ESP-IDF HMAC peripheral behind the `sonde_protocol::HmacProvider` trait. Total frame size is asserted ≤ 250 bytes (ND-0103).

### 5.2  Frame verification (inbound)

Uses `sonde_protocol::decode_frame()` and `sonde_protocol::verify_frame()`:

1. Decode frame into header + payload + HMAC.
2. Verify HMAC via `sonde_protocol::verify_frame()` using the hardware peripheral (through `HmacProvider` trait).
3. Verify echoed nonce/seq matches the value sent. Mismatch → discard.
4. Decode CBOR payload into typed `GatewayMessage`.

---

## 6  Key store

### 6.1  Flash partition layout

| Partition | Contents | Size |
|---|---|---|
| `key` | 256-bit PSK + key_hint (2 bytes) + magic (4 bytes) | 4 KB sector |
| `program_a` | Resident program image (CBOR) | 4 KB |
| `program_b` | Resident program image (CBOR, A/B swap) | 4 KB |
| `schedule` | Base wake interval (u32) + active partition flag | 4 KB sector |

The magic bytes in the key partition indicate whether a PSK is provisioned. An erased (all 0xFF) partition means unpaired.

### 6.2  Factory reset

Factory reset (ND-0402, ND-0917) erases:
1. `key` partition (PSK + key_hint + magic → all 0xFF).
2. NVS pairing keys (`peer_payload`, `reg_complete` → erased).
3. RTC slow SRAM (map data → zeroed).
4. Both program partitions (`program_a`, `program_b` → erased).
5. Schedule partition → reset to default interval.

After reset, the magic bytes are missing → firmware detects unpaired state → enters BLE pairing mode on next boot.

### 6.1a  NVS layout for BLE pairing (ND-0916)

BLE pairing artifacts are stored in NVS. ND-0916 defines the complete NVS layout including both pre-existing keys (`magic`, `key_hint`, `psk`, `channel`, `interval`, `active_p`, `prog_a`, `prog_b`) and pairing-specific keys:

| NVS key | Type | Contents |
|---|---|---|
| `peer_payload` | blob | Encrypted payload for PEER_REQUEST (erased after first WAKE/COMMAND) |
| `reg_complete` | u32 | Registration complete flag (1 = registered with gateway) |

> **Note:** The `key` partition layout in §6.1 is the original design-level storage for PSK/key_hint/magic. The requirements (ND-0916) describe all credential fields as NVS keys. Implementations may use either raw flash partitions or NVS for the core credentials — the Key Store trait (§3.1) abstracts this choice. The BLE-specific keys (`peer_payload`, `reg_complete`) always use NVS. Factory reset erases both (§6.2).

---

## 7  Program store

### 7.1  A/B partitions

Two flash partitions store resident programs. Only one is active at a time. The `schedule` partition contains a flag indicating which is active.

**Update sequence:**
1. Write new program image to the **inactive** partition.
2. Verify SHA-256 hash of written data.
3. Flip the active flag in the schedule partition.
4. The new program is now active.

If the write or hash verification fails, the active partition is untouched — the old program remains.

### 7.2  Program image decoding

After hash verification, the CBOR program image is decoded:

```rust
pub struct ProgramImage {
    pub bytecode: Vec<u8>,
    pub maps: Vec<MapDef>,
}

pub struct MapDef {
    pub map_type: u32,
    pub key_size: u32,
    pub value_size: u32,
    pub max_entries: u32,
}
```

### 7.3  LDDW relocation resolution

The bytecode contains `LDDW src=1, imm=<map_index>` instructions. At load time, the firmware:

1. Allocates map storage in RTC slow SRAM for each `MapDef`.
2. For each `LDDW src=1` instruction, replaces `imm` with the runtime pointer to the corresponding map's storage.

This must happen **before** BPF execution.

### 7.4  Ephemeral programs

Ephemeral programs are stored in RAM (heap allocation), not flash. They are decoded and executed immediately, then the allocation is freed. The resident program is unaffected.

---

## 8  BPF runtime

### 8.1  Interpreter

The BPF interpreter choice is resolved:

The project uses `sonde-bpf`, a custom RFC 9669 compliant interpreter written in pure Rust. It provides zero-allocation execution, tagged register tracking for pointer provenance, and `#![no_std]` compatibility. The firmware wraps the interpreter behind a `BpfInterpreter` trait, so the backend can be changed without affecting the rest of the design.

### 8.1a  BPF interpreter trait

```rust
pub type HelperFn = fn(r1: u64, r2: u64, r3: u64, r4: u64, r5: u64) -> u64;

pub trait BpfInterpreter {
    /// Register a helper function by call number.
    fn register_helper(&mut self, id: u32, func: HelperFn) -> Result<(), BpfError>;

    /// Load bytecode and resolve LDDW src=1 map references.
    /// `map_ptrs` maps map_index → runtime pointer for relocation.
    /// `map_defs` carries MapDef entries for bounds checking.
    fn load(
        &mut self,
        bytecode: &[u8],
        map_ptrs: &[u64],
        map_defs: &[sonde_protocol::MapDef],
    ) -> Result<(), BpfError>;

    /// Execute the loaded program with the given context pointer.
    /// `instruction_budget` limits execution; returns the program's
    /// return value or an error if budget/call-depth is exceeded.
    fn execute(&mut self, ctx_ptr: u64, instruction_budget: u64) -> Result<u64, BpfError>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum BpfError {
    InstructionBudgetExceeded,
    CallDepthExceeded,
    InvalidBytecode(&'static str),
    HelperNotRegistered(u32),
    LoadError(&'static str),
    MapLoadError { index: usize, kind: &'static str },
    RuntimeError(&'static str),
}
```

This trait is defined in the node firmware (not in `sonde-protocol`, since the gateway does not execute BPF). The `sonde-node` crate provides an adapter backed by `sonde-bpf`.

The interpreter runs in bounded mode — an instruction counter enforces the instruction budget. If the budget is exceeded, execution is terminated and the program returns an error.

### 8.2  Helper registration

Each BPF helper is registered with the interpreter by its call number:

| Helper # | Function | Module |
|---|---|---|
| 1 | `i2c_read` | HAL |
| 2 | `i2c_write` | HAL |
| 3 | `i2c_write_read` | HAL |
| 4 | `spi_transfer` | HAL |
| 5 | `gpio_read` | HAL |
| 6 | `gpio_write` | HAL |
| 7 | `adc_read` | HAL |
| 8 | `send` | Wake Cycle Engine |
| 9 | `send_recv` | Wake Cycle Engine |
| 10 | `map_lookup_elem` | Map Storage |
| 11 | `map_update_elem` | Map Storage |
| 12 | `get_time` | System |
| 13 | `get_battery_mv` | System |
| 14 | `delay_us` | System |
| 15 | `set_next_wake` | Sleep Manager |
| 16 | `bpf_trace_printk` | System |

Helper numbers are part of the firmware ABI and MUST NOT change between versions.

### 8.3  Ephemeral restrictions

For ephemeral programs, helpers 11 (`map_update_elem`) and 15 (`set_next_wake`) return an error code without performing any action. This is enforced at runtime by the helper dispatcher, which checks the current program class before executing restricted helpers.

### 8.4  Execution context

Before invoking the BPF program, the firmware populates:

```rust
pub struct SondeContext {
    pub timestamp: u64,              // from gateway timestamp_ms + local elapsed
    pub battery_mv: u16,             // current ADC reading
    pub firmware_abi_version: u16,   // firmware ABI
    pub wake_reason: u8,             // 0x00=scheduled, 0x01=early, 0x02=program_update
}
```

A pointer to this struct is passed as the first argument (R1) to the BPF program.

### 8.5  Communication helpers

`send()` and `send_recv()` are implemented by the wake cycle engine:

1. Increment the session sequence number.
2. Construct APP_DATA frame with the blob and current sequence number.
3. HMAC-sign and transmit.
4. For `send()`: return immediately (do not wait for reply).
5. For `send_recv()`: wait for APP_DATA_REPLY (50 ms timeout). Verify HMAC and echoed sequence number. Return reply blob to BPF program.

Each call increments the sequence number, ensuring independent replay protection per message.

---

## 9  Map storage

### 9.1  RTC slow SRAM layout

Maps are stored in RTC slow SRAM, which survives deep sleep. The layout is determined at program install time from the program image's map definitions.

| Property | Value |
|---|---|
| Total RTC SRAM | 8 KB (C3), 8+8 KB (S3) |
| Usable for maps | ~4–6 KB (after firmware overhead) |
| Allocation | Fixed at program install; no dynamic allocation |
| Lifetime | Survives deep sleep; cleared on factory reset or program change |

### 9.2  Map allocation

On program install:
1. Calculate total map storage needed: `sum(max_entries * (key_size + value_size))` for all maps.
2. If total exceeds the budget → reject installation, keep existing program.
3. Allocate contiguous regions in RTC SRAM for each map.
4. Zero-initialize all map storage.
5. Record the map layout in the RTC SRAM header for use after deep sleep.

### 9.3  Map access helpers

- `map_lookup_elem(map, key)` → pointer to value, or NULL.
- `map_update_elem(map, key, value)` → writes value at key. Only `BPF_MAP_TYPE_ARRAY` is supported (key is an integer index).

Bounds checking is performed on every access: key must be within `[0, max_entries)`.

---

## 10  HAL (Hardware Abstraction Layer)

The HAL wraps ESP-IDF peripheral drivers for BPF helper access:

```rust
pub struct Hal {
    i2c_buses: Vec<I2cDriver>,
    spi_buses: Vec<SpiDriver>,
    // GPIO and ADC accessed via esp-idf-hal functions
}
```

### 10.1  Bus handle encoding

Handles pack bus and address into a single `u32` (matching bpf-environment.md §6.1):

```rust
// I2C: (bus << 16) | 7-bit_addr
// SPI: (bus << 16)
```

### 10.2  Error handling

All HAL helpers return `0` on success, negative on error. Errors include NACK, bus timeout, invalid pin/channel. The BPF program decides how to handle errors — the firmware does not retry.

---

## 11  Sleep manager

### 11.1  Sleep entry

After BPF execution completes:

1. Calculate sleep duration: `min(set_next_wake_value, base_interval)`. If `set_next_wake()` was not called, use `base_interval`.
2. Configure RTC wakeup timer.
3. Enter deep sleep via ESP-IDF `esp_deep_sleep_start()`.

### 11.2  Wake reason determination

On wake, the firmware checks:

| Condition | Wake reason |
|---|---|
| New program just installed (flag in schedule partition) | `WAKE_PROGRAM_UPDATE` (0x02) |
| `set_next_wake()` was called last cycle (flag in RTC SRAM) | `WAKE_EARLY` (0x01) |
| Otherwise | `WAKE_SCHEDULED` (0x00) |

The flag for `WAKE_EARLY` is stored in RTC SRAM and cleared after reading.

---

## 12  Error handling

All inbound protocol errors result in **silent discard** — the node does not send error responses. 

| Error | Behavior |
|---|---|
| HMAC verification failure | Discard frame. |
| Echoed nonce/seq mismatch | Discard frame. |
| Malformed CBOR (ND-0800) | Discard frame. |
| Unexpected `msg_type` (ND-0801) | Discard frame. |
| Mismatched `chunk_index` (ND-0802) | Discard frame, retry GET_CHUNK. |
| Program hash mismatch after transfer | Discard program, sleep. |
| Map budget exceeded on install | Reject installation, keep existing program, sleep. |
| No PSK (unpaired) | Enter BLE pairing mode (ND-0900). |
| BPF instruction budget exceeded | Terminate program, sleep. |
| BPF call depth exceeded | Terminate program, sleep. |
| HAL error (bus timeout, NACK) | Return negative to BPF program. |

---

## 13  Memory budget (ESP32-C3 reference)

| Region | Total | Used by firmware | Available |
|---|---|---|---|
| RAM | 400 KB | ~100 KB (stack, ESP-IDF, wifi) | ~300 KB |
| RTC slow SRAM | 8 KB | ~4 KB (firmware state, flags, layout record) | ~4 KB for maps |
| Flash (program) | 8 KB (2 × 4 KB) | — | 4 KB per program image |
| BPF stack | 4 KB | — | 512 B × 8 frames |
| Ephemeral program | — | — | Allocated from heap (≤ 2 KB) |
| Main task stack (ND-0918) | 16 KB | — | `CONFIG_ESP_MAIN_TASK_STACK_SIZE=16384` |

---

## 14  Boot sequence

1. ESP-IDF initialization (clocks, peripherals, wifi/ESP-NOW).
2. Sample pairing button GPIO for 500 ms (ND-0901).
3. Read key partition: check magic bytes and load credentials if present. Read NVS `reg_complete` flag (§6.1a).
4. Determine boot path (ND-0900):
   a. No valid PSK in key partition OR pairing button held ≥ 500 ms → enter BLE pairing mode (§15). Does not return.
   b. PSK present in key partition, `reg_complete` NOT set in NVS → enter PEER_REQUEST registration (§15.7). Does not return (sleeps after listen window).
   c. PSK present in key partition, `reg_complete` set in NVS → continue to step 5 (normal WAKE cycle).
5. Read schedule partition: load base interval and active program partition flag.
6. Read active program partition: decode CBOR image header, extract program hash.
   - No program → set `program_hash` to zero-length.
7. Initialize HAL (I2C buses, SPI buses, GPIO, ADC).
8. Allocate map storage in RTC SRAM from program's map definitions (if maps survived sleep, data is preserved; if new program, zero-initialize).
9. Resolve LDDW `src=1` instructions in bytecode to runtime map pointers.
10. Enter wake cycle engine.

---

## 15  BLE pairing mode

When the node boots unpaired, or the pairing button is held during boot (ND-0900, ND-0901), the firmware enters BLE pairing mode instead of the wake cycle engine. The entry point is `run_ble_pairing_mode()` in the `esp_ble_pairing` module (compiled only with the `esp` feature).

### 15.1  NimBLE stack

The BLE stack uses the `esp32-nimble` crate, a safe Rust wrapper around the ESP-IDF NimBLE host. Key configuration:

| Setting | Value | Rationale |
|---|---|---|
| `CONFIG_BT_NIMBLE_ENABLED` | `y` | NimBLE is lighter than Bluedroid (ND-0902) |
| `CONFIG_BT_NIMBLE_PINNED_TO_CORE_0` | `y` | Prevents crash on dual-core S3; no-op on unicore C3 |
| `CONFIG_BT_NIMBLE_HOST_TASK_STACK_SIZE` | `7000` | GATT server workload |
| `CONFIG_BT_NIMBLE_NVS_PERSIST` | `n` | No persistent bonds; each session is independent |

### 15.2  GATT service

The Node Provisioning Service exposes a single characteristic:

| UUID | Property | Purpose |
|---|---|---|
| `0xFE50` (service) | — | Node Provisioning Service |
| `0xFE51` (characteristic) | Write + Indicate | NODE_PROVISION (write) / NODE_ACK (indicate) |

GATT writes received before LESC pairing completes are accepted at the ATT level but not processed immediately: the implementation buffers at most one pre-auth write in `pending_write` and defers it until authentication succeeds and the negotiated ATT MTU is ≥ 247 bytes (ND-0904). Writes that cannot be buffered (for example because a pending write is already present or the payload is invalid/too large) are rejected/ignored according to normal ATT error handling. If authentication fails, or if the post-pairing MTU negotiation results in MTU < 247, any buffered write is discarded and the connection is dropped.

### 15.3  Security model

Security is configured as LESC Just Works:

- `AuthReq::all()` — requests SC (Secure Connections) + Bond + MITM.
- `SecurityIOCap::NoInputNoOutput` — downgrades MITM to Just Works while keeping LESC. The effective pairing mode is LESC Just Works (ND-0904); `AuthReq::all()` requests the maximum security level, which is then constrained by the `NoInputNoOutput` I/O capability per BT Core Spec Vol 3 Part H §2.3.5.1.
- The node proactively initiates LESC pairing by calling `ble_gap_security_initiate(conn_handle)` in the `on_connect` callback (ND-0904 criterion 3). This sends an SMP Security Request to the client, ensuring pairing is triggered regardless of client behavior.

This matches the modem's BLE configuration so that the same phone app can pair with both gateway and node endpoints.

### 15.4  Advertising

The node advertises as `sonde-XXXX` where `XXXX` is the last two bytes of the BLE MAC in hex (ND-0903). The advertisement includes the `0xFE50` service UUID for phone-side filtering. The GAP device name is set to the same value via `BLEDevice::set_device_name()` before advertising starts (ND-0903 criterion 3), so connected clients see the correct name instead of the NimBLE default (`nimble`).

### 15.5  Event flow

```
boot → NimBLE init → GATT service register → start advertising
    ↓
phone connects → server calls ble_gap_security_initiate() → LESC pairing → MTU exchange → auth complete
    ↓
buffered GATT write flushed (if any) → handle_node_provision() → NODE_ACK indicate
    ↓
phone disconnects → return → reboot (ND-0907)
```

The main loop polls for pending GATT writes and disconnection events at 100 ms intervals. On disconnect, the function returns and the caller reboots into normal wake-cycle mode with the newly provisioned credentials.

### 15.6  Platform-independent handler

The GATT write payload is parsed and handled by `handle_node_provision()` in the platform-independent `ble_pairing` module (ND-0905, ND-0906, ND-0908). The handler parses the five NODE_PROVISION fields (`node_key_hint`, `node_psk`, `rf_channel`, `payload_len`, `encrypted_payload`), validates `payload_len` before reading `encrypted_payload`, and persists credentials: PSK and key_hint to the `key` flash partition (§6.1), and `channel`, `peer_payload`, `reg_complete` to NVS (§6.1a, ND-0916). The `reg_complete` flag is cleared on successful provision. If any write fails, the handler responds with NODE_ACK(0x02) (ND-0908). This keeps provisioning logic testable on the host (see T-N904–T-N907). The ESP-specific `esp_ble_pairing` module handles only NimBLE initialization, GATT plumbing, and the event loop.

### 15.7  Post-provisioning registration (PEER_REQUEST / PEER_ACK)

When the node boots with a PSK stored but the `reg_complete` flag not set (boot path 2, ND-0900), it enters the PEER_REQUEST registration sub-protocol. This completes the pairing handshake by registering the node with the gateway via the modem.

**Frame construction (ND-0909):**

1. Initialise ESP-NOW on the RF channel stored during provisioning (NVS key `channel`).
2. Load the encrypted payload from NVS (key `peer_payload`).
3. Build a PEER_REQUEST frame:
   - `msg_type` = 0x05.
   - `nonce` = fresh 8-byte random value from the hardware RNG.
   - CBOR payload: `{1: encrypted_payload}`.
   - HMAC-SHA256 computed with `node_psk` (loaded from the key store — see §6.1, §6.1a) over header + payload.

**Transmission and retransmission (ND-0910):**

The node transmits PEER_REQUEST on each boot where `reg_complete` is not set. The retransmission interval follows the normal wake cycle schedule (default 60 s). Each wake cycle re-sends PEER_REQUEST until a valid PEER_ACK is received.

**Listen window (ND-0911):**

After transmitting PEER_REQUEST, the node listens for a PEER_ACK for at least 10 seconds. If no valid PEER_ACK arrives within the listen window, the node enters deep sleep and retries on the next wake.

**PEER_ACK verification (ND-0912):**

On receiving a candidate PEER_ACK frame, the node:

1. Verifies the frame HMAC using `node_psk`.
2. Verifies that the echoed `nonce` matches the nonce sent in the PEER_REQUEST.
3. Computes the expected `registration_proof` as `HMAC-SHA256(node_psk, "sonde-peer-ack-v1" ‖ encrypted_payload)`.
4. Discards the frame if any check fails.

**Registration completion (ND-0913):**

On receiving a valid PEER_ACK, the node sets the `reg_complete` flag in NVS. The `peer_payload` NVS key is retained until the first successful WAKE/COMMAND exchange (ND-0914).

**Deferred payload erasure (ND-0914):**

After the first successful WAKE/COMMAND exchange (the gateway responds with a valid COMMAND), the node erases the `peer_payload` from NVS.

**Self-healing on WAKE failure (ND-0915):**

If WAKE fails (no response or HMAC verification failure) after `reg_complete` is set, the node clears the `reg_complete` flag and reverts to sending PEER_REQUEST on the next boot. This allows the node to re-register if the gateway lost its registration state.

---

## 16  Shared protocol crate (`sonde-protocol`)

The `sonde-protocol` crate is a `no_std`-compatible Rust library shared between the gateway and the node. It contains all wire-format logic so that both sides encode and decode frames identically.

### 16.1  Contents

| Component | Description |
|---|---|
| **Constants** | `msg_type` codes, command codes, CBOR key numbers, frame sizes, HMAC size |
| **Frame codec** | `encode_frame()`, `decode_frame()`, header parsing at fixed offsets |
| **CBOR messages** | `NodeMessage` and `GatewayMessage` enums with typed fields; CBOR encode/decode using integer keys |
| **Program image** | `ProgramImage` and `MapDef` structs; CBOR deterministic encode/decode |
| **HMAC trait** | `HmacProvider` trait — platform provides the implementation |

### 16.2  HMAC trait

```rust
pub trait HmacProvider {
    fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32];
    fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool;
}
```

| Platform | Implementation |
|---|---|
| Gateway | `hmac` + `sha2` crates (RustCrypto, software) |
| Node | ESP-IDF hardware HMAC peripheral |
| Tests | Software implementation (same as gateway) |

### 16.3  `no_std` compatibility

The crate uses `#![no_std]` with `alloc` (for `Vec<u8>` in message types). Both the gateway (std) and the node (ESP-IDF std) can use it. The crate has no platform-specific dependencies — all platform behavior is injected via traits.

---

## 17  Operational logging

### 17.1  Logging framework

The node firmware uses the Rust `log` crate (v0.4) as the logging facade. On ESP-IDF targets, `EspLogger::initialize_default()` routes log output through the ESP-IDF logging system, which writes to UART console. No additional logging dependencies are required.

| Level | Usage |
|---|---|
| `info!` | Normal operational events: boot, wake cycle transitions, frame send/receive, BPF execution, sleep entry |
| `warn!` | Recoverable error conditions: RNG failure, transport timeout, HMAC mismatch, storage I/O errors |
| `error!` | Non-recoverable errors: BPF load/registration failures |
| `debug!` | Verbose diagnostic output: BPF trace output (`bpf_trace_printk`) |

### 17.2  Log points

The following events are logged per the ND-10xx requirements:

| Event | Level | Module | Key fields | Requirement |
|---|---|---|---|---|
| Boot reason | INFO | `bin/node.rs` | `boot_reason` (power_on / deep_sleep_wake) | ND-1000 |
| Wake cycle started | INFO | `wake_cycle.rs` | `key_hint`, `wake_reason` | ND-1001 |
| WAKE frame sent | INFO | `wake_cycle.rs` | `key_hint`, `nonce` | ND-1002 |
| COMMAND received | INFO | `wake_cycle.rs` | `command_type`, `interval_s` (if applicable) | ND-1003 |
| PEER_REQUEST sent | INFO | `peer_request.rs` | `key_hint` | ND-1004 |
| PEER_ACK received | INFO | `peer_request.rs` | registration result | ND-1005 |
| BPF execution | INFO | `wake_cycle.rs` | `program_hash` (truncated), result | ND-1006 |
| Deep sleep entry | INFO | `wake_cycle.rs` | `duration_seconds`, `reason` | ND-1007 |
| BLE pairing mode | INFO | `esp_ble_pairing.rs` | entry/exit (already present) | ND-1008 |
| RNG failure | WARN | `wake_cycle.rs` | — | ND-1009 |
| WAKE retries exhausted | WARN | `wake_cycle.rs` | — | ND-1009 |
| HMAC mismatch | WARN | `wake_cycle.rs` | — | ND-1009 |

### 17.3  Design constraints

- **No heap allocation in error paths.** Log format strings use `&'static str` literals; only field interpolation may allocate (e.g., hex formatting).
- **No log buffering or remote transmission.** All logs go to UART console via ESP-IDF. Remote log collection is out of scope.
- **Log volume.** Each wake cycle emits at most ~5–8 INFO lines. This is acceptable for UART at 115200 baud during the ~100 ms awake window.
