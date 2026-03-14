// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * send — reads the current timestamp from context and sends it to the gateway.
 *
 * Demonstrates the minimal send() pattern: encode a value, call send().
 * Corresponds to send_program in docs/node-validation.md §2.4.
 */

#include "include/sonde_helpers.h"

SEC("sonde")
int program(struct sonde_context *ctx)
{
    __u64 ts = ctx->timestamp;

    /* Encode timestamp as an 8-byte big-endian blob. */
    __u8 blob[8];
    blob[0] = (__u8)(ts >> 56);
    blob[1] = (__u8)(ts >> 48);
    blob[2] = (__u8)(ts >> 40);
    blob[3] = (__u8)(ts >> 32);
    blob[4] = (__u8)(ts >> 24);
    blob[5] = (__u8)(ts >> 16);
    blob[6] = (__u8)(ts >>  8);
    blob[7] = (__u8)(ts);

    send(blob, sizeof(blob));
    return 0;
}
