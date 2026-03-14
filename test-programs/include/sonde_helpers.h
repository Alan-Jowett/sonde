// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * sonde_helpers.h — Helper API for sonde BPF programs.
 *
 * Include this header in every BPF C source file.  It provides:
 *   - The sonde_context struct (passed to the program entry point as R1)
 *   - All helper function declarations (called via BPF_CALL instructions)
 *   - Map definition macros (BPF_MAP_TYPE_ARRAY, __uint, __type)
 *   - I2C/SPI handle encoding macros
 *   - Wake reason constants
 *
 * Helper IDs are stable across firmware versions (part of the ABI).
 * See docs/bpf-environment.md for the full specification.
 */

#ifndef SONDE_HELPERS_H
#define SONDE_HELPERS_H

/* -------------------------------------------------------------------------
 * Fixed-width integer types (no system headers required for BPF targets)
 * ---------------------------------------------------------------------- */

typedef unsigned char           __u8;
typedef unsigned short          __u16;
typedef unsigned int            __u32;
typedef unsigned long long      __u64;
typedef signed int              __s32;
typedef signed long long        __s64;

/* -------------------------------------------------------------------------
 * ELF section annotation
 * ---------------------------------------------------------------------- */

#ifndef SEC
#define SEC(name) __attribute__((section(name)))
#endif

/* -------------------------------------------------------------------------
 * BPF-to-BPF call helpers
 * ---------------------------------------------------------------------- */

/** Prevent the compiler from inlining a function (forces a BPF_CALL insn). */
#define __noinline __attribute__((noinline))

/* -------------------------------------------------------------------------
 * BPF map type constants
 * ---------------------------------------------------------------------- */

/* Sonde uses map_type = 1 for ARRAY (not the Linux BPF value of 2).
 * The node firmware rejects any other value; see map_storage.rs. */
#define BPF_MAP_TYPE_ARRAY 1

/* -------------------------------------------------------------------------
 * BPF map definition macros (subset of libbpf's bpf_helpers.h)
 * ---------------------------------------------------------------------- */

/** Declare an unsigned integer field with a constant value in a map struct. */
#define __uint(name, val)  int (*name)[val]

/** Declare a typed field in a map struct. */
#define __type(name, val)  typeof(val) *name

/* -------------------------------------------------------------------------
 * sonde_context — execution context passed to BPF programs as R1.
 *
 * The context is read-only from the BPF program's perspective.  It
 * reflects conditions at the start of the current wake cycle.
 *
 * Matches the Rust `SondeContext` struct in
 * crates/sonde-node/src/bpf_helpers.rs.
 * ---------------------------------------------------------------------- */

struct sonde_context {
    __u64 timestamp;             /**< UTC time in milliseconds since Unix epoch */
    __u16 battery_mv;            /**< Battery voltage in millivolts             */
    __u16 firmware_abi_version;  /**< Current firmware ABI version               */
    __u8  wake_reason;           /**< Why the node woke (see WAKE_* constants)  */
    __u8  _padding[3];           /**< Explicit padding; must be zero            */
};
/* Total size: 8 + 2 + 2 + 1 + 3 = 16 bytes; 8-byte aligned.
 * The BPF interpreter bounds-checks R1 accesses against this size. */

/** Normal scheduled wake. */
#define WAKE_SCHEDULED      0x00u

/** Woke early due to a prior set_next_wake() call. */
#define WAKE_EARLY          0x01u

/** A new program was just installed; this is its first execution. */
#define WAKE_PROGRAM_UPDATE 0x02u

/* -------------------------------------------------------------------------
 * Handle encoding — packs bus + address into a single uint32_t.
 *
 * BPF helpers accept at most five arguments, so bus and device address
 * are combined into one word.  See docs/bpf-environment.md §6.1.
 * ---------------------------------------------------------------------- */

/** Build an I2C handle: (bus << 16) | (addr & 0x7F).  Masks addr to 7 bits. */
#define I2C_HANDLE(bus, addr) ((__u32)(((__u32)(bus) << 16) | ((__u32)(addr) & 0x7Fu)))

/** Build a SPI handle: (bus << 16). */
#define SPI_HANDLE(bus)       ((__u32)((__u32)(bus) << 16))

