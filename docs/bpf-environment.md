# BPF Program Environment

> **Document status:** Draft  
> **Scope:** The execution environment, API, and constraints for BPF programs running on Sonde nodes.  
> **Audience:** Developers writing BPF programs for Sonde.  
> **Related:** [gateway-api.md](gateway-api.md) (handler-side API), [protocol.md](protocol.md) (wire protocol)

---

## 1  Overview

A BPF program is the application logic that runs on a Sonde node. It reads sensors, processes data, communicates with a gateway-side handler, and manages persistent state — all without firmware changes.

The developer writes the BPF program in C (or any language that compiles to BPF bytecode), compiles it to an ELF file, and deploys it through the gateway. The firmware provides a stable helper API and manages the execution lifecycle.

---

## 2  Program classes

### 2.1  Resident programs

Resident programs are the steady-state application logic on a node.

| Property | Value |
|---|---|
| **Storage** | Flash (A/B partitions) |
| **Lifetime** | Persistent until replaced by the gateway |
| **Execution** | Runs every wake cycle on schedule |
| **Map access** | Read/write |
| **Helper set** | Full |
| **Side effects** | Allowed (send data, update maps, adjust wake interval) |
| **Max size** | 4 KB (recommended, reference implementation) |

Resident programs implement behaviors like: periodic sampling, threshold detection, data batching, and transmission.

### 2.2  Ephemeral programs

Ephemeral programs are one-shot diagnostics pushed by the gateway.

| Property | Value |
|---|---|
| **Storage** | RAM (discarded after execution) |
| **Lifetime** | Single execution |
| **Execution** | Runs once immediately after transfer |
| **Map access** | Read-only |
| **Helper set** | Limited (no `map_update_elem`, no `set_next_wake`) |
| **Side effects** | None (cannot modify node state) |
| **Max size** | 2 KB (recommended, reference implementation) |

Ephemeral programs are used for remote introspection: dump map contents, read sensor values, or report node state — without disturbing the resident program.

---

## 3  Execution lifecycle

### 3.1  When the program runs

```
Node wakes
  │
  ├── WAKE → Gateway → COMMAND
  │
  ├── (if UPDATE_PROGRAM / RUN_EPHEMERAL: chunked transfer)
  │
  ├── Execute BPF program          ◄── your code runs here
  │     ├── read sensors
  │     ├── update maps (resident only)
  │     ├── send() / send_recv()
  │     └── set_next_wake() (resident only)
  │
  └── Sleep
```

The program runs **once per wake cycle**. It is not a long-running process — it executes, performs its work, and returns. The firmware handles sleep, wake, and protocol.

### 3.2  Entry point

The BPF program's entry point receives a pointer to the execution context (see §4). The return value is currently unused (reserved for future use; return 0).

```c
int program(struct sonde_context *ctx) {
    // ... your logic ...
    return 0;
}
```

### 3.3  Execution constraints

| Constraint | Resident | Ephemeral |
|---|---|---|
| **Loops** | Bounded (verifier-checked) | None or tightly bounded |
| **Instruction budget** | Larger (platform-dependent) | Small (platform-dependent) |
| **Stack size** | Limited (512 bytes typical for BPF) | Same |
| **Execution time** | Bounded by instruction budget | Same |

