# Node Design Specification

> **Document status:** Draft  
> **Scope:** Architecture and internal design of the Sonde node firmware.  
> **Audience:** Implementers (human or LLM agent) building the node firmware.  
> **Related:** [node-requirements.md](node-requirements.md), [protocol.md](protocol.md), [security.md](security.md), [bpf-environment.md](bpf-environment.md)

---

## 1  Overview

The node firmware is a single Rust binary targeting ESP32-C3 (RISC-V) and ESP32-S3 (Xtensa) via ESP-IDF bindings. It implements a simple cyclic state machine:

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
| BPF interpreter | **⚠ OPEN** — `rbpf` (pure Rust) or `uBPF` (C, via FFI). Both require extension for BPF-to-BPF function calls. |
| CBOR | Via `sonde-protocol` (`ciborium`) | serde-compatible; matches protocol crate implementation |
| HMAC | ESP-IDF hardware HMAC peripheral (implements `sonde-protocol::HmacProvider` trait) | Hardware-accelerated; ~10x faster than software |
| SHA-256 | ESP-IDF hardware SHA peripheral | Hardware-accelerated; used for program hash verification |
| RNG | ESP-IDF hardware TRNG | True random number generator; used for WAKE nonce |
| Toolchain | Upstream Rust (C3) / `espup` (S3) | C3 is RISC-V (upstream); S3 is Xtensa (custom toolchain) |

---

## 3  Module architecture

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
| **Key Store** | PSK storage in dedicated flash partition, pairing, factory reset | ND-0400–0402 |
| **Program Store** | A/B flash partitions, program image decoding, LDDW resolution | ND-0500–0503, ND-0501a |
| **BPF Runtime** | rbpf interpreter, helper dispatch, execution constraints | ND-0504–0506, ND-0600–0606 |
| **Map Storage** | Sleep-persistent maps in RTC slow SRAM | ND-0603, ND-0606 |
| **HAL** | I2C, SPI, GPIO, ADC bus access for BPF helpers | ND-0601 |
| **Sleep Manager** | Deep sleep entry, wake interval, RTC memory management | ND-0203 |
| **Auth** | HMAC-SHA256 (hardware), nonce generation, response verification | ND-0300–0304 |

---

## 4  Wake cycle engine

The wake cycle engine is the central state machine. It runs once per wake and then the node sleeps.

### 4.1  State machine

```
┌─────────┐
│  BOOT   │
└────┬────┘
     │ read PSK from key store
     │ (if no PSK → sleep indefinitely)
     ▼
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

1. **Boot/wake**: Initialize hardware. Read PSK from key store. If no PSK, sleep indefinitely.
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

Factory reset erases:
1. `key` partition (PSK + key_hint + magic → all 0xFF).
2. RTC slow SRAM (map data → zeroed).
3. Both program partitions (`program_a`, `program_b` → erased).
4. Schedule partition → reset to default interval.

After reset, the magic bytes are missing → firmware detects unpaired state → sleeps indefinitely until USB pairing.

### 6.3  USB pairing

The provisioning tool writes the PSK, key_hint, and magic bytes to the `key` partition over USB serial. The firmware does not participate in key generation — it simply stores what the tool provides and reboots into normal operation.

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

**⚠ OPEN:** The BPF interpreter choice is an open design decision:

| Option | Pros | Cons |
|---|---|---|
| `rbpf` (Rust) | Pure Rust, no FFI, same language as firmware | Less established; needs BPF-to-BPF call extension |
| `uBPF` (C, via FFI) | Larger ecosystem, used by eBPF for Windows | Requires unsafe FFI; needs BPF-to-BPF call extension |

Both options require extension to support BPF-to-BPF function calls (max 8 call frames, 512 bytes stack each). The firmware wraps whichever interpreter behind a `BpfInterpreter` trait, so the choice can be changed without affecting the rest of the design.

### 8.1a  BPF interpreter trait

```rust
pub type HelperFn = fn(r1: u64, r2: u64, r3: u64, r4: u64, r5: u64) -> u64;

