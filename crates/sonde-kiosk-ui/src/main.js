// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

const invoke = globalThis.window?.__TAURI__?.core?.invoke;

const ENV_GUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const ENV_STORAGE_ACCOUNT_PATTERN = /^[a-z0-9]{3,24}$/;
const ENV_FUNCTION_APP_PATTERN = /^[a-zA-Z0-9][a-zA-Z0-9-]{0,58}[a-zA-Z0-9]$/;
const SENSOR_VIEW_MODES = new Set(['graph', 'table']);
const SENSOR_TIME_RANGES = new Set(['1h', '24h', '7d']);
const BLOCKED_OBJECT_KEYS = new Set(['__proto__', 'constructor', 'prototype']);
const OPERATOR_LONG_PRESS_MS = 800;
const HORIZONTAL_SWIPE_THRESHOLD_PX = 50;
const PULL_TO_REFRESH_THRESHOLD_PX = 90;
const PULL_TO_REFRESH_MAX_HORIZONTAL_DRIFT_PX = 40;
const BACKGROUND_REFRESH_INTERVAL_MS = 60 * 1000;

const APP_STATE = {
  runtime: null,
  activeEnvironment: null,
  activeDashboardIndex: 0,
  metricCharts: {},
  telemetryNotice: 'Waiting for live telemetry refresh.',
  telemetryStatusKind: 'info',
  telemetryCache: new Map(),
  refreshGeneration: 0,
  refreshTimer: null,
  refreshIntervalMs: BACKGROUND_REFRESH_INTERVAL_MS,
  operatorPanelOpen: false,
  operatorPressTimer: null,
  dependencies: {},
};

function createDefaultSensorDataPreferences() {
  return {
    viewMode: 'graph',
    timeRange: '24h',
    selectedSeries: [],
    selectedSeriesInitialized: false,
    seriesOverrides: {},
  };
}

function normalizeSeriesOverrideEntry(entry) {
  if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) {
    return null;
  }
  const displayName = typeof entry.displayName === 'string' ? entry.displayName : '';
  const unitSuffix = typeof entry.unitSuffix === 'string' ? entry.unitSuffix : '';
  const scaleDivisor = typeof entry.scaleDivisor === 'number' && Number.isFinite(entry.scaleDivisor)
    ? entry.scaleDivisor
    : 0;
  if (!displayName && !unitSuffix && scaleDivisor === 0) {
    return null;
  }
  return {
    displayName,
    scaleDivisor,
    unitSuffix,
  };
}

function validateImportedSensorDataPreferences(rawPreferences) {
  if (rawPreferences === undefined) {
    return createDefaultSensorDataPreferences();
  }
  if (typeof rawPreferences !== 'object' || rawPreferences === null || Array.isArray(rawPreferences)) {
    throw new Error('`sensorData` must be a JSON object.');
  }

  const preferences = createDefaultSensorDataPreferences();

  if ('viewMode' in rawPreferences) {
    if (typeof rawPreferences.viewMode !== 'string' || !SENSOR_VIEW_MODES.has(rawPreferences.viewMode)) {
      throw new Error('`sensorData.viewMode` must be `graph` or `table`.');
    }
    preferences.viewMode = rawPreferences.viewMode;
  }

  if ('timeRange' in rawPreferences) {
    if (typeof rawPreferences.timeRange !== 'string' || !SENSOR_TIME_RANGES.has(rawPreferences.timeRange)) {
      throw new Error('`sensorData.timeRange` must be one of `1h`, `24h`, or `7d`.');
    }
    preferences.timeRange = rawPreferences.timeRange;
  }

  if ('selectedSeries' in rawPreferences) {
    if (!Array.isArray(rawPreferences.selectedSeries) || rawPreferences.selectedSeries.some((value) => typeof value !== 'string')) {
      throw new Error('`sensorData.selectedSeries` must be an array of strings.');
    }
    preferences.selectedSeries = [...rawPreferences.selectedSeries];
    preferences.selectedSeriesInitialized = true;
  }

  if ('seriesOverrides' in rawPreferences) {
    if (typeof rawPreferences.seriesOverrides !== 'object' || rawPreferences.seriesOverrides === null || Array.isArray(rawPreferences.seriesOverrides)) {
      throw new Error('`sensorData.seriesOverrides` must be an object.');
    }
    const overrides = {};
    for (const [seriesKey, entry] of Object.entries(rawPreferences.seriesOverrides)) {
      if (BLOCKED_OBJECT_KEYS.has(seriesKey)) {
        throw new Error(`\`sensorData.seriesOverrides.${seriesKey}\` uses a reserved key.`);
      }
      if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) {
        throw new Error(`\`sensorData.seriesOverrides.${seriesKey}\` must be an object.`);
      }
      if ('displayName' in entry && typeof entry.displayName !== 'string') {
        throw new Error(`\`sensorData.seriesOverrides.${seriesKey}.displayName\` must be a string.`);
      }
      if ('unitSuffix' in entry && typeof entry.unitSuffix !== 'string') {
        throw new Error(`\`sensorData.seriesOverrides.${seriesKey}.unitSuffix\` must be a string.`);
      }
      if ('scaleDivisor' in entry && (typeof entry.scaleDivisor !== 'number' || !Number.isFinite(entry.scaleDivisor))) {
        throw new Error(`\`sensorData.seriesOverrides.${seriesKey}.scaleDivisor\` must be a finite number.`);
      }
      const normalizedEntry = normalizeSeriesOverrideEntry(entry);
      if (normalizedEntry) {
        overrides[seriesKey] = normalizedEntry;
      }
    }
    preferences.seriesOverrides = overrides;
  }

  return preferences;
}

