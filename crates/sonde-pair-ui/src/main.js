// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

const { invoke } = window.__TAURI__.core;

const pages = [
  document.getElementById("page-welcome"),
  document.getElementById("page-gateway-scan"),
  document.getElementById("page-gateway-done"),
  document.getElementById("page-node-scan"),
  document.getElementById("page-signal-check"),
  document.getElementById("page-node-provision"),
  document.getElementById("page-done"),
];

const stepperSteps = document.querySelectorAll("#stepper .step");
const btnBack = document.getElementById("btn-back");

const pairingStatus = document.getElementById("pairing-status");
const btnGetStarted = document.getElementById("btn-get-started");
const btnSkipToNode = document.getElementById("btn-skip-to-node");
const btnClear = document.getElementById("btn-clear");

const btnScanStartGw = document.getElementById("btn-scan-start-gw");
const btnScanStopGw = document.getElementById("btn-scan-stop-gw");
const deviceListGw = document.getElementById("device-list-gw");
const phoneLabel = document.getElementById("phone-label");
const btnPair = document.getElementById("btn-pair");
const pairStatus = document.getElementById("pair-status");

const pairDetails = document.getElementById("pair-details");
const btnToNode = document.getElementById("btn-to-node");
const btnClearGwDone = document.getElementById("btn-clear-gw-done");

const btnScanStartNode = document.getElementById("btn-scan-start-node");
const btnScanStopNode = document.getElementById("btn-scan-stop-node");
const deviceListNode = document.getElementById("device-list-node");
const rssiPanel = document.getElementById("rssi-panel");
const rssiValue = document.getElementById("rssi-value");
const rssiLabel = document.getElementById("rssi-label");
const rssiIndicator = document.getElementById("rssi-indicator");
const btnConnectNode = document.getElementById("btn-connect-node");

const diagBleIndicator = document.getElementById("diag-ble-indicator");
const diagBleValue = document.getElementById("diag-ble-value");
const diagBleLabel = document.getElementById("diag-ble-label");
const diagEspNowIndicator = document.getElementById("diag-espnow-indicator");
const diagEspNowValue = document.getElementById("diag-espnow-value");
const diagEspNowLabel = document.getElementById("diag-espnow-label");
const signalStatus = document.getElementById("signal-status");
const btnSignalAbort = document.getElementById("btn-signal-abort");
const btnSignalProceed = document.getElementById("btn-signal-proceed");

const nodeId = document.getElementById("node-id");
const boardSelect = document.getElementById("board-select");
const customPins = document.getElementById("custom-pins");
const customI2cSda = document.getElementById("custom-i2c-sda");
const customI2cScl = document.getElementById("custom-i2c-scl");
const customOneWire = document.getElementById("custom-one-wire");
const customBatteryAdc = document.getElementById("custom-battery-adc");
const customSensorEnable = document.getElementById("custom-sensor-enable");
const btnProvision = document.getElementById("btn-provision");
const provisionStatus = document.getElementById("provision-status");

const provisionDetails = document.getElementById("provision-details");
const btnProvisionAnother = document.getElementById("btn-provision-another");

const errorBar = document.getElementById("error-bar");
const verboseToggle = document.getElementById("verbose-toggle");
const logPanel = document.getElementById("log-panel");

let selectedAddressGw = null;
let selectedAddressNode = null;
let selectedNodeBleRssi = null;
let scanning = false;
let scanGeneration = 0;
let pollTimer = null;
let logTimer = null;
let busy = false;
let isPaired = false;
let signalLoopEnabled = false;
let diagCurrentPromise = null;

const BOARD_PRESETS = {
  rev_a: {
    label: "Sonde Sensor Node rev_a",
    layout: { i2c0Sda: 6, i2c0Scl: 7, oneWireData: 3, batteryAdc: 2, sensorEnable: 4 },
  },
  devkitm1: {
    label: "Espressif ESP32-C3 DevKitM-1",
    layout: { i2c0Sda: 0, i2c0Scl: 1, oneWireData: null, batteryAdc: null, sensorEnable: null },
  },
  sparkfun: {
    label: "SparkFun ESP32-C3 Pro Micro",
    layout: { i2c0Sda: 5, i2c0Scl: 6, oneWireData: null, batteryAdc: null, sensorEnable: null },
  },
};

const PAGE_TO_PHASE = [0, 0, 0, 1, 1, 1, 2];
const STORAGE_KEY = "sonde-pair-page";

class Navigator {
  constructor() {
    this.currentPage = 0;
    this._skipPush = false;
  }