pub trait BpfInterpreter {
    /// Register a helper function by call number.
    fn register_helper(&mut self, id: u32, func: HelperFn) -> Result<(), BpfError>;

    /// Load bytecode and resolve LDDW src=1 map references.
    /// `map_ptrs` maps map_index → runtime pointer for relocation.
    fn load(&mut self, bytecode: &[u8], map_ptrs: &[u64]) -> Result<(), BpfError>;

    /// Execute the loaded program with the given context pointer.
    /// `instruction_budget` limits execution; returns the program's
    /// return value or an error if budget/call-depth is exceeded.
    fn execute(
        &mut self,
        ctx_ptr: u64,
        instruction_budget: u64,
    ) -> Result<u64, BpfError>;
}

#[derive(Debug)]
pub enum BpfError {
    InstructionBudgetExceeded,
    CallDepthExceeded,
    InvalidBytecode,
    HelperNotRegistered(u32),
    LoadError(String),
}
```

This trait is defined in the node firmware (not in `sonde-protocol`, since the gateway does not execute BPF). Both `rbpf` and `uBPF` adapters implement it.

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
| Malformed CBOR | Discard frame. |
| Unexpected `msg_type` | Discard frame. |
| Mismatched `chunk_index` | Discard frame, retry GET_CHUNK. |
| Program hash mismatch after transfer | Discard program, sleep. |
| Map budget exceeded on install | Reject installation, keep existing program, sleep. |
| No PSK (unpaired) | Sleep indefinitely. |
| BPF instruction budget exceeded | Terminate program, sleep. |
| BPF call depth exceeded | Terminate program, sleep. |
| HAL error (bus timeout, NACK) | Return negative to BPF program. |

---

## 13  Memory budget (ESP32-C3 reference)

| Region | Total | Used by firmware | Available |
|---|---|---|---|
| RAM | 400 KB | ~100 KB (stack, ESP-IDF, wifi) | ~300 KB |
| RTC slow SRAM | 8 KB | ~2 KB (firmware state, flags) | ~6 KB for maps |
| Flash (program) | 8 KB (2 × 4 KB) | — | 4 KB per program image |
| BPF stack | 4 KB | — | 512 B × 8 frames |
| Ephemeral program | — | — | Allocated from heap (≤ 2 KB) |

---

## 14  Boot sequence

1. ESP-IDF initialization (clocks, peripherals, wifi/ESP-NOW).
2. Read key partition: check magic bytes.
   - No magic → unpaired. Log. Sleep indefinitely.
   - Magic present → load PSK and key_hint.
3. Read schedule partition: load base interval and active program partition flag.
4. Read active program partition: decode CBOR image header, extract program hash.
   - No program → set `program_hash` to zero-length.
5. Initialize HAL (I2C buses, SPI buses, GPIO, ADC).
6. Allocate map storage in RTC SRAM from program's map definitions (if maps survived sleep, data is preserved; if new program, zero-initialize).
7. Resolve LDDW `src=1` instructions in bytecode to runtime map pointers.
8. Enter wake cycle engine.

---

## 15  Shared protocol crate (`sonde-protocol`)

The `sonde-protocol` crate is a `no_std`-compatible Rust library shared between the gateway and the node. It contains all wire-format logic so that both sides encode and decode frames identically.

### 15.1  Contents

| Component | Description |
|---|---|
| **Constants** | `msg_type` codes, command codes, CBOR key numbers, frame sizes, HMAC size |
| **Frame codec** | `encode_frame()`, `decode_frame()`, header parsing at fixed offsets |
| **CBOR messages** | `NodeMessage` and `GatewayMessage` enums with typed fields; CBOR encode/decode using integer keys |
| **Program image** | `ProgramImage` and `MapDef` structs; CBOR deterministic encode/decode |
| **HMAC trait** | `HmacProvider` trait — platform provides the implementation |

### 15.2  HMAC trait

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

### 15.3  `no_std` compatibility

The crate uses `#![no_std]` with `alloc` (for `Vec<u8>` in message types). Both the gateway (std) and the node (ESP-IDF std) can use it. The crate has no platform-specific dependencies — all platform behavior is injected via traits.