function validateEnvironmentFields(fields) {
  if (!fields.clientId || typeof fields.clientId !== 'string') return 'Client ID is required.';
  if (!fields.tenantId || typeof fields.tenantId !== 'string') return 'Tenant ID is required.';
  if (!fields.storageAccount || typeof fields.storageAccount !== 'string') return 'Storage Account is required.';
  if (!fields.functionAppName || typeof fields.functionAppName !== 'string') return 'Function App Name is required.';
  if (!ENV_GUID_PATTERN.test(fields.clientId)) return 'Client ID must be a valid GUID.';
  if (!ENV_GUID_PATTERN.test(fields.tenantId)) return 'Tenant ID must be a valid GUID.';
  if (!ENV_STORAGE_ACCOUNT_PATTERN.test(fields.storageAccount)) return 'Storage Account must be 3–24 lowercase alphanumeric characters.';
  if (!ENV_FUNCTION_APP_PATTERN.test(fields.functionAppName) || fields.functionAppName.length < 2) return 'Function App Name must be 2–60 alphanumeric characters with optional hyphens.';
  return null;
}

async function loadSharedDashboardRuntime(deps = {}) {
  if (deps.runtime) {
    APP_STATE.runtime = deps.runtime;
    return deps.runtime;
  }
  if (APP_STATE.runtime) {
    return APP_STATE.runtime;
  }
  if (globalThis.SondeDashboardRuntime) {
    APP_STATE.runtime = globalThis.SondeDashboardRuntime;
    return APP_STATE.runtime;
  }
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  const source = await invokeFn('shared_dashboard_runtime_source');
  const blob = new Blob([source], { type: 'text/javascript' });
  const url = URL.createObjectURL(blob);
  try {
    await new Promise((resolve, reject) => {
      const script = document.createElement('script');
      script.src = url;
      script.onload = () => resolve();
      script.onerror = () => reject(new Error('Failed to load shared dashboard runtime.'));
      document.head.appendChild(script);
    });
  } finally {
    URL.revokeObjectURL(url);
  }
  if (!globalThis.SondeDashboardRuntime) {
    throw new Error('Shared dashboard runtime did not register itself.');
  }
  APP_STATE.runtime = globalThis.SondeDashboardRuntime;
  return APP_STATE.runtime;
}