/* -------------------------------------------------------------------------
 * Helper function declarations.
 *
 * Each entry is a function pointer initialised to its helper call number.
 * The clang BPF back-end lowers a call through such a pointer to a
 * BPF_CALL instruction with the matching immediate value.
 *
 * Helper call numbers MUST match helper_ids in
 * crates/sonde-node/src/bpf_helpers.rs.
 *
 * The __unused attribute suppresses -Wunused-variable for helpers that
 * a given program does not call (e.g. nop.c uses none of them).
 *
 * NOTE on gateway verification: the gateway currently verifies ELF files
 * using Prevail's LinuxPlatform (crates/sonde-gateway/src/program.rs).
 * Linux BPF assigns different semantics to helper IDs 1–16 than sonde does
 * (e.g. Linux helper 1 = map_lookup_elem; Sonde helper 1 = i2c_read).
 * Until the gateway switches to a Sonde-specific Prevail platform, programs
 * that call these helpers will be verified under Linux helper semantics,
 * which may produce incorrect verification results.  These programs are
 * provided primarily as compilation examples and for use once a Sonde
 * Prevail platform is in place (GW-0400/GW-0401).
 * ---------------------------------------------------------------------- */

/* Suppress -Wunused-variable for helpers not called by a given program. */
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wunused-variable"

/**
 * i2c_read — read bytes from an I2C device.
 *
 * @handle:   I2C_HANDLE(bus, 7-bit-addr)
 * @buf:      destination buffer
 * @buf_len:  number of bytes to read
 * Returns:   0 on success, negative on error (NACK, timeout, …)
 */
static int (*i2c_read)(__u32 handle, void *buf, __u32 buf_len) = (void *)1;

/**
 * i2c_write — write bytes to an I2C device.
 *
 * @handle:   I2C_HANDLE(bus, 7-bit-addr)
 * @data:     source buffer
 * @data_len: number of bytes to write
 * Returns:   0 on success, negative on error
 */
static int (*i2c_write)(__u32 handle, const void *data, __u32 data_len) = (void *)2;

/**
 * i2c_write_read — write then read in a single I2C transaction (repeated start).
 *
 * The canonical pattern for reading a sensor register: write the register
 * address, then immediately read the value without releasing the bus.
 *
 * @handle:    I2C_HANDLE(bus, 7-bit-addr)
 * @write_ptr: bytes to write (typically a register address)
 * @write_len: number of bytes to write
 * @read_ptr:  destination buffer
 * @read_len:  number of bytes to read
 * Returns:    0 on success, negative on error
 */
static int (*i2c_write_read)(__u32 handle,
                             const void *write_ptr, __u32 write_len,
                             void *read_ptr, __u32 read_len) = (void *)3;

/**
 * spi_transfer — full-duplex SPI transfer.
 *
 * Simultaneously transmits and receives @len bytes.  Pass NULL for @tx to
 * perform a receive-only transfer; pass NULL for @rx to perform a
 * transmit-only transfer.
 *
 * @handle: SPI_HANDLE(bus)
 * @tx:     transmit buffer (NULL for receive-only)
 * @rx:     receive buffer  (NULL for transmit-only)
 * @len:    number of bytes
 * Returns: 0 on success, negative on error
 */
static int (*spi_transfer)(__u32 handle,
                           const void *tx, void *rx,
                           __u32 len) = (void *)4;

/**
 * gpio_read — read the digital state of a GPIO pin.
 *
 * @pin:    platform GPIO pin number
 * Returns: 0 (low), 1 (high), or negative on error (invalid pin)
 */
static int (*gpio_read)(__u32 pin) = (void *)5;

/**
 * gpio_write — set the digital state of a GPIO pin.
 *
 * @pin:    platform GPIO pin number
 * @value:  0 (low) or 1 (high)
 * Returns: 0 on success, negative on error
 */
static int (*gpio_write)(__u32 pin, __u32 value) = (void *)6;

/**
 * adc_read — read a raw value from an ADC channel.
 *
 * @channel: platform ADC channel index
 * Returns:  raw ADC reading on success, negative on error (invalid channel,
 *           hardware fault)
 *
 * This is the authoritative ABI: single argument, reading returned in R0.
 * See bpf_dispatch.rs::helper_adc_read for the implementation.
 */
