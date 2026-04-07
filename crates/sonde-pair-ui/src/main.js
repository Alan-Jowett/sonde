// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

// Tauri v2 injects __TAURI__ when withGlobalTauri is true.
const { invoke } = window.__TAURI__.core;

// ---------------------------------------------------------------------------
// DOM references
// ---------------------------------------------------------------------------

const btnScanStart = document.getElementById("btn-scan-start");
const btnScanStop = document.getElementById("btn-scan-stop");
const deviceList = document.getElementById("device-list");
const phoneLabel = document.getElementById("phone-label");
const btnPair = document.getElementById("btn-pair");
const nodeId = document.getElementById("node-id");
const boardSelect = document.getElementById("board-select");
const customPins = document.getElementById("custom-pins");
const customSda = document.getElementById("custom-sda");
const customScl = document.getElementById("custom-scl");
const btnProvision = document.getElementById("btn-provision");
const pairingStatus = document.getElementById("pairing-status");
const btnClear = document.getElementById("btn-clear");
const phaseBar = document.getElementById("phase-bar");
const errorBar = document.getElementById("error-bar");
const verboseToggle = document.getElementById("verbose-toggle");
const logPanel = document.getElementById("log-panel");

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

let selectedAddress = null;
let selectedType = null;
let scanning = false;
let pollTimer = null;
let logTimer = null;
let busy = false;

// ---------------------------------------------------------------------------
// Board presets (PT-1216)
// ---------------------------------------------------------------------------

const BOARD_PRESETS = {
  devkitm1: { label: "Espressif ESP32-C3 DevKitM-1", sda: 0, scl: 1 },
  sparkfun: { label: "SparkFun ESP32-C3 Pro Micro",   sda: 5, scl: 6 },
};

/** Populate the board selector from BOARD_PRESETS (single source of truth). */
function initBoardSelect() {
  const customOption = boardSelect.querySelector('option[value="custom"]');
  boardSelect.textContent = "";
  for (const [value, preset] of Object.entries(BOARD_PRESETS)) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = preset.label;
    boardSelect.appendChild(option);
  }
  if (customOption) boardSelect.appendChild(customOption);
  customPins.classList.toggle("hidden", boardSelect.value !== "custom");
}