function validateImportedEnvironmentJson(text, runtime, deps = {}) {
  let data;
  try {
    data = JSON.parse(text);
  } catch {
    throw new Error('File does not contain valid JSON.');
  }
  if (data === null || typeof data !== 'object' || Array.isArray(data)) {
    throw new Error('File must contain a JSON object (not an array or primitive).');
  }
  if (data.version !== 1) {
    throw new Error(`Unsupported environment file version: ${data.version ?? 'missing'}. Expected version 1.`);
  }

  const fields = {
    clientId: typeof data.clientId === 'string' ? data.clientId.trim() : '',
    tenantId: typeof data.tenantId === 'string' ? data.tenantId.trim() : '',
    storageAccount: typeof data.storageAccount === 'string' ? data.storageAccount.trim() : '',
    functionAppName: typeof data.functionAppName === 'string' ? data.functionAppName.trim() : '',
  };
  const validationError = validateEnvironmentFields(fields);
  if (validationError) {
    throw new Error(validationError);
  }

  const sensorData = validateImportedSensorDataPreferences(data.sensorData);
  const dashboards = Array.isArray(data.dashboards)
    ? data.dashboards.map((dashboard, index) => runtime.validateImportedDashboard(dashboard, index, {
        validateVariableNameFn: runtime.validateVariableName,
      }))
    : [];

  let name = typeof data.name === 'string' ? data.name.trim() : '';
  if (!name) {
    const promptFn = deps.promptFn || globalThis.window?.prompt;
    if (typeof promptFn !== 'function') {
      throw new Error('Import cancelled — no name provided.');
    }
    name = promptFn('Enter a name for this environment:');
    if (!name || !name.trim()) {
      throw new Error('Import cancelled — no name provided.');
    }
    name = name.trim();
  }

  return runtime.normalizeEnvironmentRecord({
    name,
    ...fields,
    sensorData,
    dashboards,
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });
}

function buildDashboardOverlay(environment, activeDashboardIndex) {
  const dashboard = environment.dashboards[activeDashboardIndex];
  return `${dashboard.name} (${activeDashboardIndex + 1}/${environment.dashboards.length})`;
}

function renderDashboardFrame(runtime, environment, activeDashboardIndex, statusMessage) {
  const dashboard = environment.dashboards[activeDashboardIndex];
  return `
    <div class="dashboard-frame">
      <div class="dashboard-page-status text-muted">${statusMessage}</div>
      ${runtime.renderReadOnlyDashboardPage(dashboard)}
    </div>
  `;
}

function setTelemetryNotice(text, kind = 'info') {
  APP_STATE.telemetryNotice = text;
  APP_STATE.telemetryStatusKind = kind;

  const status = document.getElementById('dashboard-status');
  if (status) {
    status.textContent = text;
    status.className = `status-pill status-pill--${kind}`;
  }

  const pageStatus = document.querySelector?.('.dashboard-page-status');
  if (pageStatus) {
    pageStatus.textContent = text;
    pageStatus.className = `dashboard-page-status dashboard-page-status--${kind}`;
  }
}

function clearTelemetryCache() {
  APP_STATE.telemetryCache.clear();
}

function getActiveDashboard() {
  if (!APP_STATE.activeEnvironment) {
    return null;
  }
  return APP_STATE.activeEnvironment.dashboards[APP_STATE.activeDashboardIndex] ?? null;
}

function buildTelemetrySourceCacheKey(environment, source) {
  return JSON.stringify({
    clientId: environment.clientId,
    storageAccount: environment.storageAccount,
    nodeId: source.nodeId,
    readingType: source.readingType,
  });
}

function filterTelemetryPointsToRange(points, startMs, endMs) {
  return points.filter((point) => Number.isFinite(point.timestampMs)
    && point.timestampMs >= startMs
    && point.timestampMs <= endMs);
}

function buildCachedVariableData(runtime, environment, dashboard, nowMs = Date.now()) {
  const { startMs, endMs } = runtime.getDashboardTimeRangeBounds(dashboard.timeRange, nowMs);
  const result = Object.create(null);

  for (const variable of dashboard.variables) {
    const cacheKey = buildTelemetrySourceCacheKey(environment, variable);
    const cached = APP_STATE.telemetryCache.get(cacheKey);
    result[variable.name] = cached
      ? filterTelemetryPointsToRange(cached.points, startMs, endMs)
      : [];
  }

  return result;
}

