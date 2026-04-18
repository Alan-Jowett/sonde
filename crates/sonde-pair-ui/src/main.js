// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

// Tauri v2 injects __TAURI__ when withGlobalTauri is true.
const { invoke } = window.__TAURI__.core;

// ---------------------------------------------------------------------------
// DOM references
// ---------------------------------------------------------------------------

// Pages (PT-1217)
const pages = [
  document.getElementById("page-welcome"),
  document.getElementById("page-gateway-scan"),
  document.getElementById("page-gateway-done"),
  document.getElementById("page-node-scan"),
  document.getElementById("page-node-provision"),
  document.getElementById("page-done"),
];

// Stepper (PT-1218)
const stepperSteps = document.querySelectorAll("#stepper .step");

// Back button (PT-1220 AC 6–8)
const btnBack = document.getElementById("btn-back");

// Page 1: Welcome
const pairingStatus = document.getElementById("pairing-status");
const btnGetStarted = document.getElementById("btn-get-started");
const btnSkipToNode = document.getElementById("btn-skip-to-node");
const btnClear = document.getElementById("btn-clear");

// Page 2: Gateway Scan
const btnScanStartGw = document.getElementById("btn-scan-start-gw");
const btnScanStopGw = document.getElementById("btn-scan-stop-gw");
const deviceListGw = document.getElementById("device-list-gw");
const phoneLabel = document.getElementById("phone-label");
const btnPair = document.getElementById("btn-pair");
const pairStatus = document.getElementById("pair-status");

// Page 3: Pairing Complete
const pairDetails = document.getElementById("pair-details");
const btnToNode = document.getElementById("btn-to-node");
const btnClearGwDone = document.getElementById("btn-clear-gw-done");

// Page 4: Node Scan
const btnScanStartNode = document.getElementById("btn-scan-start-node");
const btnScanStopNode = document.getElementById("btn-scan-stop-node");
const deviceListNode = document.getElementById("device-list-node");
const rssiPanel = document.getElementById("rssi-panel");
const rssiValue = document.getElementById("rssi-value");
const rssiLabel = document.getElementById("rssi-label");
const rssiIndicator = document.getElementById("rssi-indicator");
const btnToProvision = document.getElementById("btn-to-provision");

// Page 5: Node Provision
const nodeId = document.getElementById("node-id");
const boardSelect = document.getElementById("board-select");
const customPins = document.getElementById("custom-pins");
const customSda = document.getElementById("custom-sda");
const customScl = document.getElementById("custom-scl");
const btnProvision = document.getElementById("btn-provision");
const provisionStatus = document.getElementById("provision-status");

// Page 6: Done
const provisionDetails = document.getElementById("provision-details");
const btnProvisionAnother = document.getElementById("btn-provision-another");

// Global
const errorBar = document.getElementById("error-bar");
const verboseToggle = document.getElementById("verbose-toggle");
const logPanel = document.getElementById("log-panel");

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

let selectedAddressGw = null;
let selectedAddressNode = null;
let scanning = false;
let pollTimer = null;
let logTimer = null;
let busy = false;
let isPaired = false;
let scanGeneration = 0;

// ---------------------------------------------------------------------------
// Board presets (PT-1216)
// ---------------------------------------------------------------------------

const BOARD_PRESETS = {
  devkitm1: { label: "Espressif ESP32-C3 DevKitM-1", sda: 0, scl: 1 },
  sparkfun: { label: "SparkFun ESP32-C3 Pro Micro",   sda: 5, scl: 6 },
};

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
// Navigator (PT-1217, PT-1218, PT-1219, PT-1220, PT-1222)
// ---------------------------------------------------------------------------

// Maps page index → stepper phase index (0=Gateway, 1=Node, 2=Done)
const PAGE_TO_PHASE = [0, 0, 0, 1, 1, 2];
const STORAGE_KEY = "sonde-pair-page";

class Navigator {
  constructor() {
    this.currentPage = 0;
    this._skipPush = false;
  }