/** Resolve the current board selection to { sda, scl } or null on error. */
function resolveI2cPins() {
  const board = boardSelect.value;
  if (board === "custom") {
    const sda = customSda.valueAsNumber;
    const scl = customScl.valueAsNumber;
    if (!Number.isInteger(sda) || !Number.isInteger(scl)) { showError("Enter SDA and SCL GPIO numbers"); return null; }
    if (sda < 0 || sda > 21 || scl < 0 || scl > 21) { showError("GPIO must be 0–21"); return null; }
    if (sda === scl) { showError("SDA and SCL must be different pins"); return null; }
    return { sda, scl };
  }
  const preset = BOARD_PRESETS[board];
  if (!preset) { showError("Unknown board selection"); return null; }
  return { sda: preset.sda, scl: preset.scl };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function showError(msg) {
  errorBar.textContent = msg;
  errorBar.classList.remove("hidden");
}

function clearError() {
  errorBar.textContent = "";
  errorBar.classList.add("hidden");
}

function setPhase(text) {
  phaseBar.textContent = text;
  const lower = text.toLowerCase();
  phaseBar.className = "phase";
  if (lower.startsWith("error"))       phaseBar.classList.add("error");
  else if (lower === "idle")           phaseBar.classList.add("idle");
  else if (lower === "scanning")       phaseBar.classList.add("scanning");
  else if (lower === "pairing")        phaseBar.classList.add("pairing");
  else if (lower === "provisioning")   phaseBar.classList.add("provisioning");
  else if (lower === "complete")       phaseBar.classList.add("complete");
}

function setBusy(b) {
  busy = b;
  btnPair.disabled = b || !selectedAddress;
  btnProvision.disabled = b || !selectedAddress;
  btnScanStart.disabled = b || scanning;
  btnScanStop.disabled = b || !scanning;
}

function selectDevice(address, serviceType) {
  selectedAddress = address;
  selectedType = serviceType;
  // Update selection UI
  for (const li of deviceList.children) {
    li.classList.toggle("selected", li.dataset.address === address);
  }
  btnPair.disabled = busy || !address;
  btnProvision.disabled = busy || !address;
}

// ---------------------------------------------------------------------------
// Device list rendering
// ---------------------------------------------------------------------------

function renderDevices(devices) {
  deviceList.innerHTML = "";

  if (devices.length === 0) {
    const li = document.createElement("li");
    li.className = "placeholder";
    li.textContent = scanning ? "Scanning\u2026" : "No devices found";
    deviceList.appendChild(li);
    return;
  }

  for (const d of devices) {
    const li = document.createElement("li");
    li.dataset.address = d.address;
    if (d.address === selectedAddress) li.classList.add("selected");
    li.onclick = () => selectDevice(d.address, d.service_type);

    const name = document.createElement("span");
    name.className = "device-name";
    name.textContent = d.name || "(unnamed)";

    const meta = document.createElement("span");
    meta.className = "device-meta";

    const badge = document.createElement("span");
    badge.className = "badge " + d.service_type.toLowerCase();
    badge.textContent = d.service_type;

    const rssi = document.createElement("span");
    rssi.textContent = d.rssi + " dBm";

    meta.appendChild(badge);
    meta.appendChild(rssi);
    li.appendChild(name);
    li.appendChild(meta);
    deviceList.appendChild(li);
  }
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

async function startScan() {
  clearError();
  setBusy(true);
  try {
    await invoke("start_scan");
    scanning = true;
    btnScanStart.disabled = true;
    btnScanStop.disabled = false;
    setPhase("Scanning");
    selectedAddress = null;
    selectedType = null;
    renderDevices([]);
    // Poll for devices every 1.5 s.
    pollTimer = setInterval(pollDevices, 1500);
  } catch (e) {
    showError(e);
  } finally {
    setBusy(false);
  }
}

async function stopScan() {
  clearError();
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  try {
    await invoke("stop_scan");
  } catch (e) {
    showError(e);
  }
  scanning = false;
  btnScanStart.disabled = false;
  btnScanStop.disabled = true;
  setPhase("Idle");
}

async function pollDevices() {
  try {
    const devices = await invoke("get_devices");
    renderDevices(devices);
  } catch (_) {
    // Ignore transient poll errors.
  }
}

// ---------------------------------------------------------------------------
// Phase 1: Gateway Pairing
// ---------------------------------------------------------------------------

async function pairGateway() {
  if (!selectedAddress) return;
  clearError();
  if (scanning) await stopScan();
  setBusy(true);
  setPhase("Pairing");
  try {
    await invoke("pair_gateway", {
      address: selectedAddress,
      phoneLabel: phoneLabel.value || "sonde-phone",
    });
    setPhase("Complete");
    await refreshPairingStatus();
  } catch (e) {
    showError(e);
    setPhase("Error: " + e);
  } finally {
    setBusy(false);
  }
}

// ---------------------------------------------------------------------------
// Phase 2: Node Provisioning
// ---------------------------------------------------------------------------

async function provisionNode() {
  if (!selectedAddress) return;
  const nid = nodeId.value.trim();
  if (!nid) { showError("Enter a Node ID"); return; }
  const pins = resolveI2cPins();
  if (!pins) return;
  clearError();
  if (scanning) await stopScan();
  setBusy(true);
  setPhase("Provisioning");
  try {
    const status = await invoke("provision_node", {
      address: selectedAddress,
      nodeId: nid,
      i2cSda: pins.sda,
      i2cScl: pins.scl,
    });
    setPhase("Complete");
    // status is a human-readable string from NodeAckStatus
  } catch (e) {
    showError(e);
    setPhase("Error: " + e);
  } finally {
    setBusy(false);
  }
}

// ---------------------------------------------------------------------------
// Pairing status
// ---------------------------------------------------------------------------

async function refreshPairingStatus() {
  try {
    const s = await invoke("get_pairing_status");
    if (s.paired) {
      pairingStatus.textContent = "Paired \u2014 Gateway " + (s.gateway_id || "").substring(0, 8) + "\u2026";
    } else {
      pairingStatus.textContent = "Not paired";
    }
  } catch (e) {
    pairingStatus.textContent = "Error: " + e;
  }
}

async function clearPairing() {
  clearError();
  try {
    await invoke("clear_pairing");
    setPhase("Idle");
    await refreshPairingStatus();
  } catch (e) {
    showError(e);
  }
}

// ---------------------------------------------------------------------------
// Verbose diagnostic mode
// ---------------------------------------------------------------------------

function toggleVerbose() {
  const on = verboseToggle.checked;
  logPanel.classList.toggle("hidden", !on);
  if (on) {
    logTimer = setInterval(pollLogs, 1000);
  } else {
    if (logTimer) { clearInterval(logTimer); logTimer = null; }
  }
}

async function pollLogs() {
  try {
    const lines = await invoke("get_logs");
    if (lines.length > 0) {
      logPanel.textContent += lines.join("\n") + "\n";
      logPanel.scrollTop = logPanel.scrollHeight;
    }
  } catch (_) {
    // Ignore.
  }
}

// ---------------------------------------------------------------------------
// Event bindings
// ---------------------------------------------------------------------------

btnScanStart.addEventListener("click", startScan);
btnScanStop.addEventListener("click", stopScan);
btnPair.addEventListener("click", pairGateway);
btnProvision.addEventListener("click", provisionNode);
btnClear.addEventListener("click", clearPairing);
verboseToggle.addEventListener("change", toggleVerbose);
boardSelect.addEventListener("change", () => {
  customPins.classList.toggle("hidden", boardSelect.value !== "custom");
});

// Initial state
initBoardSelect();
refreshPairingStatus();