function buildDashboardRefreshRequest(environment, dashboard, runtime, nowMs = Date.now()) {
  const { startMs, endMs } = runtime.getDashboardTimeRangeBounds(dashboard.timeRange, nowMs);
  const seenSources = new Set();
  const variables = [];

  for (const variable of dashboard.variables) {
    const sourceKey = `${variable.nodeId}\n${variable.readingType}`;
    if (seenSources.has(sourceKey)) {
      continue;
    }
    seenSources.add(sourceKey);
    variables.push({
      nodeId: variable.nodeId,
      readingType: variable.readingType,
    });
  }

  return {
    clientId: environment.clientId,
    tenantId: environment.tenantId,
    storageAccount: environment.storageAccount,
    functionAppName: environment.functionAppName,
    startMs,
    endMs,
    variables,
  };
}

function normalizeTelemetryPoints(points) {
  if (!Array.isArray(points)) {
    return [];
  }

  return points
    .filter((point) => typeof point === 'object' && point !== null)
    .map((point) => ({
      timestampMs: Number(point.timestampMs),
      value: Number(point.value),
    }))
    .filter((point) => Number.isFinite(point.timestampMs) && Number.isFinite(point.value))
    .sort((left, right) => left.timestampMs - right.timestampMs);
}

function cacheTelemetryRefreshResponse(environment, request, response) {
  for (const series of response?.series ?? []) {
    if (typeof series !== 'object' || series === null) {
      continue;
    }

    const key = buildTelemetrySourceCacheKey(environment, {
      nodeId: series.nodeId,
      readingType: series.readingType,
    });
    APP_STATE.telemetryCache.set(key, {
      points: normalizeTelemetryPoints(series.points),
      coverageStartMs: request.startMs,
      coverageEndMs: request.endMs,
      refreshedAtMs: Number.isFinite(response.refreshedAtMs) ? Number(response.refreshedAtMs) : Date.now(),
    });
  }
}

async function fetchDashboardVariableData(request, deps = APP_STATE.dependencies) {
  if (typeof deps.fetchDashboardVariableDataFn === 'function') {
    return deps.fetchDashboardVariableDataFn(request);
  }

  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }

  return invokeFn('fetch_dashboard_variable_data', { request });
}

function createCachedMetricEvaluator(runtime, environment, dashboard, nowMs = Date.now()) {
  const cachedVariableData = buildCachedVariableData(runtime, environment, dashboard, nowMs);
  return async (metric, variables, timeRange) => runtime.evaluateMetricTimeSeries(metric, variables, timeRange, {
    fetchVariableDataFn: async (usedVariables) => {
      const result = Object.create(null);
      for (const variable of usedVariables) {
        result[variable.name] = cachedVariableData[variable.name]
          ? [...cachedVariableData[variable.name]]
          : [];
      }
      return result;
    },
  });
}

function interpretDashboardGesture(deltaX, deltaY) {
  if (deltaY >= PULL_TO_REFRESH_THRESHOLD_PX && Math.abs(deltaX) <= PULL_TO_REFRESH_MAX_HORIZONTAL_DRIFT_PX) {
    return 'refresh';
  }
  if (Math.abs(deltaX) >= HORIZONTAL_SWIPE_THRESHOLD_PX && Math.abs(deltaX) > Math.abs(deltaY)) {
    return deltaX < 0 ? 'next' : 'previous';
  }
  return null;
}

function destroyDashboardCharts() {
  for (const chart of Object.values(APP_STATE.metricCharts)) {
    if (chart && typeof chart.destroy === 'function') {
      chart.destroy();
    }
  }
  APP_STATE.metricCharts = {};
}