  goTo(pageIndex, { push = true } = {}) {
    if (pageIndex < 0 || pageIndex >= pages.length) return;

    // Scan lifecycle: stop scan when leaving a scan page (pages 1=gw, 3=node)
    // but preserve node selection when advancing from page 3→4 (node scan → provision)
    const leavingPage = this.currentPage;
    if (leavingPage === 1 || leavingPage === 3) {
      const advancingToProvision = leavingPage === 3 && pageIndex === 4;
      this._cleanupScanPage(leavingPage, { preserveSelection: advancingToProvision });
    }

    const direction = pageIndex >= this.currentPage ? "forward" : "back";
    const oldPage = pages[this.currentPage];
    const newPage = pages[pageIndex];

    // Apply transition classes (PT-1222)
    if (oldPage !== newPage) {
      if (direction === "forward") {
        oldPage.classList.add("slide-out-left");
        newPage.classList.add("slide-in-right");
      } else {
        oldPage.classList.add("slide-out-right");
        newPage.classList.add("slide-in-left");
      }
      newPage.classList.add("page--active");

      // Remove old page and clean up animation classes after transition
      setTimeout(() => {
        oldPage.classList.remove("page--active", "slide-out-left", "slide-out-right");
        newPage.classList.remove("slide-in-right", "slide-in-left");
      }, 300);
    }

    this.currentPage = pageIndex;
    this._updateStepper();
    clearError();

    // Persist (PT-1219)
    localStorage.setItem(STORAGE_KEY, String(pageIndex));

    // History (PT-1220)
    if (push && !this._skipPush) {
      history.pushState({ page: pageIndex }, "", "");
    }
  }

  next() { this.goTo(this.currentPage + 1); }

  back() { this.goTo(this.currentPage - 1); }

  restore() {
    // Seed history with sentinel at the bottom so back on page 1 is a no-op (PT-1220 AC 3)
    history.replaceState({ page: 0, sentinel: true }, "", "");

    const saved = parseInt(localStorage.getItem(STORAGE_KEY), 10);
    const earliestValid = isPaired ? 3 : 0;
    let target = earliestValid;
    if (!isNaN(saved) && saved >= 0 && saved < pages.length) {
      target = saved;
    }

    // Validate prerequisites: pages 2+ require pairing
    if (target >= 2 && !isPaired) {
      target = 0;
    }

    // Pages 5–6 require ephemeral state (selected node / provisioning-success context)
    // that cannot survive a restart — fall back to Node Scan when paired,
    // or Welcome when unpaired, consistent with the prerequisite check above (PT-1219 AC 4)
    if (target >= 4) {
      target = isPaired ? 3 : 0;
    }

    // Push all intermediate pages so back navigation traverses each step (PT-1220 AC 3)
    for (let i = 0; i <= target; i++) {
      history.pushState({ page: i }, "", "");
    }

    this._skipPush = true;
    try {
      this.goTo(target, { push: false });
    } finally {
      this._skipPush = false;
    }
  }

  get current() { return this.currentPage; }

  _updateStepper() {
    const activePhase = PAGE_TO_PHASE[this.currentPage];
    stepperSteps.forEach((el, i) => {
      el.classList.remove("step--active", "step--done");
      if (i < activePhase) {
        el.classList.add("step--done");
      } else if (i === activePhase) {
        el.classList.add("step--active");
      }
    });
    // PT-1220 AC 6–7: show back button on pages 2–6, hide on page 1
    btnBack.classList.toggle("hidden", this.currentPage === 0);
  }

  _cleanupScanPage(pageIndex, { preserveSelection = false } = {}) {
    if (scanning) {
      invoke("stop_scan").catch(() => {});
      if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
      scanning = false;
      scanGeneration++;
    }
    if (pageIndex === 1) {
      selectedAddressGw = null;
      btnPair.disabled = true;
      btnScanStartGw.disabled = false;
      btnScanStopGw.disabled = true;
      renderDevices(deviceListGw, [], false);
    } else if (pageIndex === 3) {
      if (!preserveSelection) {
        selectedAddressNode = null;
        btnToProvision.disabled = true;
        rssiPanel.classList.add("hidden");
      }
      btnScanStartNode.disabled = false;
      btnScanStopNode.disabled = true;
      renderDevices(deviceListNode, [], false);
    }
  }
}

const navigator_ = new Navigator();

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

function showStatus(el, msg) {
  el.textContent = msg;
  el.classList.remove("hidden");
}

function hideStatus(el) {
  el.textContent = "";
  el.classList.add("hidden");
}

function setBusy(b) {
  busy = b;
  // Disable action buttons on the current page while busy
  btnPair.disabled = b || !selectedAddressGw;
  btnProvision.disabled = b || !selectedAddressNode;
  btnScanStartGw.disabled = b || scanning;
  btnScanStopGw.disabled = b || !scanning;
  btnScanStartNode.disabled = b || scanning;
  btnScanStopNode.disabled = b || !scanning;
  btnToProvision.disabled = b || !selectedAddressNode;
}

// ---------------------------------------------------------------------------
// RSSI quality classification (PT-1221)
// ---------------------------------------------------------------------------