static int (*adc_read)(__u32 channel) = (void *)7;

/**
 * send — fire-and-forget APP_DATA message to the gateway.
 *
 * @ptr: pointer to data blob
 * @len: length of the data blob in bytes
 * Returns: 0 on success, negative on error (e.g. payload too large)
 */
static int (*send)(const void *ptr, __u32 len) = (void *)8;

/**
 * send_recv — send APP_DATA and block until APP_DATA_REPLY arrives.
 *
 * @ptr:        outbound data blob
 * @len:        length of outbound blob in bytes
 * @reply_buf:  buffer to write the reply into
 * @reply_len:  capacity of reply_buf in bytes
 * @timeout_ms: milliseconds to wait for a reply (max 5000)
 * Returns:     number of bytes received on success (may be 0),
 *              negative on timeout or error
 */
static int (*send_recv)(const void *ptr, __u32 len,
                        void *reply_buf, __u32 reply_len,
                        __u32 timeout_ms) = (void *)9;

/**
 * map_lookup_elem — look up a key in a BPF map.
 *
 * @map: pointer to the map (resolved from ELF relocation by the loader)
 * @key: pointer to the key
 * Returns: pointer to the value on success, NULL if not found
 */
static void *(*map_lookup_elem)(void *map, const void *key) = (void *)10;

/**
 * map_update_elem — insert or update a key-value pair in a BPF map.
 *
 * Available to resident programs only.  Ephemeral programs cannot modify
 * maps.
 *
 * @map:   pointer to the map (resolved from ELF relocation by the loader)
 * @key:   pointer to the key
 * @value: pointer to the value
 * Returns: 0 on success, negative on failure (e.g. index out of range)
 */
static int (*map_update_elem)(void *map,
                              const void *key,
                              const void *value) = (void *)11;

/**
 * get_time — current UTC time in milliseconds since the Unix epoch.
 *
 * Derived from the gateway's timestamp_ms (received in COMMAND) plus local
 * elapsed time since COMMAND was processed.
 *
 * Returns: milliseconds since 1970-01-01T00:00:00Z
 */
static __u64 (*get_time)(void) = (void *)12;

/**
 * get_battery_mv — current battery voltage in millivolts.
 *
 * Returns the same value as ctx->battery_mv but accessible without the
 * context pointer.
 *
 * Returns: battery voltage in millivolts
 */
static __u16 (*get_battery_mv)(void) = (void *)13;

/**
 * delay_us — busy-wait for the specified number of microseconds.
 *
 * Used for sensor timing (ADC conversion windows, I2C device ready
 * delays, etc.).  Maximum delay is 1 000 000 µs (1 second).
 *
 * @microseconds: duration to wait (max 1 000 000)
 * Returns: 0 on success, -1 if the duration exceeds the maximum
 */
static int (*delay_us)(__u32 microseconds) = (void *)14;

/**
 * set_next_wake — request an earlier wake than the gateway-configured interval.
 *
 * The node will wake at min(requested, gateway_interval).  A BPF program
 * can only shorten the interval, never extend it.
 *
 * Available to resident programs only.
 *
 * @seconds: seconds until the next wake
 * Returns: 0 on success, -1 on error (e.g. called from ephemeral program)
 */
static int (*set_next_wake)(__u32 seconds) = (void *)15;

/**
 * bpf_trace_printk — emit a debug trace message.
 *
 * Output is platform-dependent (serial console or ring buffer).  Not
 * intended for production use.
 *
 * @fmt:     pointer to the message byte slice (need not be null-terminated)
 * @fmt_len: number of bytes to log (do NOT include a trailing null)
 * Returns: 0 on success, -1 on error
 *
 * NOTE: Varargs are accepted by this declaration for C compatibility but
 * are ignored by the firmware implementation — only `fmt` and `fmt_len`
 * are used.
 */
static int (*bpf_trace_printk)(const char *fmt, __u32 fmt_len, ...) = (void *)16;

#pragma GCC diagnostic pop

#endif /* SONDE_HELPERS_H */