  goTo(pageIndex, { push = true } = {}) {
    if (pageIndex < 0 || pageIndex >= pages.length) return;

    const leavingPage = this.currentPage;
    if (leavingPage === 1 || leavingPage === 3 || leavingPage === 4) {
      const preserveNodeSelection = leavingPage === 3 && pageIndex === 4;
      const preserveConnection = leavingPage === 4 && pageIndex === 5;
      this._cleanupPage(leavingPage, { preserveNodeSelection, preserveConnection });
    }

    const direction = pageIndex >= this.currentPage ? "forward" : "back";
    const oldPage = pages[this.currentPage];
    const newPage = pages[pageIndex];

    if (oldPage !== newPage) {
      oldPage.classList.add(direction === "forward" ? "slide-out-left" : "slide-out-right");
      newPage.classList.add(direction === "forward" ? "slide-in-right" : "slide-in-left");
      newPage.classList.add("page--active");
      setTimeout(() => {
        oldPage.classList.remove("page--active", "slide-out-left", "slide-out-right");
        newPage.classList.remove("slide-in-right", "slide-in-left");
      }, 300);
    }

    this.currentPage = pageIndex;
    this._updateStepper();
    clearError();
    localStorage.setItem(STORAGE_KEY, String(pageIndex));
    if (push && !this._skipPush) history.pushState({ page: pageIndex }, "", "");
  }

  next() { this.goTo(this.currentPage + 1); }
  back() { this.goTo(this.currentPage - 1); }

  restore() {
    history.replaceState({ page: 0, sentinel: true }, "", "");
    const saved = parseInt(localStorage.getItem(STORAGE_KEY), 10);
    let target = isPaired ? 3 : 0;
    if (!Number.isNaN(saved) && saved >= 0 && saved < pages.length) target = saved;
    if (target >= 2 && !isPaired) target = 0;
    if (target >= 4) target = isPaired ? 3 : 0;
    for (let i = 0; i <= target; i++) history.pushState({ page: i }, "", "");
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
      if (i < activePhase) el.classList.add("step--done");
      else if (i === activePhase) el.classList.add("step--active");
    });
    btnBack.classList.toggle("hidden", this.currentPage === 0);
  }

  _cleanupPage(pageIndex, { preserveNodeSelection = false, preserveConnection = false } = {}) {
    if (pageIndex === 1 || pageIndex === 3) {
      cleanupScanPage(pageIndex, preserveNodeSelection);
    }
    if (pageIndex === 4 && !preserveConnection) {
      stopSignalLoop();
      invoke("disconnect_node").catch(() => {});
      resetSignalCheckView();
    }
  }
}

const navigator_ = new Navigator();

function showError(msg) {
  errorBar.textContent = String(msg);
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

function setBusy(value) {
  busy = value;
  btnPair.disabled = value || !selectedAddressGw;
  btnProvision.disabled = value || !selectedAddressNode;
  btnConnectNode.disabled = value || !selectedAddressNode;
  btnSignalAbort.disabled = value;
  btnSignalProceed.disabled = value || !selectedAddressNode || !signalLoopEnabled;
  btnScanStartGw.disabled = value || scanning;
  btnScanStopGw.disabled = value || !scanning || navigator_.current !== 1;
  btnScanStartNode.disabled = value || scanning;
  btnScanStopNode.disabled = value || !scanning || navigator_.current !== 3;
}

function classifyRssi(rssi) {
  if (rssi >= -60) return { label: "Good", cls: "rssi--good" };
  if (rssi >= -75) return { label: "Marginal", cls: "rssi--marginal" };
  return { label: "Bad", cls: "rssi--bad" };
}

function setIndicator(panel, valueEl, labelEl, rssi, waitingLabel = "--") {
  if (rssi == null) {
    valueEl.textContent = "--";
    labelEl.textContent = waitingLabel;
    panel.className = "rssi-indicator";
    return;
  }
  const quality = classifyRssi(rssi);
  valueEl.textContent = `${rssi} dBm`;
  labelEl.textContent = quality.label;
  panel.className = `rssi-indicator ${quality.cls}`;
}

function updateBleSelectionIndicator(rssi) {
  if (rssi == null) {
    rssiPanel.classList.add("hidden");
    setIndicator(rssiIndicator, rssiValue, rssiLabel, null);
    return;
  }
  rssiPanel.classList.remove("hidden");
  setIndicator(rssiIndicator, rssiValue, rssiLabel, rssi);
}

function updateSignalCheckIndicators(diagRssi = null) {
  setIndicator(diagBleIndicator, diagBleValue, diagBleLabel, selectedNodeBleRssi, "Waiting");
  setIndicator(diagEspNowIndicator, diagEspNowValue, diagEspNowLabel, diagRssi, "Waiting");
}

function resetSignalCheckView() {
  signalLoopEnabled = false;
  diagCurrentPromise = null;
  updateSignalCheckIndicators(null);
  hideStatus(signalStatus);
  btnSignalProceed.disabled = true;
}

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
  boardSelect.value = "rev_a";
  customPins.classList.toggle("hidden", boardSelect.value !== "custom");
}