All programs are verified by [Prevail](https://github.com/vbpf/ebpf-verifier) before loading. Programs that fail verification are rejected by the gateway and never reach the node.

---

## 4  Execution context

The firmware passes a read-only context structure to the program on each invocation. This provides metadata about the current wake cycle.

```c
struct sonde_context {
    uint64_t timestamp;           // current time (milliseconds since epoch)
    uint16_t battery_mv;          // battery voltage in millivolts
    uint16_t firmware_abi_version; // firmware ABI version
    uint8_t  wake_reason;         // why the node woke (see below)
};
```

### Wake reasons

| Value | Name | Description |
|---|---|---|
| `0x00` | `WAKE_SCHEDULED` | Normal scheduled wake. |
| `0x01` | `WAKE_EARLY` | Woke early due to prior `set_next_wake()` call. |
| `0x02` | `WAKE_PROGRAM_UPDATE` | New program was just installed. First execution. |

---

## 5  Memory model

BPF programs have access to four memory regions with different lifetimes:

### 5.1  Context (read-only, per-wake)

The `sonde_context` structure (§4). Populated by firmware before each invocation. The program cannot modify it.

### 5.2  Scratch (volatile)

The BPF program's stack and any local variables. Lost on sleep. Used for working memory during a single execution.

| Property | Value |
|---|---|
| Stack size | 512 bytes (typical BPF limit) |
| Lifetime | Single execution |

### 5.3  Maps (sleep-persistent)

Key-value stores that survive deep sleep. Used for accumulating readings, maintaining state across wake cycles, storing calibration data, etc.

| Property | Value |
|---|---|
| Persistence | Survives deep sleep (backed by RTC slow SRAM) |
| Capacity | ~4–6 KB usable (platform-dependent) |
| Access | Read/write for resident programs, read-only for ephemeral |
| Definition | Declared in the BPF ELF using standard BPF map definitions |

#### Map types

| Type | Description |
|---|---|
| `BPF_MAP_TYPE_ARRAY` | Fixed-size array indexed by integer key. |
| `BPF_MAP_TYPE_HASH` | Key-value hash map. |

#### Map definition (in BPF C)

```c
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 16);
    __type(key, uint32_t);
    __type(value, struct sensor_reading);
} readings SEC(".maps");
```

Maps are defined in the ELF and delivered alongside the program. The firmware allocates map storage on program install and enforces the platform's memory budget. If the new program's maps exceed available sleep-persistent memory, installation fails.

### 5.4  Flash (permanent)

The resident program binary and schedule configuration. Managed by firmware — the BPF program cannot directly access flash.

| Property | Value |
|---|---|
| Program storage | A/B partitions for atomic updates |
| Flash endurance | ~100K erase cycles per 4 KB sector |

---

## 6  Helper API

The firmware exposes the following helpers to BPF programs. The ABI remains stable across firmware versions.

### 6.1  Bus access

The firmware provides raw bus primitives. Sensor-specific protocols (register sequences, calibration, timing) are encoded in the BPF program — not the firmware. This means adding new sensor types never requires a firmware update.

#### `i2c_read`

```c
int i2c_read(uint8_t bus, uint8_t addr, void *buf, uint32_t buf_len);
```

Read bytes from an I2C device.

| Parameter | Description |
|---|---|
| `bus` | I2C bus index (platform-dependent). |
| `addr` | 7-bit I2C device address. |
| `buf` | Buffer to read into. |
| `buf_len` | Number of bytes to read. |

**Returns:** `0` on success, negative on failure (NACK, bus error, timeout).

**Availability:** Resident and ephemeral.

#### `i2c_write`

```c
int i2c_write(uint8_t bus, uint8_t addr, const void *data, uint32_t data_len);
```

Write bytes to an I2C device.

| Parameter | Description |
|---|---|
| `bus` | I2C bus index. |
| `addr` | 7-bit I2C device address. |
| `data` | Data to write. |
| `data_len` | Number of bytes to write. |

**Returns:** `0` on success, negative on failure.

**Availability:** Resident and ephemeral.

#### `i2c_write_read`

```c
int i2c_write_read(uint8_t bus, uint8_t addr,
                   const void *write_ptr, uint32_t write_len,
                   void *read_ptr, uint32_t read_len);
```

Write bytes then read bytes in a single I2C transaction (repeated start). This is the common pattern for reading a register: write the register address, then read the value.

| Parameter | Description |
|---|---|
| `bus` | I2C bus index. |
| `addr` | 7-bit I2C device address. |
| `write_ptr` | Data to write (typically a register address). |
| `write_len` | Number of bytes to write. |
| `read_ptr` | Buffer to read into. |
| `read_len` | Number of bytes to read. |

**Returns:** `0` on success, negative on failure.

**Availability:** Resident and ephemeral.

#### `spi_transfer`

```c
int spi_transfer(uint8_t bus, const void *tx, void *rx, uint32_t len);
```

Full-duplex SPI transfer. Simultaneously transmits and receives `len` bytes.

| Parameter | Description |
|---|---|
| `bus` | SPI bus index (platform-dependent). |
| `tx` | Transmit buffer (can be NULL for read-only). |
| `rx` | Receive buffer (can be NULL for write-only). |
| `len` | Number of bytes to transfer. |

**Returns:** `0` on success, negative on failure.

**Availability:** Resident and ephemeral.

#### `gpio_read`

```c
int gpio_read(uint8_t pin);
```

Read the state of a GPIO pin.

| Parameter | Description |
|---|---|
| `pin` | GPIO pin number (platform-dependent). |

**Returns:** `0` (low) or `1` (high), negative on failure (invalid pin).

**Availability:** Resident and ephemeral.

#### `gpio_write`

```c
int gpio_write(uint8_t pin, uint8_t value);
```

Set the state of a GPIO pin.

| Parameter | Description |
|---|---|
| `pin` | GPIO pin number. |
| `value` | `0` (low) or `1` (high). |

**Returns:** `0` on success, negative on failure.

**Availability:** Resident and ephemeral.

#### `adc_read`

```c
int adc_read(uint8_t channel, uint32_t *value);
```

Read a raw value from an ADC channel.

| Parameter | Description |
|---|---|
| `channel` | ADC channel index (platform-dependent). |
| `value` | Pointer to store the raw ADC reading. |

**Returns:** `0` on success, negative on failure.

**Availability:** Resident and ephemeral.

---

### 6.2  Communication

#### `send`

```c
int send(const void *ptr, uint32_t len);
```

Emit an `APP_DATA` message to the gateway. Fire-and-forget — the node does not wait for a reply.

| Parameter | Description |
|---|---|
| `ptr` | Pointer to the data blob. |
| `len` | Length of the data in bytes. |

**Returns:** `0` on success, negative on failure (e.g., exceeds frame size).

**Availability:** Resident and ephemeral.

#### `send_recv`

```c
int send_recv(const void *ptr, uint32_t len,
              void *reply_buf, uint32_t reply_len,
              uint32_t timeout_ms);
```

Send an `APP_DATA` message and block until `APP_DATA_REPLY` arrives or the timeout expires.

| Parameter | Description |
|---|---|
| `ptr` | Pointer to the outbound data blob. |
| `len` | Length of the outbound data in bytes. |
| `reply_buf` | Buffer to write the reply into. |
| `reply_len` | Size of the reply buffer in bytes. |
| `timeout_ms` | How long to wait for the reply in milliseconds. |

**Returns:** Number of bytes received on success (may be 0 for an empty reply), negative on timeout or error.

**Availability:** Resident and ephemeral.

---

### 6.3  Map operations

#### `map_lookup_elem`

```c
void *map_lookup_elem(uint32_t map_id, const void *key);
```

Look up a value in a BPF map.

| Parameter | Description |
|---|---|
| `map_id` | Map identifier (index in the ELF map section). |
| `key` | Pointer to the key. |

**Returns:** Pointer to the value on success, `NULL` if the key is not found.

**Availability:** Resident and ephemeral.

#### `map_update_elem`

```c
int map_update_elem(uint32_t map_id, const void *key, const void *value);
```

Insert or update a key-value pair in a BPF map.

| Parameter | Description |
|---|---|
| `map_id` | Map identifier. |
| `key` | Pointer to the key. |
| `value` | Pointer to the value. |

**Returns:** `0` on success, negative on failure (map full, key not found for array type).

**Availability:** Resident only. Ephemeral programs cannot modify maps.

---

### 6.4  System

#### `get_time`

```c
uint64_t get_time(void);
```

Get the current time in milliseconds since epoch.

**Availability:** Resident and ephemeral.

#### `get_battery_mv`

```c
uint16_t get_battery_mv(void);
```

Get the current battery voltage in millivolts. Same value as `ctx->battery_mv` but accessible without the context pointer.

**Availability:** Resident and ephemeral.

#### `set_next_wake`

```c
int set_next_wake(uint32_t seconds);
```

Request an earlier wake than the gateway-configured schedule. The node will wake at `min(set_next_wake value, gateway interval)` — the BPF program can request earlier wakes but cannot extend beyond the gateway's interval.

| Parameter | Description |
|---|---|
| `seconds` | Seconds until the next wake. |

**Returns:** `0` on success.

**Availability:** Resident only. Ephemeral programs cannot modify the schedule.

---

### 6.5  Debug

#### `bpf_trace_printk`

```c
int bpf_trace_printk(const char *fmt, uint32_t fmt_len, ...);
```

Emit a debug trace message. Output is platform-dependent (serial console, log buffer, etc.). Not intended for production use.

**Availability:** Resident and ephemeral.

---

## 7  Verification profiles

All programs are verified by [Prevail](https://github.com/vbpf/ebpf-verifier) on the gateway before distribution. Two profiles enforce different safety guarantees:

| Property | Resident | Ephemeral |
|---|---|---|
| **Loops** | Bounded | None or tightly bounded |
| **Map access** | Read/write | Read-only |
| **Instruction budget** | Larger | Small |
| **Helper set** | Full | Limited (`send`, `send_recv`, `i2c_read`, `i2c_write`, `i2c_write_read`, `spi_transfer`, `gpio_read`, `adc_read`, `map_lookup_elem`, `get_time`, `get_battery_mv`, `bpf_trace_printk`) |
| **Side effects** | Allowed | None |

A program that fails verification is rejected with a diagnostic explaining why. It never reaches the node.

---

## 8  Development workflow

BPF programs are platform-agnostic. The development cycle does not require hardware.

### 8.1  Compile

Compile C to BPF ELF using clang:

```bash
clang -target bpf -O2 -c my_program.c -o my_program.o
```

### 8.2  Verify

Run the Prevail verifier locally to check the program before deployment:

```bash
prevail my_program.o --profile resident
```

### 8.3  Test locally

Execute the program locally using uBPF with mock sensor data and maps:

```bash
ubpf_run my_program.o --context mock_context.bin --maps mock_maps.bin
```

Use `bpf_trace_printk` for debug output during local testing.

### 8.4  Deploy

Provide the ELF file to the gateway for distribution. The gateway verifies the program (Prevail), computes its hash, and distributes it to the assigned nodes on their next wake.

---

## 9  Platform constraints (reference implementation)

These values are specific to the ESP32-C3/S3 reference implementation. Other platforms may differ.

| Constraint | ESP32-C3 | ESP32-S3 |
|---|---|---|
| **Sleep-persistent memory** | 8 KB RTC slow SRAM | 8+8 KB RTC slow SRAM |
| **Usable map storage** | ~4 KB | ~6 KB |
| **RAM** | 400 KB (16 KB cache) | 512 KB |
| **BPF execution** | Interpreter only | Interpreter only |
| **Max resident program** | 4 KB | 4 KB |
| **Max ephemeral program** | 2 KB | 2 KB |
| **APP_DATA payload** | ~190 bytes per frame | ~190 bytes per frame |