async function renderActiveDashboard(deps = APP_STATE.dependencies) {
  const environment = APP_STATE.activeEnvironment;
  const runtime = APP_STATE.runtime;
  if (!environment || !runtime) {
    return;
  }
  const pageHost = document.getElementById('dashboard-page-host');
  const overlay = document.getElementById('dashboard-overlay');
  const status = document.getElementById('dashboard-status');
  if (!pageHost || !overlay || !status) {
    return;
  }

  destroyDashboardCharts();
  overlay.classList.remove('hidden');
  overlay.textContent = buildDashboardOverlay(environment, APP_STATE.activeDashboardIndex);
  status.className = `status-pill status-pill--${APP_STATE.telemetryStatusKind}`;
  status.textContent = APP_STATE.telemetryNotice;
  pageHost.innerHTML = renderDashboardFrame(runtime, environment, APP_STATE.activeDashboardIndex, APP_STATE.telemetryNotice);

  const dashboard = environment.dashboards[APP_STATE.activeDashboardIndex];
  if (dashboard.charts.length > 0) {
    await runtime.renderMetricCharts(dashboard, {
      document,
      destroyChartFn: (chartIndex) => {
        const chart = APP_STATE.metricCharts[chartIndex];
        if (chart && typeof chart.destroy === 'function') {
          chart.destroy();
        }
        delete APP_STATE.metricCharts[chartIndex];
      },
      storeChartInstanceFn: (chartIndex, chart) => {
        APP_STATE.metricCharts[chartIndex] = chart;
      },
      evaluateMetricTimeSeriesFn: createCachedMetricEvaluator(
        runtime,
        environment,
        dashboard,
        (deps.nowFn || Date.now)(),
      ),
    });
  }
}

function stopBackgroundRefreshLoop(deps = APP_STATE.dependencies) {
  if (APP_STATE.refreshTimer == null) {
    return;
  }
  const clearIntervalFn = deps.clearIntervalFn || globalThis.clearInterval;
  if (typeof clearIntervalFn === 'function') {
    clearIntervalFn(APP_STATE.refreshTimer);
  }
  APP_STATE.refreshTimer = null;
}

function startBackgroundRefreshLoop(deps = APP_STATE.dependencies) {
  stopBackgroundRefreshLoop(deps);
  const setIntervalFn = deps.setIntervalFn || globalThis.setInterval;
  if (typeof setIntervalFn !== 'function') {
    return;
  }
  APP_STATE.refreshTimer = setIntervalFn(() => {
    void triggerDashboardRefresh('background', deps);
  }, APP_STATE.refreshIntervalMs);
}

async function triggerDashboardRefresh(reason = 'background', deps = APP_STATE.dependencies) {
  const runtime = APP_STATE.runtime;
  const environment = APP_STATE.activeEnvironment;
  const dashboard = getActiveDashboard();
  if (!runtime || !environment || !dashboard) {
    return;
  }

  const nowFn = deps.nowFn || Date.now;
  const refreshRequest = buildDashboardRefreshRequest(environment, dashboard, runtime, nowFn());
  if (refreshRequest.variables.length === 0) {
    setTelemetryNotice('No telemetry variables are defined for this dashboard.', 'info');
    return;
  }
  const refreshGeneration = ++APP_STATE.refreshGeneration;
  const dashboardIndexAtStart = APP_STATE.activeDashboardIndex;
  const refreshingMessage = reason === 'manual'
    ? 'Manual refresh in progress…'
    : reason === 'switch'
      ? 'Loading live data for this dashboard…'
      : 'Refreshing live dashboard data…';
  setTelemetryNotice(refreshingMessage, 'refreshing');

  try {
    const response = await fetchDashboardVariableData(refreshRequest, deps);
    cacheTelemetryRefreshResponse(environment, refreshRequest, response);
    if (refreshGeneration !== APP_STATE.refreshGeneration
      || environment !== APP_STATE.activeEnvironment
      || dashboardIndexAtStart !== APP_STATE.activeDashboardIndex) {
      return;
    }
    const refreshedAtMs = Number.isFinite(response?.refreshedAtMs)
      ? Number(response.refreshedAtMs)
      : nowFn();
    setTelemetryNotice(`Live data refreshed at ${new Date(refreshedAtMs).toLocaleTimeString()}.`, 'live');
    await renderActiveDashboard(deps);
  } catch (error) {
    if (refreshGeneration !== APP_STATE.refreshGeneration
      || environment !== APP_STATE.activeEnvironment
      || dashboardIndexAtStart !== APP_STATE.activeDashboardIndex) {
      return;
    }
    const message = error instanceof Error ? error.message : String(error);
    setTelemetryNotice(`Live refresh unavailable: ${message}`, 'error');
  }
}

