<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->

# Pairing Tool UI Redesign — Multi-Page Installer Workflow

## Problem

The current pairing UI presents all functionality (scanning, gateway
pairing, node provisioning, status, diagnostics) on a single page.
This makes the workflow hard to follow and error-prone, especially
for field installers provisioning multiple nodes at a site.

## Design Goal

Transform the pairing tool from a single-page utility into a
**guided multi-step installer workflow** optimized for the field
deployment use case.  The installer is typically the same person
who sets up the entire site:

1. Plug in the gateway modem and install `sonde-gateway` software
2. Deploy the `.sondeapp` bundle to the gateway
3. Pair the phone with the gateway
4. Download the node plan (bundle manifest)
5. Walk through each node: view setup instructions, connect sensors,
   pair, check signal quality, provision
6. Track progress across all nodes at the site

## Workflow Overview

```
┌─────────────────────┐
│  1. Connect to      │  One-time per site visit.
│     Gateway         │  Scan → Select → Pair → Download node plan.
└─────────┬───────────┘
          ▼
┌─────────────────────┐
│  2. Node Plan       │  Overview of all nodes from the bundle.
│     Overview        │  Shows name, sensors, status (pending/done).
│                     │  Progress: "3 of 8 nodes provisioned."
└─────────┬───────────┘
          ▼
    ┌─────────────┐
    │  Node Loop  │  Repeat for each node in the plan:
    │             │
    │  3. Setup   │  ← Node-specific setup guide with:
    │     Guide   │     - Required sensors and wiring
    │             │     - Pin configuration (SDA/SCL)
    │             │     - Setup media (photos/videos from bundle)
    │             │
    │  4. Pair &  │  ← Scan for node BLE service
    │     RSSI    │     Select physical device
    │             │     Live RSSI signal quality indicator
    │             │     "Signal good — ready to provision"
    │             │
    │  5. Prov-   │  ← Progress: Connect → Provision → ACK
    │     ision   │     Success/failure with actionable errors
    │             │     "Next Node" returns to step 2
    └─────────────┘
          ▼
┌─────────────────────┐
│  6. All Done        │  Summary: "8/8 nodes provisioned."
│                     │  List of all nodes with final status.
└─────────────────────┘
```

## Step Details

### Step 1 — Connect to Gateway

**Purpose:** Establish trust with the gateway and download the
deployment plan.

**UI elements:**
- Auto-start BLE scan on entry (with scanning animation)
- List of discovered gateways with signal strength bars
- Tap to select a gateway
- Phone label input field (pre-filled with device name if available)
- "Pair" button → progress overlay ("Connecting…", "Pairing…",
  "Registering…", "Downloading plan…")
- Numeric Comparison confirmation (required; prompt/UX is
  platform-dependent)
- On success: transition to Step 2 with the downloaded node plan

**Error handling:**
- No gateways found → "Make sure the gateway modem is powered on
  and nearby" + retry button
- Pairing failed → actionable error with retry
- Already paired → show status, offer "Re-pair" or skip to Step 2

**Backend requirements:**
- Existing `pair_gateway()` Tauri command
  (internally calls `phase1::pair_with_gateway`)
- **New:** Download bundle manifest from gateway after pairing
  (requires new protocol message or admin API endpoint)

### Step 2 — Node Plan Overview

**Purpose:** Show the full deployment checklist and track progress.

**UI elements:**
- List/cards showing each node from the bundle manifest:
  - Node name/ID
  - Sensor types (icons + text)
  - Status badge: ⬜ Pending / ✅ Provisioned / ❌ Failed
- Progress bar: "3 of 8 nodes provisioned"
- Tap a node → Step 3 (setup guide)
- Dropdown/select for quick navigation
- "All Done" button (enabled when all nodes provisioned)

**Data source:** Bundle manifest `nodes[]` array (current schema):
- `name` — node identifier
- `hardware.sensors[]` — list of sensor types
- `hardware.pins` — I²C pin configuration (`i2c0_sda`, `i2c0_scl`)
- `setup_media[]` — **proposed extension** (not in current schema):
  optional photos/videos (URLs or embedded in bundle ZIP)

### Step 3 — Node Setup Guide

**Purpose:** Guide the installer through physical sensor setup
before initiating the wireless pairing.

**UI elements:**
- Node name and ID prominently displayed
- Sensor checklist:
  - "☐ Connect TMP102 temperature sensor to Qwiic port"
  - "☐ Connect DS18B20 to 1-Wire port A"
- Pin configuration display: "I²C: SDA=GPIO4, SCL=GPIO5"
- Setup media viewer:
  - Photos showing correct wiring (swipeable gallery)
  - Optional short video walkthrough
- "Sensors Connected — Ready to Pair" button → Step 4

**Design notes:**
- Media is optional — nodes without setup media show text-only
- The checklist is informational (not enforced by the app)
- Pin config is displayed for reference; validated during provisioning

### Step 4 — Pair & Signal Check

**Purpose:** Find the physical node via BLE and verify RF signal
quality before committing to provisioning.

**UI elements:**
- Auto-start BLE scan for node provisioning service UUID
- List of discovered nodes with:
  - Device address
  - Signal strength (animated bars)