function classifyRssi(rssi) {
  if (rssi >= -60) return { level: "good", label: "Good", cls: "rssi--good" };
  if (rssi >= -75) return { level: "marginal", label: "Marginal", cls: "rssi--marginal" };
  return { level: "bad", label: "Bad", cls: "rssi--bad" };
}

function updateRssiIndicator(rssi) {
  if (rssi == null) {
    rssiPanel.classList.add("hidden");
    return;
  }
  rssiPanel.classList.remove("hidden");
  const q = classifyRssi(rssi);
  rssiValue.textContent = rssi + " dBm";
  rssiLabel.textContent = q.label;
  rssiIndicator.className = "rssi-indicator " + q.cls;
}

// ---------------------------------------------------------------------------
// Device list rendering
// ---------------------------------------------------------------------------

function renderDevices(listEl, devices, isScanning) {
  listEl.innerHTML = "";

  if (devices.length === 0) {
    const li = document.createElement("li");
    li.className = "placeholder";
    li.textContent = isScanning ? "Scanning\u2026" : "No devices found";
    listEl.appendChild(li);
    return;
  }

  const isGw = listEl === deviceListGw;
  const selectedAddr = isGw ? selectedAddressGw : selectedAddressNode;

  for (const d of devices) {
    const li = document.createElement("li");
    li.dataset.address = d.address;
    if (d.address === selectedAddr) li.classList.add("selected");
    li.onclick = () => {
      if (isGw) {
        selectGatewayDevice(d.address);
      } else {
        selectNodeDevice(d.address, d.rssi);
      }
    };

    const name = document.createElement("span");
    name.className = "device-name";
    name.textContent = d.name || "(unnamed)";

    const meta = document.createElement("span");
    meta.className = "device-meta";

    const badge = document.createElement("span");
    badge.className = "badge " + d.service_type.toLowerCase();
    badge.textContent = d.service_type;

    const rssiSpan = document.createElement("span");
    rssiSpan.textContent = d.rssi + " dBm";

    meta.appendChild(badge);
    meta.appendChild(rssiSpan);
    li.appendChild(name);
    li.appendChild(meta);
    listEl.appendChild(li);
  }
}

function selectGatewayDevice(address) {
  selectedAddressGw = address;
  for (const li of deviceListGw.children) {
    li.classList.toggle("selected", li.dataset.address === address);
  }
  btnPair.disabled = busy || !address;
}

function selectNodeDevice(address, rssi) {
  selectedAddressNode = address;
  for (const li of deviceListNode.children) {
    li.classList.toggle("selected", li.dataset.address === address);
  }
  updateRssiIndicator(rssi);
  btnToProvision.disabled = busy || !address;
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
    scanGeneration++;
    const gen = scanGeneration;
    const onGwPage = navigator_.current === 1;
    if (onGwPage) {
      selectedAddressGw = null;
      btnScanStartGw.disabled = true;
      btnScanStopGw.disabled = false;
    } else {
      selectedAddressNode = null;
      updateRssiIndicator(null);
      btnToProvision.disabled = true;
      btnScanStartNode.disabled = true;
      btnScanStopNode.disabled = false;
    }
    const listEl = onGwPage ? deviceListGw : deviceListNode;
    renderDevices(listEl, [], true);
    pollTimer = setInterval(() => pollDevices(listEl, gen), 1500);
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
  scanGeneration++;
  const onGwPage = navigator_.current === 1;
  if (onGwPage) {
    btnScanStartGw.disabled = false;
    btnScanStopGw.disabled = true;
  } else {
    btnScanStartNode.disabled = false;
    btnScanStopNode.disabled = true;
  }
}

async function pollDevices(listEl, gen) {
  if (gen !== scanGeneration) return;
  try {
    const devices = await invoke("get_devices");
    if (gen !== scanGeneration) return;
    const isScanning = scanning;
    renderDevices(listEl, devices, isScanning);

    // Update RSSI for selected node device (PT-1221)
    if (listEl === deviceListNode && selectedAddressNode) {
      const selected = devices.find(d => d.address === selectedAddressNode);
      if (selected) {
        updateRssiIndicator(selected.rssi);
      } else {
        updateRssiIndicator(null);
      }
    }
  } catch (_) {
    // Ignore transient poll errors.
  }
}

// ---------------------------------------------------------------------------
// Phase 1: Gateway Pairing
// ---------------------------------------------------------------------------

