# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors
#
# Build sonde BPF test programs.
#
# Requirements: clang with BPF target support (version 14 or later).
#
# Usage:
#   make          -- compile all .c files to BPF ELF object files
#   make <name>.o -- compile a single program (e.g. make nop.o)
#   make clean    -- remove all compiled objects
#
# After compiling, the resulting ELF object files must be converted to
# CBOR program images before they can be ingested.  The gateway's ELF
# ingestion path (GW-0400) performs this conversion; the sonde-admin
# CLI currently accepts only CBOR-encoded images (not raw ELF).
#
# Workflow: compile → ingest ELF via gateway (GW-0400 converts to CBOR
# internally) → gateway verifies and stores.  See the gateway design
# doc for the ingestion API details.

CLANG  ?= clang
CFLAGS := -target bpf -O2 -Wall -Wextra -I.

SRCS   := nop.c \
          send.c \
          send_recv.c \
          map.c \
          early_wake.c \
          oversized_map.c \
          deep_call.c \
          budget_exceeded.c \
          i2c_sensor.c

OBJS   := $(SRCS:.c=.o)

.PHONY: all clean

all: $(OBJS)

%.o: %.c include/sonde_helpers.h
	$(CLANG) $(CFLAGS) -c $< -o $@

clean:
	rm -f $(OBJS)