- Tap to select → show detailed RSSI panel:
  - **Live RSSI gauge** (good ≥ −60 dBm / marginal −60 to −75 dBm / bad < −75 dBm)
  - "Move the node closer" warning if signal is bad
  - RSSI history graph (last 10 seconds)
- "Signal Good — Provision" button → Step 5
  - Enabled when RSSI is good or marginal (≥ −75 dBm)
  - "Provision Anyway" override shown when RSSI is bad (< −75 dBm)

**Backend requirements:**
- Existing `start_scan()` / `get_devices()` Tauri commands
- RSSI data from scan results (already available)
- **New (future):** DIAG_REQUEST/DIAG_REPLY for gateway-side RSSI
  measurement (protocol already defined, not yet in pairing tool)

### Step 5 — Provision

**Purpose:** Execute the Phase 2 provisioning protocol.

**UI elements:**
- Progress steps with animated indicators:
  1. "Connecting to node…" (spinner)
  2. "Provisioning…" (spinner)
  3. "Waiting for acknowledgment…" (spinner)
- On success:
  - Green checkmark animation
  - Node details: ID, key hint, channel
  - "Next Node" button → back to Step 2 (node marked ✅)
  - "Provision Another" if this was the last planned node
- On failure:
  - Red X with actionable error message
  - "Retry" button
  - "Skip This Node" → back to Step 2 (node marked ❌)

**Backend requirements:**
- Existing `provision_node()` Tauri command for Phase 2 provisioning
- **Extension needed:** `provision_node()` must be extended to accept
  and forward `PinConfig` from the bundle manifest (the current command
  does not support passing pin config)

### Step 6 — All Done

**Purpose:** Summary and completion confirmation.

**UI elements:**
- "All nodes provisioned!" celebration banner
- Summary table: node name, status, signal quality at provisioning time
- "Finish" button → back to Step 1
- Option to export provisioning report (future)

## Navigation Principles

| Principle | Implementation |
|-----------|---------------|
| **Progress indicator** | Top stepper bar showing current phase |
| **One action per screen** | Each step focuses on a single task |
| **Back navigation** | Hardware/gesture back returns to previous step |
| **Session persistence** | Provisioning progress survives app restart |
| **Real-time feedback** | Animated spinners, RSSI gauges, status updates |
| **Graceful errors** | Every error has an actionable message + retry |

## Technical Implementation Notes

### Frontend (HTML/JS/CSS)

The current `index.html` is a single-page layout. The redesign uses
client-side page routing (hash-based or simple show/hide of `<section>`
elements). No framework dependency is required — vanilla JS with a
simple state machine is sufficient for 6 pages.

**Suggested approach:**
- Each step is a `<section class="page" id="page-N">` element
- A `Navigator` class manages visibility and transition animations
- State persisted to `localStorage` for session recovery
- CSS transitions for page slides (left/right)

### Backend (Tauri commands)

Most backend commands already exist:
- `start_scan()`, `stop_scan()`, `get_devices()` — scanning
- `pair_gateway()` — Phase 1
- `provision_node()` — Phase 2
- `get_pairing_status()` — status check

**New commands needed:**
- `get_node_plan()` — download bundle manifest from gateway
  (requires gateway admin API extension or new BLE message)
- `get_rssi_history()` — return recent RSSI samples for a device
  (may be derivable from existing scan results)

### Bundle Manifest Integration

The bundle manifest (`.sondeapp`) already contains node definitions
with sensor types and pin configurations. The gateway needs to
expose this data to the pairing tool — either via:

1. **Admin API (gRPC):** New `GetDeploymentPlan` RPC that returns
   the active bundle manifest. The pairing tool would need network
   access to the gateway's admin socket (not always available).

2. **BLE extension:** New message type in the Gateway Pairing Service
   that sends the bundle manifest after `PHONE_REGISTERED`. This
   keeps everything within the existing BLE connection.

3. **Local bundle file:** The installer loads the `.sondeapp` file
   directly on the phone (e.g., via file picker or QR code URL).
   Simplest approach — no protocol changes needed.

**Recommendation:** Start with option 3 (local bundle file) for the
initial implementation. Options 1 and 2 can be added later for a
more seamless workflow.

## Phased Implementation

### Phase A — Page routing and basic wizard (no bundle integration)
- Implement multi-page navigation with stepper bar
- Move existing functionality into separate pages
- Add RSSI display on node selection page
- No bundle manifest — manual node ID entry (current behavior)

### Phase B — Bundle manifest integration
- Add `.sondeapp` file picker
- Parse bundle manifest for node list, sensors, pins
- Show setup guides and progress tracking

### Phase C — Setup media support
- Photo/video viewer in setup guide step
- Media sourced from bundle or external URLs

### Phase D — Gateway-side plan download
- Implement BLE or admin API manifest transfer
- Automatic plan download after gateway pairing

## Open Questions

1. **Bundle file format for media:** Should setup photos/videos be
   embedded in the `.sondeapp` ZIP, or referenced by URL?

2. **Offline capability:** Should the app work fully offline (all
   media embedded), or is network access acceptable at the site?

3. **Multi-installer coordination:** If two installers are working
   the same site, how is node assignment coordinated? (Future scope)

4. **Plan modification:** Can the installer skip or reorder nodes,
   or must they follow the plan sequentially?
