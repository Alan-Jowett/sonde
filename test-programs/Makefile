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
# CBOR program images before they can be ingested.
#
# Current ingestion paths:
#   - ProgramLibrary::ingest_elf()  (internal gateway API, GW-0400)
#     Parses the ELF, runs Prevail verification, converts to CBOR, and
#     stores the result.  Not yet exposed via the admin gRPC API.
#   - GatewayAdmin.IngestProgram (gRPC / sonde-admin CLI)
#     Accepts a CBOR-encoded ProgramImage directly.  Does not parse ELF.
#
# Until ELF ingestion is wired into the admin API, convert the ELF to
# CBOR via ProgramLibrary::ingest_elf() in an integration test or a
# custom tool, then use sonde-admin to upload the resulting CBOR image.

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