async function showDashboardMode(deps = APP_STATE.dependencies) {
  document.getElementById('setup-screen')?.classList.add('hidden');
  document.getElementById('dashboard-screen')?.classList.remove('hidden');
  await renderActiveDashboard(deps);
  startBackgroundRefreshLoop(deps);
  await triggerDashboardRefresh('initial', deps);
}

function showSetupMode(message) {
  stopBackgroundRefreshLoop();
  destroyDashboardCharts();
  APP_STATE.activeEnvironment = null;
  APP_STATE.activeDashboardIndex = 0;
  APP_STATE.refreshGeneration = 0;
  document.getElementById('dashboard-screen')?.classList.add('hidden');
  document.getElementById('setup-screen')?.classList.remove('hidden');
  document.getElementById('dashboard-overlay')?.classList.add('hidden');
  const status = document.getElementById('setup-status');
  if (status) {
    status.textContent = message;
  }
}

async function loadStoredEnvironment(deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    return null;
  }
  const raw = await invokeFn('get_environment_json');
  if (!raw) {
    return null;
  }
  const runtime = deps.runtime || APP_STATE.runtime;
  if (!runtime) {
    throw new Error('Shared dashboard runtime is not loaded.');
  }
  return runtime.normalizeEnvironmentRecord(JSON.parse(raw), {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });
}

async function persistEnvironment(environment, deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  await invokeFn('save_environment_json', { json: JSON.stringify(environment) });
}

async function clearStoredEnvironment(deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  await invokeFn('clear_environment_json');
}

function moveDashboard(delta, deps = APP_STATE.dependencies) {
  if (!APP_STATE.activeEnvironment || !APP_STATE.activeEnvironment.dashboards.length) {
    return;
  }
  const nextIndex = APP_STATE.activeDashboardIndex + delta;
  if (nextIndex < 0 || nextIndex >= APP_STATE.activeEnvironment.dashboards.length) {
    return;
  }
  APP_STATE.activeDashboardIndex = nextIndex;
  renderActiveDashboard(deps)
    .then(() => triggerDashboardRefresh('switch', deps))
    .catch((error) => showSetupMode(error.message));
}

function hideOperatorPanel() {
  APP_STATE.operatorPanelOpen = false;
  document.getElementById('operator-panel')?.classList.add('hidden');
}

function showOperatorPanel() {
  APP_STATE.operatorPanelOpen = true;
  document.getElementById('operator-panel')?.classList.remove('hidden');
}

function installSwipeNavigation(host, deps = APP_STATE.dependencies) {
  let touchStartX = null;
  let touchStartY = null;
  host.addEventListener('touchstart', (event) => {
    touchStartX = event.changedTouches[0]?.clientX ?? null;
    touchStartY = event.changedTouches[0]?.clientY ?? null;
  }, { passive: true });
  host.addEventListener('touchend', (event) => {
    const endX = event.changedTouches[0]?.clientX ?? null;
    const endY = event.changedTouches[0]?.clientY ?? null;
    if (!Number.isFinite(touchStartX) || !Number.isFinite(touchStartY)
      || !Number.isFinite(endX) || !Number.isFinite(endY)) {
      touchStartX = null;
      touchStartY = null;
      return;
    }
    const deltaX = endX - touchStartX;
    const deltaY = endY - touchStartY;
    touchStartX = null;
    touchStartY = null;
    switch (interpretDashboardGesture(deltaX, deltaY)) {
      case 'next':
        moveDashboard(1, deps);
        break;
      case 'previous':
        moveDashboard(-1, deps);
        break;
      case 'refresh':
        void triggerDashboardRefresh('manual', deps);
        break;
      default:
        break;
    }
  }, { passive: true });
}

