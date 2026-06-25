// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

const invoke = globalThis.window?.__TAURI__?.core?.invoke;

const ENV_GUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const ENV_STORAGE_ACCOUNT_PATTERN = /^[a-z0-9]{3,24}$/;
const ENV_FUNCTION_APP_PATTERN = /^[a-zA-Z0-9][a-zA-Z0-9-]{0,58}[a-zA-Z0-9]$/;
const HTTPS_AUTHORITY_PATTERN = /^https:\/\/[^/\s?#@]+$/i;
const SENSOR_VIEW_MODES = new Set(['graph', 'table']);
const SENSOR_TIME_RANGES = new Set(['1h', '24h', '7d']);
const BLOCKED_OBJECT_KEYS = new Set(['__proto__', 'constructor', 'prototype']);
const OPERATOR_LONG_PRESS_MS = 800;
const HORIZONTAL_SWIPE_THRESHOLD_PX = 50;
const PULL_TO_REFRESH_THRESHOLD_PX = 90;
const PULL_TO_REFRESH_MAX_HORIZONTAL_DRIFT_PX = 40;
const BACKGROUND_REFRESH_INTERVAL_MS = 900 * 1000;
const DEVICE_CODE_POLL_FALLBACK_MS = 5000;
const TELEMETRY_CACHE_MAX_SERIES = 128;
const TELEMETRY_CACHE_MAX_POINTS_PER_SERIES = 2048;
const OVERLAY_AUTO_HIDE_MS = 2200;
const STATUS_AUTO_HIDE_MS = 3200;
const STATUS_ERROR_AUTO_HIDE_MS = 5200;

const APP_STATE = {
  runtime: null,
  activeEnvironment: null,
  activeDashboardIndex: 0,
  activeChartIndex: 0,
  metricCharts: {},
  telemetryNotice: 'Waiting for live telemetry refresh.',
  telemetryStatusKind: 'info',
  telemetryCache: new Map(),
  refreshGeneration: 0,
  refreshTimer: null,
  refreshIntervalMs: BACKGROUND_REFRESH_INTERVAL_MS,
  refreshInFlightPromise: null,
  operatorPanelOpen: false,
  operatorPressTimer: null,
  overlayHideTimer: null,
  statusHideTimer: null,
  identitySummary: null,
  deviceCodeSession: null,
  setupStatusMessage: 'No environment imported yet.',
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

function normalizeOptionalGuid(value) {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeOptionalLoginEndpoint(value) {
  return typeof value === 'string' ? value.trim().replace(/\/+$/, '') : '';
}

function describeError(error) {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

function validateSetupLoginMetadata(metadata) {
  const loginEndpoint = normalizeOptionalLoginEndpoint(metadata?.loginEndpoint);
  const kioskSetupClientId = normalizeOptionalGuid(metadata?.kioskSetupClientId);
  if (!loginEndpoint || !kioskSetupClientId) {
    return {
      valid: false,
      loginEndpoint,
      kioskSetupClientId,
      error: 'This environment is missing kiosk setup login metadata. Re-export it after Azure provisioning adds the kiosk setup client.',
    };
  }

  if (!ENV_GUID_PATTERN.test(kioskSetupClientId)) {
    return {
      valid: false,
      loginEndpoint,
      kioskSetupClientId,
      error: 'Kiosk Setup Client ID must be a valid GUID.',
    };
  }
  if (!HTTPS_AUTHORITY_PATTERN.test(loginEndpoint)) {
    return {
      valid: false,
      loginEndpoint,
      kioskSetupClientId,
      error: 'Login endpoint must be a valid HTTPS authority URL.',
    };
  }
  return {
    valid: true,
    loginEndpoint,
    kioskSetupClientId,
  };
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
    loginEndpoint: normalizeOptionalLoginEndpoint(data.loginEndpoint),
    kioskSetupClientId: normalizeOptionalGuid(data.kioskSetupClientId),
    sensorData,
    dashboards,
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });
}

function buildChartPages(environment) {
  if (!environment || !Array.isArray(environment.dashboards)) {
    return [];
  }
  const pages = [];
  environment.dashboards.forEach((dashboard, dashboardIndex) => {
    if (!dashboard || !Array.isArray(dashboard.charts)) {
      return;
    }
    dashboard.charts.forEach((chart, chartIndex) => {
      pages.push({
        dashboardIndex,
        chartIndex,
        dashboard,
        chart,
      });
    });
  });
  return pages;
}

function getActiveChartPage(environment = APP_STATE.activeEnvironment) {
  const pages = buildChartPages(environment);
  if (pages.length === 0) {
    return {
      pages,
      page: null,
      pageIndex: -1,
    };
  }
  let pageIndex = pages.findIndex((page) => page.dashboardIndex === APP_STATE.activeDashboardIndex
    && page.chartIndex === APP_STATE.activeChartIndex);
  if (pageIndex < 0) {
    pageIndex = 0;
  }
  return {
    pages,
    page: pages[pageIndex],
    pageIndex,
  };
}

function buildChartOverlayText(environment, activeDashboardIndex, activeChartIndex = 0) {
  const pages = buildChartPages(environment);
  const pageIndex = pages.findIndex((page) => page.dashboardIndex === activeDashboardIndex
    && page.chartIndex === activeChartIndex);
  const activePage = pageIndex >= 0 ? pages[pageIndex] : pages[0];
  if (!activePage) {
    return '';
  }
  return activePage.dashboard.name;
}

function buildChartRenderDashboard(page) {
  if (!page) {
    return null;
  }
  return {
    name: page.dashboard.name,
    variables: page.dashboard.variables,
    charts: [page.chart],
    timeRange: page.dashboard.timeRange,
  };
}

function renderDashboardFrame(runtime, environment, activeDashboardIndex, activeChartIndex = 0) {
  const pages = buildChartPages(environment);
  const activePage = pages.find((page) => page.dashboardIndex === activeDashboardIndex
    && page.chartIndex === activeChartIndex) ?? pages[0];
  if (!activePage) {
    return `
      <div class="kiosk-chart-page">
        <div class="kiosk-chart-frame">
          <div class="kiosk-empty-state">No charts are defined in the imported dashboards.</div>
        </div>
      </div>
    `;
  }
  if (!Array.isArray(activePage.chart.metrics) || activePage.chart.metrics.length === 0) {
    return `
      <div class="kiosk-chart-page">
        <div class="kiosk-chart-frame">
          <div class="kiosk-empty-state">No metrics are defined for this chart.</div>
        </div>
      </div>
    `;
  }
  return `
    <div class="kiosk-chart-page">
      <div class="kiosk-chart-frame">
        <div class="metric-chart-container">
          <canvas id="metric-chart-0"></canvas>
        </div>
      </div>
    </div>
  `;
}

function clearOverlayTimers() {
  if (APP_STATE.overlayHideTimer != null) {
    clearTimeout(APP_STATE.overlayHideTimer);
    APP_STATE.overlayHideTimer = null;
  }
  if (APP_STATE.statusHideTimer != null) {
    clearTimeout(APP_STATE.statusHideTimer);
    APP_STATE.statusHideTimer = null;
  }
}

function showIdentityOverlay(text) {
  const overlay = document.getElementById('dashboard-overlay');
  if (!overlay || !text) {
    return;
  }
  overlay.textContent = text;
  overlay.classList.remove('hidden');
}

function setTelemetryNotice(text, kind = 'info') {
  APP_STATE.telemetryNotice = text;
  APP_STATE.telemetryStatusKind = kind;

  const status = document.getElementById('dashboard-status');
  if (status) {
    status.textContent = text;
    status.className = kind === 'info'
      ? 'status-pill'
      : `status-pill status-pill--${kind}`;
  }

  const overlay = document.getElementById('dashboard-status-overlay');
  if (overlay) {
    clearTimeout(APP_STATE.statusHideTimer);
    overlay.textContent = text;
    overlay.className = kind === 'info'
      ? 'dashboard-status-overlay'
      : `dashboard-status-overlay dashboard-status-overlay--${kind}`;
    overlay.classList.remove('hidden');
    if (kind !== 'refreshing') {
      const hideDelay = kind === 'error' ? STATUS_ERROR_AUTO_HIDE_MS : STATUS_AUTO_HIDE_MS;
      APP_STATE.statusHideTimer = setTimeout(() => {
        overlay.classList.add('hidden');
        APP_STATE.statusHideTimer = null;
      }, hideDelay);
    }
  }
}

function clearTelemetryCache() {
  APP_STATE.telemetryCache.clear();
}

function replaceTelemetryCache(entries) {
  APP_STATE.telemetryCache = entries instanceof Map ? entries : new Map();
  enforceTelemetryCacheBounds();
}

function getActiveDashboard() {
  if (!APP_STATE.activeEnvironment) {
    return null;
  }
  return getActiveChartPage().page?.dashboard
    ?? APP_STATE.activeEnvironment.dashboards[APP_STATE.activeDashboardIndex]
    ?? null;
}

function buildTelemetrySourceCacheKey(environment, source) {
  return JSON.stringify({
    clientId: environment.clientId,
    storageAccount: environment.storageAccount,
    nodeId: source.nodeId,
    readingType: source.readingType,
  });
}

function collectEnvironmentTelemetrySources(environment) {
  const seenSources = new Set();
  const variables = [];

  for (const dashboard of environment?.dashboards ?? []) {
    for (const variable of dashboard?.variables ?? []) {
      const sourceKey = JSON.stringify([variable?.nodeId, variable?.readingType]);
      if (typeof variable?.nodeId !== 'string'
        || typeof variable?.readingType !== 'string'
        || seenSources.has(sourceKey)) {
        continue;
      }
      seenSources.add(sourceKey);
      variables.push({
        nodeId: variable.nodeId,
        readingType: variable.readingType,
      });
    }
  }

  return variables;
}

function getLargestDashboardTimeRange(environment, runtime, nowMs = Date.now()) {
  let selectedRange = null;
  let largestDurationMs = -Infinity;

  for (const dashboard of environment?.dashboards ?? []) {
    const { startMs, endMs } = runtime.getDashboardTimeRangeBounds(dashboard.timeRange, nowMs);
    if (!Number.isFinite(startMs) || !Number.isFinite(endMs) || endMs <= startMs) {
      continue;
    }
    const durationMs = endMs - startMs;
    if (durationMs > largestDurationMs) {
      largestDurationMs = durationMs;
      selectedRange = { startMs, endMs };
    }
  }

  return selectedRange ?? { startMs: nowMs, endMs: nowMs };
}

function computeIncrementalRefreshStartMs(environment, variables, fullStartMs) {
  let incrementalStartMs = null;

  for (const variable of variables) {
    const cached = APP_STATE.telemetryCache.get(buildTelemetrySourceCacheKey(environment, variable));
    if (!cached
      || !Number.isFinite(cached.coverageStartMs)
      || cached.coverageStartMs > fullStartMs
      || !Number.isFinite(cached.coverageEndMs)
      || cached.coverageEndMs < fullStartMs) {
      return null;
    }
    if (incrementalStartMs == null || cached.coverageEndMs < incrementalStartMs) {
      incrementalStartMs = cached.coverageEndMs;
    }
  }

  return Number.isFinite(incrementalStartMs) ? incrementalStartMs : null;
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
    if (cached) {
      cached.lastAccessedAtMs = nowMs;
    }
    result[variable.name] = cached
      ? filterTelemetryPointsToRange(cached.points, startMs, endMs)
      : [];
  }

  return result;
}

function convertCachedPointsToRuntimeTimeSeries(points) {
  return Array.isArray(points)
    ? points
      .filter((point) => typeof point === 'object' && point !== null)
      .map((point) => ({
        timestamp: Number(point.timestampMs),
        value: Number(point.value),
      }))
      .filter((point) => Number.isFinite(point.timestamp) && Number.isFinite(point.value))
    : [];
}

function hasUsableCachedDashboardData(runtime, environment, dashboard, nowMs = Date.now()) {
  const cachedVariableData = buildCachedVariableData(runtime, environment, dashboard, nowMs);
  return Object.values(cachedVariableData).some((points) => Array.isArray(points) && points.length > 0);
}

function normalizeTelemetryCacheRecord(record) {
  if (typeof record !== 'object' || record === null) {
    return null;
  }

  const coverageStartMs = Number(record.coverageStartMs);
  const coverageEndMs = Number(record.coverageEndMs);
  const refreshedAtMs = Number(record.refreshedAtMs);
  const lastAccessedAtMs = Number(record.lastAccessedAtMs);
  return {
    points: normalizeTelemetryPoints(record.points),
    coverageStartMs: Number.isFinite(coverageStartMs) ? coverageStartMs : null,
    coverageEndMs: Number.isFinite(coverageEndMs) ? coverageEndMs : null,
    refreshedAtMs: Number.isFinite(refreshedAtMs) ? refreshedAtMs : null,
    lastAccessedAtMs: Number.isFinite(lastAccessedAtMs)
      ? lastAccessedAtMs
      : (Number.isFinite(refreshedAtMs) ? refreshedAtMs : null),
  };
}

function serializeTelemetryCache() {
  return JSON.stringify({
    version: 1,
    entries: Array.from(APP_STATE.telemetryCache.entries()).map(([key, value]) => ({
      key,
      ...value,
    })),
  });
}

function parseTelemetryCacheJson(text) {
  let data;
  try {
    data = JSON.parse(text);
  } catch {
    throw new Error('Stored telemetry cache is not valid JSON.');
  }

  if (typeof data !== 'object' || data === null || Array.isArray(data)) {
    throw new Error('Stored telemetry cache must be a JSON object.');
  }
  if (data.version !== 1) {
    throw new Error(`Unsupported telemetry cache version: ${data.version ?? 'missing'}.`);
  }
  if (!Array.isArray(data.entries)) {
    throw new Error('Stored telemetry cache entries must be an array.');
  }

  const cache = new Map();
  for (const entry of data.entries) {
    if (typeof entry !== 'object' || entry === null || Array.isArray(entry) || typeof entry.key !== 'string') {
      throw new Error('Stored telemetry cache contains an invalid entry.');
    }
    cache.set(entry.key, normalizeTelemetryCacheRecord(entry));
  }
  return cache;
}

function enforceTelemetryCacheBounds(options = {}) {
  const nowMs = Number.isFinite(options.nowMs) ? options.nowMs : Date.now();
  const minimumSeries = Number.isFinite(options.minimumSeries) && options.minimumSeries > 0
    ? Math.floor(options.minimumSeries)
    : 0;
  const protectedKeys = options.protectedKeys instanceof Set ? options.protectedKeys : new Set();
  const maxSeries = Math.max(TELEMETRY_CACHE_MAX_SERIES, minimumSeries);

  for (const entry of APP_STATE.telemetryCache.values()) {
    entry.points = normalizeTelemetryPoints(entry.points)
      .slice(-TELEMETRY_CACHE_MAX_POINTS_PER_SERIES);
    if (!Number.isFinite(entry.lastAccessedAtMs)) {
      entry.lastAccessedAtMs = Number.isFinite(entry.refreshedAtMs) ? entry.refreshedAtMs : nowMs;
    }
  }

  if (APP_STATE.telemetryCache.size <= maxSeries) {
    return;
  }

  const evictionOrder = Array.from(APP_STATE.telemetryCache.entries())
    .sort(([leftKey, left], [rightKey, right]) => {
      const leftProtected = protectedKeys.has(leftKey) ? 1 : 0;
      const rightProtected = protectedKeys.has(rightKey) ? 1 : 0;
      if (leftProtected !== rightProtected) {
        return leftProtected - rightProtected;
      }
      const leftAccess = Number.isFinite(left.lastAccessedAtMs) ? left.lastAccessedAtMs : -Infinity;
      const rightAccess = Number.isFinite(right.lastAccessedAtMs) ? right.lastAccessedAtMs : -Infinity;
      if (leftAccess !== rightAccess) {
        return leftAccess - rightAccess;
      }
      const leftRefresh = Number.isFinite(left.refreshedAtMs) ? left.refreshedAtMs : -Infinity;
      const rightRefresh = Number.isFinite(right.refreshedAtMs) ? right.refreshedAtMs : -Infinity;
      return leftRefresh - rightRefresh;
    });

  for (let index = 0; APP_STATE.telemetryCache.size > maxSeries && index < evictionOrder.length; index += 1) {
    const [cacheKey] = evictionOrder[index];
    APP_STATE.telemetryCache.delete(cacheKey);
  }
}

function buildEnvironmentRefreshRequest(environment, runtime, nowMs = Date.now()) {
  const { startMs: fullStartMs, endMs: fullEndMs } = getLargestDashboardTimeRange(environment, runtime, nowMs);
  const variables = collectEnvironmentTelemetrySources(environment);
  const incrementalStartMs = computeIncrementalRefreshStartMs(environment, variables, fullStartMs);

  return {
    clientId: environment.clientId,
    tenantId: environment.tenantId,
    storageAccount: environment.storageAccount,
    startMs: Number.isFinite(incrementalStartMs) ? incrementalStartMs : fullStartMs,
    endMs: fullEndMs,
    fullStartMs,
    fullEndMs,
    incremental: Number.isFinite(incrementalStartMs),
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

function mergeTelemetryPoints(existingPoints, incomingPoints) {
  const merged = new Map();
  for (const point of normalizeTelemetryPoints(existingPoints)) {
    merged.set(point.timestampMs, point);
  }
  for (const point of normalizeTelemetryPoints(incomingPoints)) {
    merged.set(point.timestampMs, point);
  }
  return Array.from(merged.values()).sort((left, right) => left.timestampMs - right.timestampMs);
}

function cacheTelemetryRefreshResponse(environment, request, response) {
  const complete = response?.complete !== false;
  let cachedSeriesCount = 0;
  const refreshedCacheKeys = new Set();
  const requestedCacheKeys = new Set();
  const responseSeriesByKey = new Map();
  const refreshedAtMs = Number.isFinite(response?.refreshedAtMs) ? Number(response.refreshedAtMs) : Date.now();
  for (const variable of request?.variables ?? []) {
    if (typeof variable !== 'object' || variable === null
    || typeof variable.nodeId !== 'string'
      || typeof variable.readingType !== 'string') {
      continue;
    }
    requestedCacheKeys.add(buildTelemetrySourceCacheKey(environment, variable));
  }
  for (const series of response?.series ?? []) {
    if (typeof series !== 'object' || series === null
      || typeof series.nodeId !== 'string'
      || typeof series.readingType !== 'string') {
      continue;
    }

    const key = buildTelemetrySourceCacheKey(environment, {
      nodeId: series.nodeId,
      readingType: series.readingType,
    });
    if (!requestedCacheKeys.has(key)) {
      continue;
    }
    responseSeriesByKey.set(key, normalizeTelemetryPoints(series.points));
  }
  for (const variable of request?.variables ?? []) {
    if (typeof variable !== 'object' || variable === null
      || typeof variable.nodeId !== 'string'
      || typeof variable.readingType !== 'string') {
      continue;
    }
    const key = buildTelemetrySourceCacheKey(environment, variable);
    const existing = APP_STATE.telemetryCache.get(key);
    const responseIncludedSeries = responseSeriesByKey.has(key);
    const incomingPoints = responseIncludedSeries ? responseSeriesByKey.get(key) : [];
    const preserveExistingPoints = request.incremental !== true
      && incomingPoints.length === 0
      && existing
      && existing.points.length > 0;
    const mergedPoints = preserveExistingPoints
      ? existing.points
      : (request.incremental === true && existing
      ? mergeTelemetryPoints(existing.points, incomingPoints)
      : incomingPoints);
    const coverageStartMs = complete
      ? (request.incremental === true && existing && Number.isFinite(existing.coverageStartMs)
        ? existing.coverageStartMs
        : (Number.isFinite(request.fullStartMs) ? request.fullStartMs : request.startMs))
      : (request.incremental === true && existing ? existing.coverageStartMs : null);
    const coverageEndMs = complete
      ? request.endMs
      : (request.incremental === true && existing ? existing.coverageEndMs : null);
    APP_STATE.telemetryCache.set(key, {
      points: mergedPoints,
      coverageStartMs: Number.isFinite(coverageStartMs) ? coverageStartMs : null,
      coverageEndMs: Number.isFinite(coverageEndMs) ? coverageEndMs : null,
      refreshedAtMs,
      lastAccessedAtMs: Date.now(),
    });
    refreshedCacheKeys.add(key);
    if (responseIncludedSeries || mergedPoints.length > 0) {
      cachedSeriesCount += 1;
    }
  }
  enforceTelemetryCacheBounds({
    minimumSeries: refreshedCacheKeys.size,
    protectedKeys: refreshedCacheKeys,
  });
  return cachedSeriesCount;
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

function validateFetchDashboardVariableDataResponse(response) {
  if (typeof response !== 'object' || response === null || Array.isArray(response)) {
    throw new Error('Telemetry refresh returned an invalid response payload.');
  }
  if (!Array.isArray(response.series)) {
    throw new Error('Telemetry refresh returned an invalid response payload.');
  }
  if ('complete' in response && typeof response.complete !== 'boolean') {
    throw new Error('Telemetry refresh returned an invalid response payload.');
  }
  return response;
}

async function loadStoredTelemetryCache(deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    return new Map();
  }
  const raw = await invokeFn('get_telemetry_cache_json');
  if (!raw) {
    return new Map();
  }
  try {
    return parseTelemetryCacheJson(raw);
  } catch {
    await clearStoredTelemetryCache(deps);
    return new Map();
  }
}

async function persistTelemetryCache(deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    return;
  }
  await invokeFn('save_telemetry_cache_json', { json: serializeTelemetryCache() });
}

async function clearStoredTelemetryCache(deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    return;
  }
  await invokeFn('clear_telemetry_cache_json');
}

function createCachedMetricEvaluator(runtime, environment, dashboard, nowMs = Date.now()) {
  const cachedVariableData = buildCachedVariableData(runtime, environment, dashboard, nowMs);
  return async (metric, variables, timeRange) => runtime.evaluateMetricTimeSeries(metric, variables, timeRange, {
    fetchVariableDataFn: async (usedVariables) => {
      const result = Object.create(null);
      for (const variable of usedVariables) {
        result[variable.name] = cachedVariableData[variable.name]
          ? convertCachedPointsToRuntimeTimeSeries(cachedVariableData[variable.name])
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
  if (!pageHost) {
    return;
  }
  const { page } = getActiveChartPage(environment);

  destroyDashboardCharts();
  pageHost.innerHTML = renderDashboardFrame(
    runtime,
    environment,
    APP_STATE.activeDashboardIndex,
    APP_STATE.activeChartIndex,
  );
  if (!page) {
    return;
  }
  showIdentityOverlay(buildChartOverlayText(
    environment,
    APP_STATE.activeDashboardIndex,
    APP_STATE.activeChartIndex,
  ));
  setTelemetryNotice(APP_STATE.telemetryNotice, APP_STATE.telemetryStatusKind);

  const dashboard = page.dashboard;
  const renderDashboard = buildChartRenderDashboard(page);
  if (!renderDashboard || !renderDashboard.charts.length) {
    return;
  }
  await runtime.renderMetricCharts(renderDashboard, {
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
  if (reason === 'background' && APP_STATE.refreshInFlightPromise) {
    return APP_STATE.refreshInFlightPromise;
  }

  const runtime = APP_STATE.runtime;
  const environment = APP_STATE.activeEnvironment;
  const dashboard = getActiveDashboard();
  if (!runtime || !environment || !dashboard) {
    return;
  }

  const nowFn = deps.nowFn || Date.now;
  const refreshRequest = buildEnvironmentRefreshRequest(environment, runtime, nowFn());
  if (refreshRequest.variables.length === 0) {
    if (reason !== 'background') {
      setTelemetryNotice('No telemetry variables are defined for this environment.', 'info');
    }
    return;
  }
  const refreshGeneration = ++APP_STATE.refreshGeneration;
  const dashboardIndexAtStart = APP_STATE.activeDashboardIndex;
  const chartIndexAtStart = APP_STATE.activeChartIndex;
  if (reason !== 'background') {
    const refreshingMessage = reason === 'manual'
      ? 'Manual refresh in progress…'
      : reason === 'switch'
        ? 'Loading live data for this chart…'
        : 'Refreshing live chart data…';
    setTelemetryNotice(refreshingMessage, 'refreshing');
  }

  const refreshPromise = (async () => {
    try {
      const response = validateFetchDashboardVariableDataResponse(
        await fetchDashboardVariableData(refreshRequest, deps),
      );
      if (refreshGeneration !== APP_STATE.refreshGeneration
        || environment !== APP_STATE.activeEnvironment
        || dashboardIndexAtStart !== APP_STATE.activeDashboardIndex
        || chartIndexAtStart !== APP_STATE.activeChartIndex) {
        return;
      }
      const cachedSeriesCount = cacheTelemetryRefreshResponse(environment, refreshRequest, response);
      if (cachedSeriesCount === 0 && !hasUsableCachedDashboardData(runtime, environment, dashboard, nowFn())) {
        throw new Error('Telemetry refresh returned no usable series.');
      }
      await persistTelemetryCache(deps);
      const refreshedAtMs = Number.isFinite(response?.refreshedAtMs)
        ? Number(response.refreshedAtMs)
        : nowFn();
      if (reason !== 'background') {
        if (response.complete === false) {
          setTelemetryNotice(`Partial live data refreshed at ${new Date(refreshedAtMs).toLocaleTimeString()}.`, 'live');
        } else if (cachedSeriesCount === 0) {
          setTelemetryNotice(`Live data checked at ${new Date(refreshedAtMs).toLocaleTimeString()}.`, 'live');
        } else {
          setTelemetryNotice(`Live data refreshed at ${new Date(refreshedAtMs).toLocaleTimeString()}.`, 'live');
        }
      }
      await renderActiveDashboard(deps);
    } catch (error) {
      if (refreshGeneration !== APP_STATE.refreshGeneration
        || environment !== APP_STATE.activeEnvironment
        || dashboardIndexAtStart !== APP_STATE.activeDashboardIndex
        || chartIndexAtStart !== APP_STATE.activeChartIndex) {
        return;
      }
      const message = describeError(error);
      if (hasUsableCachedDashboardData(runtime, environment, dashboard, nowFn())) {
        setTelemetryNotice(`Showing cached data. Live refresh unavailable: ${message}`, 'error');
      } else {
        setTelemetryNotice(`Live refresh unavailable: ${message}`, 'error');
      }
    } finally {
      if (APP_STATE.refreshInFlightPromise === refreshPromise) {
        APP_STATE.refreshInFlightPromise = null;
      }
    }
  })();
  APP_STATE.refreshInFlightPromise = refreshPromise;
  return refreshPromise;
}

async function showDashboardMode(deps = APP_STATE.dependencies) {
  APP_STATE.setupStatusMessage = '';
  APP_STATE.deviceCodeSession = null;
  document.getElementById('setup-screen')?.classList.add('hidden');
  document.getElementById('dashboard-screen')?.classList.remove('hidden');
  await renderActiveDashboard(deps);
  startBackgroundRefreshLoop(deps);
  await triggerDashboardRefresh('initial', deps);
}

function renderSetupScreen() {
  const status = document.getElementById('setup-status');
  const importButton = document.getElementById('import-button');
  const authButton = document.getElementById('setup-auth-button');
  const deviceCodePanel = document.getElementById('device-code-panel');
  const deviceCodeValue = document.getElementById('device-code-value');
  const deviceCodeLink = document.getElementById('device-code-link');
  const setupContext = document.getElementById('setup-context');
  if (status) {
    status.textContent = APP_STATE.setupStatusMessage;
  }
  if (setupContext) {
    setupContext.textContent = APP_STATE.activeEnvironment
      ? `Environment: ${APP_STATE.activeEnvironment.name}`
      : 'Import an environment JSON exported from the SPA to start kiosk dashboard mode.';
  }
  if (importButton) {
    importButton.textContent = APP_STATE.activeEnvironment
      ? 'Import Replacement Environment JSON'
      : 'Import Environment JSON';
  }
  if (authButton) {
    const metadata = validateSetupLoginMetadata(APP_STATE.activeEnvironment);
    const showButton = APP_STATE.activeEnvironment && !APP_STATE.deviceCodeSession;
    authButton.classList.toggle('hidden', !showButton);
    authButton.disabled = !metadata.valid;
    authButton.textContent = APP_STATE.identitySummary
      ? 'Renew Kiosk Certificate'
      : 'Start Device Code Sign-In';
  }
  if (deviceCodePanel) {
    const activeSession = APP_STATE.deviceCodeSession;
    deviceCodePanel.classList.toggle('hidden', !activeSession);
    if (deviceCodeValue) {
      deviceCodeValue.textContent = activeSession?.userCode || '';
    }
    if (deviceCodeLink) {
      const href = activeSession?.verificationUriComplete || activeSession?.verificationUri || '#';
      deviceCodeLink.textContent = activeSession?.verificationUri || '';
      deviceCodeLink.href = href;
    }
  }
}

function showSetupMode(message, options = {}) {
  stopBackgroundRefreshLoop();
  destroyDashboardCharts();
  clearOverlayTimers();
  if (options.clearEnvironment === true) {
    APP_STATE.activeEnvironment = null;
    APP_STATE.activeDashboardIndex = 0;
    APP_STATE.activeChartIndex = 0;
    APP_STATE.identitySummary = null;
  }
  APP_STATE.deviceCodeSession = null;
  APP_STATE.setupStatusMessage = message;
  APP_STATE.refreshGeneration = 0;
  APP_STATE.refreshInFlightPromise = null;
  document.getElementById('dashboard-screen')?.classList.add('hidden');
  document.getElementById('setup-screen')?.classList.remove('hidden');
  document.getElementById('dashboard-overlay')?.classList.add('hidden');
  document.getElementById('dashboard-status-overlay')?.classList.add('hidden');
  renderSetupScreen();
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

async function loadKioskIdentitySummary(deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    return null;
  }
  return invokeFn('get_kiosk_identity_summary');
}

async function clearStoredKioskIdentity(deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  await invokeFn('clear_kiosk_identity_local_state');
}

async function startDeviceCodeSignIn(request, deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  return invokeFn('start_device_code_sign_in', { request });
}

async function pollDeviceCodeSignIn(sessionId, deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  return invokeFn('poll_device_code_sign_in', { request: { sessionId } });
}

async function completeKioskSetup(sessionId, deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  const environment = APP_STATE.activeEnvironment;
  return invokeFn('complete_kiosk_setup', {
    request: {
      sessionId,
      sharedAppClientId: environment.clientId,
      tenantId: environment.tenantId,
      loginEndpoint: environment.loginEndpoint,
      setupClientId: environment.kioskSetupClientId,
    },
  });
}

async function signInKioskApplication(deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  return invokeFn('sign_in_kiosk_application');
}

async function renewKioskCertificate(sessionId, deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  return invokeFn('renew_kiosk_certificate', { request: { sessionId } });
}

async function resetKioskAppState(sessionId, deps = {}) {
  const invokeFn = deps.invoke || invoke;
  if (typeof invokeFn !== 'function') {
    throw new Error('Tauri invoke bridge is unavailable.');
  }
  return invokeFn('reset_kiosk_app_state', { request: { sessionId: sessionId ?? null } });
}

async function beginDeviceCodeFlow(purpose, deps = {}) {
  const environment = APP_STATE.activeEnvironment;
  if (!environment) {
    throw new Error('Import an environment before starting kiosk setup.');
  }
  const metadata = validateSetupLoginMetadata(environment);
  if (!metadata.valid) {
    throw new Error(metadata.error);
  }
  APP_STATE.setupStatusMessage = purpose === 'renew'
    ? 'Starting operator device-code sign-in for certificate renewal…'
    : purpose === 'reset'
      ? 'Starting operator device-code sign-in for reset cleanup…'
      : 'Starting operator device-code sign-in for kiosk setup…';
  showSetupMode(APP_STATE.setupStatusMessage);
  APP_STATE.deviceCodeSession = await startDeviceCodeSignIn({
    purpose,
    tenantId: environment.tenantId,
    loginEndpoint: metadata.loginEndpoint,
    setupClientId: metadata.kioskSetupClientId,
  }, deps);
  renderSetupScreen();
  await pollUntilDeviceCodeComplete(purpose, deps);
}

async function pollUntilDeviceCodeComplete(purpose, deps = {}) {
  const setTimeoutFn = deps.setTimeoutFn || globalThis.setTimeout;
  while (APP_STATE.deviceCodeSession) {
    const sessionId = APP_STATE.deviceCodeSession.sessionId;
    const response = await pollDeviceCodeSignIn(sessionId, deps);
    if (response.status === 'pending') {
      APP_STATE.setupStatusMessage = response.message || 'Waiting for operator sign-in to complete…';
      renderSetupScreen();
      const pollDelayMs = Number.isFinite(response.pollIntervalSeconds) && response.pollIntervalSeconds > 0
        ? response.pollIntervalSeconds * 1000
        : DEVICE_CODE_POLL_FALLBACK_MS;
      await new Promise((resolve) => {
        setTimeoutFn(resolve, pollDelayMs);
      });
      continue;
    }
    APP_STATE.setupStatusMessage = response.message || '';
    APP_STATE.deviceCodeSession = null;
    renderSetupScreen();
    if (response.status !== 'complete') {
      throw new Error(response.message || 'Operator sign-in failed.');
    }
    if (purpose === 'renew') {
      const result = await renewKioskCertificate(sessionId, deps);
      APP_STATE.identitySummary = result.summary;
      setTelemetryNotice(result.message, result.cleanupStatus === 'removed_previous' ? 'info' : 'error');
      await signInAndShowDashboard(deps);
      return;
    }
    if (purpose === 'reset') {
      const result = await resetKioskAppState(sessionId, deps);
      APP_STATE.identitySummary = null;
      clearTelemetryCache();
      showSetupMode(result.message, { clearEnvironment: true });
      return;
    }
    const result = await completeKioskSetup(sessionId, deps);
    APP_STATE.identitySummary = result.summary;
    setTelemetryNotice(result.message, 'info');
    await signInAndShowDashboard(deps);
    return;
  }
}

async function signInAndShowDashboard(deps = {}) {
  const result = await signInKioskApplication(deps);
  APP_STATE.identitySummary = result.summary;
  setTelemetryNotice(result.message, result.summary.renewalRequired ? 'info' : 'live');
  await showDashboardMode(deps);
}

function moveDashboard(delta, deps = APP_STATE.dependencies) {
  const { pages, pageIndex } = getActiveChartPage();
  if (!pages.length || pageIndex < 0) {
    return;
  }
  const nextPage = pages[pageIndex + delta];
  if (!nextPage) {
    return;
  }
  APP_STATE.activeDashboardIndex = nextPage.dashboardIndex;
  APP_STATE.activeChartIndex = nextPage.chartIndex;
  const runtime = APP_STATE.runtime;
  const environment = APP_STATE.activeEnvironment;
  renderActiveDashboard(deps)
    .then(() => {
      if (runtime && environment
        && !hasUsableCachedDashboardData(runtime, environment, nextPage.dashboard, (deps.nowFn || Date.now)())) {
        return triggerDashboardRefresh('switch', deps);
      }
      return null;
    })
    .catch((error) => showSetupMode(describeError(error)));
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
  const renew = document.getElementById('operator-renew');
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
  renew?.addEventListener('click', async () => {
    hideOperatorPanel();
    document.getElementById('dashboard-screen')?.classList.add('hidden');
    document.getElementById('setup-screen')?.classList.remove('hidden');
    try {
      await beginDeviceCodeFlow('renew', deps);
    } catch (error) {
      showSetupMode(`Certificate renewal failed: ${describeError(error)}`);
    }
  });
  reset?.addEventListener('click', async () => {
    hideOperatorPanel();
    document.getElementById('dashboard-screen')?.classList.add('hidden');
    document.getElementById('setup-screen')?.classList.remove('hidden');
    try {
      await beginDeviceCodeFlow('reset', deps);
    } catch (error) {
      const result = await resetKioskAppState(null, deps);
      APP_STATE.identitySummary = null;
      clearTelemetryCache();
      showSetupMode(`${result.message} Reset fallback detail: ${describeError(error)}`, { clearEnvironment: true });
    }
  });
  close?.addEventListener('click', hideOperatorPanel);
}

async function importEnvironmentFromText(text, deps = {}) {
  const runtime = deps.runtime || APP_STATE.runtime;
  if (!runtime) {
    throw new Error('Shared dashboard runtime is not loaded.');
  }
  const environment = validateImportedEnvironmentJson(text, runtime, deps);
  if (APP_STATE.identitySummary) {
    await clearStoredKioskIdentity(deps);
    APP_STATE.identitySummary = null;
  }
  await persistEnvironment(environment, deps);
  clearTelemetryCache();
  await clearStoredTelemetryCache(deps);
  setTelemetryNotice('Waiting for live telemetry refresh.', 'info');
  APP_STATE.activeEnvironment = environment;
  APP_STATE.activeDashboardIndex = 0;
  APP_STATE.activeChartIndex = 0;
  const metadata = validateSetupLoginMetadata(environment);
  showSetupMode(
    metadata.valid
      ? 'Environment imported. Complete operator sign-in to provision the kiosk certificate.'
      : metadata.error,
  );
}

async function initKioskApp(deps = {}) {
  const runtime = await loadSharedDashboardRuntime(deps);
  APP_STATE.runtime = runtime;
  APP_STATE.dependencies = deps;
  replaceTelemetryCache(await loadStoredTelemetryCache(deps));

  const fileInput = document.getElementById('import-file');
  const importButton = document.getElementById('import-button');
  const setupAuthButton = document.getElementById('setup-auth-button');
  const pageHost = document.getElementById('dashboard-page-host');

  if (!fileInput || !importButton || !pageHost || !setupAuthButton) {
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
      showSetupMode(`Import failed: ${describeError(error)}`);
    } finally {
      fileInput.value = '';
    }
  });
  setupAuthButton.addEventListener('click', async () => {
    const purpose = APP_STATE.identitySummary ? 'renew' : 'initial';
    try {
      await beginDeviceCodeFlow(purpose, deps);
    } catch (error) {
      showSetupMode(`${purpose === 'renew' ? 'Certificate renewal' : 'Kiosk setup'} failed: ${describeError(error)}`);
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
    APP_STATE.activeEnvironment = storedEnvironment;
    APP_STATE.activeDashboardIndex = 0;
    APP_STATE.activeChartIndex = 0;
    APP_STATE.identitySummary = await loadKioskIdentitySummary(deps);
    if (APP_STATE.identitySummary) {
      try {
        await signInAndShowDashboard(deps);
        return;
      } catch (error) {
        const message = describeError(error);
        const initialDashboard = buildChartPages(storedEnvironment)[0]?.dashboard
          ?? storedEnvironment.dashboards[0];
        if (initialDashboard && hasUsableCachedDashboardData(runtime, storedEnvironment, initialDashboard)) {
          setTelemetryNotice(`Showing cached data while reconnecting. Application sign-in failed: ${message}`, 'error');
          await showDashboardMode(deps);
        } else {
          showSetupMode(`Application sign-in failed: ${message}`);
        }
        return;
      }
    }
    const metadata = validateSetupLoginMetadata(storedEnvironment);
    showSetupMode(
      metadata.valid
        ? 'Environment imported. Complete operator sign-in to provision the kiosk certificate.'
        : metadata.error,
    );
  } else {
    showSetupMode('No environment imported yet.');
  }
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    APP_STATE,
    BACKGROUND_REFRESH_INTERVAL_MS,
    beginDeviceCodeFlow,
    buildChartPages,
    buildEnvironmentRefreshRequest,
    createDefaultSensorDataPreferences,
    createCachedMetricEvaluator,
    convertCachedPointsToRuntimeTimeSeries,
    describeError,
    buildCachedVariableData,
    cacheTelemetryRefreshResponse,
    clearStoredTelemetryCache,
    collectEnvironmentTelemetrySources,
    fetchDashboardVariableData,
    hasUsableCachedDashboardData,
    importEnvironmentFromText,
    initKioskApp,
    interpretDashboardGesture,
    loadStoredTelemetryCache,
    loadSharedDashboardRuntime,
    enforceTelemetryCacheBounds,
    parseTelemetryCacheJson,
    pollUntilDeviceCodeComplete,
    persistTelemetryCache,
    replaceTelemetryCache,
    renderActiveDashboard,
    renderDashboardFrame,
    serializeTelemetryCache,
    showSetupMode,
    setTelemetryNotice,
    startBackgroundRefreshLoop,
    triggerDashboardRefresh,
    validateFetchDashboardVariableDataResponse,
    validateEnvironmentFields,
    validateImportedEnvironmentJson,
    validateImportedSensorDataPreferences,
    validateSetupLoginMetadata,
  };
}

document.addEventListener('DOMContentLoaded', () => {
  initKioskApp().catch((error) => {
    showSetupMode(`Kiosk startup failed: ${describeError(error)}`);
  });
});