async function pairGateway() {
  if (!selectedAddressGw) return;
  clearError();
  if (scanning) await stopScan();
  setBusy(true);
  showStatus(pairStatus, "Connecting\u2026");
  try {
    showStatus(pairStatus, "Pairing\u2026");
    await invoke("pair_gateway", {
      address: selectedAddressGw,
      phoneLabel: phoneLabel.value || "sonde-phone",
    });
    showStatus(pairStatus, "Verifying\u2026");
    const status = await invoke("get_pairing_status");
    isPaired = !!status.paired;
    if (isPaired) {
      pairingStatus.textContent = "Paired \u2014 Gateway " + (status.gateway_id || "").substring(0, 8) + "\u2026";
      btnGetStarted.classList.add("hidden");
      btnSkipToNode.classList.remove("hidden");
      pairDetails.textContent = "Gateway " + (status.gateway_id || "").substring(0, 8) + "\u2026";
    }
    hideStatus(pairStatus);
    navigator_.next();
  } catch (e) {
    hideStatus(pairStatus);
    showError(e);
  } finally {
    setBusy(false);
  }
}

// ---------------------------------------------------------------------------
// Phase 2: Node Provisioning
// ---------------------------------------------------------------------------

async function provisionNode() {
  if (!selectedAddressNode) return;
  const nid = nodeId.value.trim();
  if (!nid) { showError("Enter a Node ID"); return; }
  const pins = resolveI2cPins();
  if (!pins) return;
  clearError();
  if (scanning) await stopScan();
  setBusy(true);
  showStatus(provisionStatus, "Connecting to node\u2026");
  try {
    showStatus(provisionStatus, "Provisioning\u2026");
    await invoke("provision_node", {
      address: selectedAddressNode,
      nodeId: nid,
      i2cSda: pins.sda,
      i2cScl: pins.scl,
    });
    hideStatus(provisionStatus);
    provisionDetails.textContent = "Node \"" + nid + "\" provisioned.";
    navigator_.next();
  } catch (e) {
    hideStatus(provisionStatus);
    showError(e);
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
      isPaired = true;
      const gwLabel = "Gateway " + (s.gateway_id || "").substring(0, 8) + "\u2026";
      pairingStatus.textContent = "Paired \u2014 " + gwLabel;
      btnGetStarted.classList.add("hidden");
      btnSkipToNode.classList.remove("hidden");
      pairDetails.textContent = gwLabel;
    } else {
      isPaired = false;
      pairingStatus.textContent = "Not paired";
      btnGetStarted.classList.remove("hidden");
      btnSkipToNode.classList.add("hidden");
    }
  } catch (e) {
    pairingStatus.textContent = "Error: " + e;
  }
}

async function clearPairing() {
  clearError();
  try {
    await invoke("clear_pairing");
    isPaired = false;
    await refreshPairingStatus();
    navigator_.goTo(0);
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

// Header back button (PT-1220 AC 8)
btnBack.addEventListener("click", () => history.back());

// Page 1: Welcome
btnGetStarted.addEventListener("click", () => navigator_.next());
btnSkipToNode.addEventListener("click", () => navigator_.goTo(3));
btnClear.addEventListener("click", clearPairing);

// Page 2: Gateway Scan
btnScanStartGw.addEventListener("click", startScan);
btnScanStopGw.addEventListener("click", stopScan);
btnPair.addEventListener("click", pairGateway);

// Page 3: Pairing Complete
btnToNode.addEventListener("click", () => navigator_.next());
btnClearGwDone.addEventListener("click", clearPairing);

// Page 4: Node Scan
btnScanStartNode.addEventListener("click", startScan);
btnScanStopNode.addEventListener("click", stopScan);
btnToProvision.addEventListener("click", () => {
  btnProvision.disabled = !selectedAddressNode;
  navigator_.next();
});

// Page 5: Node Provision
btnProvision.addEventListener("click", provisionNode);
boardSelect.addEventListener("change", () => {
  customPins.classList.toggle("hidden", boardSelect.value !== "custom");
});

// Page 6: Done
btnProvisionAnother.addEventListener("click", () => {
  nodeId.value = "";
  selectedAddressNode = null;
  rssiPanel.classList.add("hidden");
  btnToProvision.disabled = true;
  renderDevices(deviceListNode, [], false);
  navigator_.goTo(3);
});

// Diagnostics
verboseToggle.addEventListener("change", toggleVerbose);

// Back navigation (PT-1220)
window.addEventListener("popstate", (e) => {
  if (e.state && e.state.sentinel) {
    // At the bottom of the history stack — navigate to page 1 and re-establish
    // a no-exit floor so further back presses stay on page 1 (PT-1220 AC 2, 3)
    history.pushState({ page: 0 }, "", "");
    navigator_.goTo(0, { push: false });
    return;
  }
  if (e.state && typeof e.state.page === "number") {
    navigator_.goTo(e.state.page, { push: false });
  }
});

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

initBoardSelect();
refreshPairingStatus().then(() => {
  navigator_.restore();
});