function installOperatorControls(fileInput, deps = APP_STATE.dependencies) {
  const hotspot = document.getElementById('operator-hotspot');
  const reimport = document.getElementById('operator-reimport');
  const reset = document.getElementById('operator-reset');
  const close = document.getElementById('operator-close');

  if (hotspot) {
    const beginPress = () => {
      clearTimeout(APP_STATE.operatorPressTimer);
      APP_STATE.operatorPressTimer = setTimeout(() => {
        showOperatorPanel();
      }, OPERATOR_LONG_PRESS_MS);
    };
    const cancelPress = () => {
      clearTimeout(APP_STATE.operatorPressTimer);
      APP_STATE.operatorPressTimer = null;
    };
    hotspot.addEventListener('pointerdown', beginPress);
    hotspot.addEventListener('pointerup', cancelPress);
    hotspot.addEventListener('pointerleave', cancelPress);
    hotspot.addEventListener('pointercancel', cancelPress);
  }

  reimport?.addEventListener('click', () => {
    hideOperatorPanel();
    fileInput.click();
  });
  reset?.addEventListener('click', async () => {
    await clearStoredEnvironment(deps);
    clearTelemetryCache();
    hideOperatorPanel();
    showSetupMode('Imported environment cleared. Import a new SPA environment JSON to resume kiosk mode.');
  });
  close?.addEventListener('click', hideOperatorPanel);
}

async function importEnvironmentFromText(text, deps = {}) {
  const runtime = deps.runtime || APP_STATE.runtime;
  if (!runtime) {
    throw new Error('Shared dashboard runtime is not loaded.');
  }
  const environment = validateImportedEnvironmentJson(text, runtime, deps);
  await persistEnvironment(environment, deps);
  clearTelemetryCache();
  setTelemetryNotice('Waiting for live telemetry refresh.', 'info');
  APP_STATE.activeEnvironment = environment;
  APP_STATE.activeDashboardIndex = 0;
  await showDashboardMode(deps);
}

async function initKioskApp(deps = {}) {
  const runtime = await loadSharedDashboardRuntime(deps);
  APP_STATE.runtime = runtime;
  APP_STATE.dependencies = deps;

  const fileInput = document.getElementById('import-file');
  const importButton = document.getElementById('import-button');
  const pageHost = document.getElementById('dashboard-page-host');

  if (!fileInput || !importButton || !pageHost) {
    throw new Error('Kiosk UI is missing required DOM elements.');
  }

  importButton.addEventListener('click', () => fileInput.click());
  fileInput.addEventListener('change', async () => {
    const file = fileInput.files?.[0];
    if (!file) {
      return;
    }
    try {
      await importEnvironmentFromText(await file.text(), deps);
    } catch (error) {
      showSetupMode(`Import failed: ${error.message}`);
    } finally {
      fileInput.value = '';
    }
  });

  installSwipeNavigation(pageHost, deps);
  installOperatorControls(fileInput, deps);
  document.addEventListener('keydown', (event) => {
    if (APP_STATE.operatorPanelOpen) {
      if (event.key === 'Escape') {
        hideOperatorPanel();
      }
      return;
    }
    if (event.key === 'ArrowLeft') {
      moveDashboard(-1, deps);
    } else if (event.key === 'ArrowRight') {
      moveDashboard(1, deps);
    }
  });

  const storedEnvironment = await loadStoredEnvironment({ ...deps, runtime });
  if (storedEnvironment) {
    clearTelemetryCache();
    setTelemetryNotice('Waiting for live telemetry refresh.', 'info');
    APP_STATE.activeEnvironment = storedEnvironment;
    await showDashboardMode(deps);
  } else {
    showSetupMode('No environment imported yet.');
  }
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    APP_STATE,
    BACKGROUND_REFRESH_INTERVAL_MS,
    createDefaultSensorDataPreferences,
    buildCachedVariableData,
    buildDashboardRefreshRequest,
    cacheTelemetryRefreshResponse,
    fetchDashboardVariableData,
    initKioskApp,
    interpretDashboardGesture,
    loadSharedDashboardRuntime,
    renderDashboardFrame,
    startBackgroundRefreshLoop,
    triggerDashboardRefresh,
    validateEnvironmentFields,
    validateImportedEnvironmentJson,
    validateImportedSensorDataPreferences,
  };
}

document.addEventListener('DOMContentLoaded', () => {
  initKioskApp().catch((error) => {
    showSetupMode(`Kiosk startup failed: ${error.message}`);
  });
});