function parseOptionalPin(input, label) {
  const raw = input.value.trim();
  if (raw === "") return null;
  const value = Number(raw);
  if (!Number.isInteger(value)) {
    showError(`Enter a whole GPIO number for ${label}`);
    return undefined;
  }
  if (value < 0 || value > 21) {
    showError(`${label} must be 0–21`);
    return undefined;
  }
  return value;
}

function resolveBoardLayout() {
  if (boardSelect.value === "custom") {
    const i2c0Sda = parseOptionalPin(customI2cSda, "I2C SDA");
    const i2c0Scl = parseOptionalPin(customI2cScl, "I2C SCL");
    const oneWireData = parseOptionalPin(customOneWire, "1-Wire data");
    const batteryAdc = parseOptionalPin(customBatteryAdc, "battery ADC");
    const sensorEnable = parseOptionalPin(customSensorEnable, "sensor enable");
    if ([i2c0Sda, i2c0Scl, oneWireData, batteryAdc, sensorEnable].includes(undefined)) return null;
    if ((i2c0Sda === null) !== (i2c0Scl === null)) {
      showError("I2C SDA and I2C SCL must both be assigned or both left blank");
      return null;
    }
    if (i2c0Sda !== null && i2c0Sda === i2c0Scl) {
      showError("I2C SDA and I2C SCL must be different pins");
      return null;
    }
    return { i2c0Sda, i2c0Scl, oneWireData, batteryAdc, sensorEnable };
  }
  const preset = BOARD_PRESETS[boardSelect.value];
  if (!preset) {
    showError("Unknown board selection");
    return null;
  }
  return preset.layout;
}

function renderDevices(listEl, devices, isScanning) {
  listEl.innerHTML = "";
  if (devices.length === 0) {
    const li = document.createElement("li");
    li.className = "placeholder";
    li.textContent = isScanning ? "Scanning…" : "No devices found";
    listEl.appendChild(li);
    return;
  }

  const isGw = listEl === deviceListGw;
  const selectedAddr = isGw ? selectedAddressGw : selectedAddressNode;
  for (const device of devices) {
    const li = document.createElement("li");
    li.dataset.address = device.address;
    li.classList.toggle("selected", device.address === selectedAddr);
    li.onclick = () => {
      if (isGw) selectGatewayDevice(device.address);
      else selectNodeDevice(device.address, device.rssi);
    };

    const name = document.createElement("span");
    name.className = "device-name";
    name.textContent = device.name || "(unnamed)";

    const meta = document.createElement("span");
    meta.className = "device-meta";

    const badge = document.createElement("span");
    badge.className = `badge ${device.service_type.toLowerCase()}`;
    badge.textContent = device.service_type;

    const rssi = document.createElement("span");
    rssi.textContent = `${device.rssi} dBm`;

    meta.appendChild(badge);
    meta.appendChild(rssi);
    li.appendChild(name);
    li.appendChild(meta);
    listEl.appendChild(li);
  }
}

function selectGatewayDevice(address) {
  selectedAddressGw = address;
  for (const li of deviceListGw.children) li.classList.toggle("selected", li.dataset.address === address);
  btnPair.disabled = busy || !address;
}

function selectNodeDevice(address, rssi) {
  selectedAddressNode = address;
  selectedNodeBleRssi = rssi;
  for (const li of deviceListNode.children) li.classList.toggle("selected", li.dataset.address === address);
  updateBleSelectionIndicator(rssi);
  updateSignalCheckIndicators(null);
  btnConnectNode.disabled = busy || !address;
}

function cleanupScanPage(pageIndex, preserveNodeSelection) {
  if (scanning) {
    invoke("stop_scan").catch(() => {});
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
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
    if (!preserveNodeSelection) {
      selectedAddressNode = null;
      selectedNodeBleRssi = null;
      btnConnectNode.disabled = true;
      updateBleSelectionIndicator(null);
    }
    btnScanStartNode.disabled = false;
    btnScanStopNode.disabled = true;
    renderDevices(deviceListNode, [], false);
  }
}

