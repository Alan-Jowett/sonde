// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * send_recv — sends a request to the gateway and waits for a reply.
 *
 * Demonstrates the request-response pattern using send_recv().  On success
 * the reply is echoed back to the gateway via send().  On timeout or error
 * a single-byte 0xFF error marker is sent instead.
 *
 * Corresponds to send_recv_program in docs/node-validation.md §2.4.
 */

#include "include/sonde_helpers.h"

/** Application-level opcode: "request a sensor reading from the handler". */
#define OP_READ_SENSOR 0x01u

/** Maximum reply size the program is prepared to handle. */
#define REPLY_BUF_LEN  32u

/** Milliseconds to wait for a gateway reply before giving up. */
#define TIMEOUT_MS     1000u

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;

    __u8 req[1]            = { OP_READ_SENSOR };
    __u8 reply[REPLY_BUF_LEN];

    int rc = send_recv(req, sizeof(req), reply, sizeof(reply), TIMEOUT_MS);
    if (rc < 0) {
        /*
         * send_recv() returns a negative value on any failure (timeout,
         * transport error).  Report a single-byte generic error marker to
         * the gateway rather than encoding the return value, since the ABI
         * does not currently distinguish failure reasons.
         */
        __u8 err[1] = { 0xFFu };
        send(err, sizeof(err));
        return 0;
    }

    /* Echo the reply to the gateway as an acknowledgement.
     * Clamp rc to the buffer capacity as a defence-in-depth measure
     * in case a future ABI change allows send_recv() to report a
     * length exceeding the provided capacity.
     */
    if (rc > 0) {
        __u32 len = (__u32)rc;
        if (len > sizeof(reply))
            len = sizeof(reply);
        send(reply, len);
    }

    return 0;
}