async function startScan() {
  clearError();
  setBusy(true);
  try {
    await invoke("start_scan");
    scanning = true;
    scanGeneration += 1;
    const generation = scanGeneration;
    const gatewayPage = navigator_.current === 1;
    if (gatewayPage) {
      selectedAddressGw = null;
      btnScanStartGw.disabled = true;
      btnScanStopGw.disabled = false;
    } else {
      selectedAddressNode = null;
      selectedNodeBleRssi = null;
      btnConnectNode.disabled = true;
      updateBleSelectionIndicator(null);
      btnScanStartNode.disabled = true;
      btnScanStopNode.disabled = false;
    }
    const listEl = gatewayPage ? deviceListGw : deviceListNode;
    renderDevices(listEl, [], true);
    pollTimer = setInterval(() => pollDevices(listEl, generation), 1500);
  } catch (e) {
    showError(e);
  } finally {
    setBusy(false);
  }
}

async function stopScan() {
  clearError();
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
  try {
    await invoke("stop_scan");
  } catch (e) {
    showError(e);
  }
  scanning = false;
  scanGeneration += 1;
  btnScanStartGw.disabled = false;
  btnScanStopGw.disabled = true;
  btnScanStartNode.disabled = false;
  btnScanStopNode.disabled = true;
}

async function pollDevices(listEl, generation) {
  if (generation !== scanGeneration) return;
  try {
    const devices = await invoke("get_devices");
    if (generation !== scanGeneration) return;
    renderDevices(listEl, devices, scanning);
    if (listEl === deviceListNode && selectedAddressNode) {
      const selected = devices.find((device) => device.address === selectedAddressNode);
      if (selected) {
        selectedNodeBleRssi = selected.rssi;
        updateBleSelectionIndicator(selected.rssi);
      }
    }
  } catch (_) {
  }
}

async function pairGateway() {
  if (!selectedAddressGw) return;
  clearError();
  if (scanning) await stopScan();
  setBusy(true);
  showStatus(pairStatus, "Pairing…");
  try {
    await invoke("pair_gateway", {
      address: selectedAddressGw,
      phoneLabel: phoneLabel.value || "sonde-phone",
    });
    const status = await invoke("get_pairing_status");
    isPaired = !!status.paired;
    if (isPaired) {
      const gwLabel = `Gateway ${(status.gateway_id || "").substring(0, 8)}…`;
      pairingStatus.textContent = `Paired — ${gwLabel}`;
      btnGetStarted.classList.add("hidden");
      btnSkipToNode.classList.remove("hidden");
      pairDetails.textContent = gwLabel;
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

function stopSignalLoop() {
  signalLoopEnabled = false;
}

async function waitForCurrentDiagnostic() {
  if (diagCurrentPromise) {
    try {
      await diagCurrentPromise;
    } catch (_) {
    }
  }
}

async function runSignalCheckLoop() {
  if (!signalLoopEnabled) return;
  diagCurrentPromise = (async () => {
    try {
      const result = await invoke("check_rssi");
      updateSignalCheckIndicators(result.rssiDbm);
      showStatus(signalStatus, `Last update: ${result.rssiDbm} dBm via ESP-NOW`);
      btnSignalProceed.disabled = false;
    } catch (e) {
      showStatus(signalStatus, `Diagnostic unavailable: ${e}`);
      btnSignalProceed.disabled = false;
    } finally {
      diagCurrentPromise = null;
    }
  })();

  await diagCurrentPromise;
  if (signalLoopEnabled) setTimeout(runSignalCheckLoop, 1000);
}

async function connectNode() {
  if (!selectedAddressNode) return;
  clearError();
  if (scanning) await stopScan();
  setBusy(true);
  navigator_.next();
  resetSignalCheckView();
  updateSignalCheckIndicators(null);
  showStatus(signalStatus, "Connecting to node…");
  try {
    await invoke("connect_node", { address: selectedAddressNode });
    signalLoopEnabled = true;
    btnSignalProceed.disabled = false;
    showStatus(signalStatus, "Running ESP-NOW signal check…");
    setBusy(false);
    runSignalCheckLoop();
  } catch (e) {
    navigator_.back();
    showError(e);
    setBusy(false);
  }
}

async function abortSignalCheck() {
  clearError();
  setBusy(true);
  stopSignalLoop();
  await waitForCurrentDiagnostic();
  try {
    await invoke("disconnect_node");
  } catch (e) {
    showError(e);
  } finally {
    resetSignalCheckView();
    navigator_.goTo(3);
    setBusy(false);
  }
}

async function proceedFromSignalCheck() {
  clearError();
  setBusy(true);
  stopSignalLoop();
  await waitForCurrentDiagnostic();
  btnProvision.disabled = !selectedAddressNode;
  navigator_.next();
  setBusy(false);
}

async function provisionNode() {
  if (!selectedAddressNode) return;
  const nid = nodeId.value.trim();
  if (!nid) {
    showError("Enter a Node ID");
    return;
  }
  const boardLayout = resolveBoardLayout();
  if (!boardLayout) return;
  clearError();
  setBusy(true);
  showStatus(provisionStatus, "Provisioning…");
  try {
    await invoke("provision_node", {
      address: selectedAddressNode,
      nodeId: nid,
      boardLayout,
    });
    hideStatus(provisionStatus);
    resetSignalCheckView();
    provisionDetails.textContent = `Node "${nid}" provisioned.`;
    navigator_.next();
  } catch (e) {
    hideStatus(provisionStatus);
    showError(e);
  } finally {
    setBusy(false);
  }
}

async function refreshPairingStatus() {
  try {
    const status = await invoke("get_pairing_status");
    if (status.paired) {
      isPaired = true;
      const gwLabel = `Gateway ${(status.gateway_id || "").substring(0, 8)}…`;
      pairingStatus.textContent = `Paired — ${gwLabel}`;
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
    pairingStatus.textContent = `Error: ${e}`;
  }
}

async function clearPairing() {
  clearError();
  stopSignalLoop();
  await waitForCurrentDiagnostic();
  try {
    await invoke("clear_pairing");
    isPaired = false;
    selectedAddressNode = null;
    selectedNodeBleRssi = null;
    updateBleSelectionIndicator(null);
    resetSignalCheckView();
    await refreshPairingStatus();
    navigator_.goTo(0);
  } catch (e) {
    showError(e);
  }
}

function toggleVerbose() {
  const enabled = verboseToggle.checked;
  logPanel.classList.toggle("hidden", !enabled);
  if (enabled) logTimer = setInterval(pollLogs, 1000);
  else if (logTimer) {
    clearInterval(logTimer);
    logTimer = null;
  }
}

async function pollLogs() {
  try {
    const lines = await invoke("get_logs");
    if (lines.length > 0) {
      logPanel.textContent += `${lines.join("\n")}\n`;
      logPanel.scrollTop = logPanel.scrollHeight;
    }
  } catch (_) {
  }
}

btnBack.addEventListener("click", () => history.back());
btnGetStarted.addEventListener("click", () => navigator_.next());
btnSkipToNode.addEventListener("click", () => navigator_.goTo(3));
btnClear.addEventListener("click", clearPairing);
btnScanStartGw.addEventListener("click", startScan);
btnScanStopGw.addEventListener("click", stopScan);
btnPair.addEventListener("click", pairGateway);
btnToNode.addEventListener("click", () => navigator_.next());
btnClearGwDone.addEventListener("click", clearPairing);
btnScanStartNode.addEventListener("click", startScan);
btnScanStopNode.addEventListener("click", stopScan);
btnConnectNode.addEventListener("click", connectNode);
btnSignalAbort.addEventListener("click", abortSignalCheck);
btnSignalProceed.addEventListener("click", proceedFromSignalCheck);
btnProvision.addEventListener("click", provisionNode);
boardSelect.addEventListener("change", () => {
  customPins.classList.toggle("hidden", boardSelect.value !== "custom");
});
btnProvisionAnother.addEventListener("click", () => {
  nodeId.value = "";
  selectedAddressNode = null;
  selectedNodeBleRssi = null;
  updateBleSelectionIndicator(null);
  resetSignalCheckView();
  renderDevices(deviceListNode, [], false);
  navigator_.goTo(3);
});
verboseToggle.addEventListener("change", toggleVerbose);

window.addEventListener("popstate", (event) => {
  if (event.state && event.state.sentinel) {
    history.pushState({ page: 0 }, "", "");
    navigator_.goTo(0, { push: false });
    return;
  }
  if (event.state && typeof event.state.page === "number") {
    navigator_.goTo(event.state.page, { push: false });
  }
});

initBoardSelect();
resetSignalCheckView();
refreshPairingStatus().then(() => navigator_.restore());
