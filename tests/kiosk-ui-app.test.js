// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');

function makeElement() {
  return {
    innerHTML: '',
    textContent: '',
    value: '',
    files: [],
    src: '',
    classList: { add() {}, remove() {}, toggle() {} },
    addEventListener() {},
    appendChild() {},
    click() {},
  };
}

global.window = {
  prompt: () => null,
};
global.document = {
  addEventListener() {},
  createElement() { return makeElement(); },
  getElementById() { return makeElement(); },
  querySelector() { return makeElement(); },
  head: { appendChild() {} },
};
global.URL = {
  createObjectURL() { return 'blob:test'; },
  revokeObjectURL() {},
};
global.Blob = class Blob {
  constructor(parts) {
    this.parts = parts;
  }
};

const runtime = require(path.resolve(__dirname, '..', 'deploy', 'web-ui', 'dashboard-runtime.js'));
const kiosk = require(path.resolve(__dirname, '..', 'crates', 'sonde-kiosk-ui', 'src', 'main.js'));

function buildEnvironment(overrides = {}) {
  return runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    loginEndpoint: 'https://login.microsoftonline.com',
    kioskSetupClientId: '33333333-3333-3333-3333-333333333333',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [],
    ...overrides,
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });
}

test('validateImportedEnvironmentJson accepts SPA-style environment imports', () => {
  const environment = kiosk.validateImportedEnvironmentJson(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: {
      viewMode: 'graph',
      timeRange: '24h',
      selectedSeries: ['series-a'],
      seriesOverrides: {},
    },
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{
        name: 'Primary',
        metrics: [{ id: 'metric-1', displayName: 'Temperature', expression: 'TEMP / 1000', color: '#123456' }],
      }],
      timeRange: { preset: '24h', start: null, end: null },
    }],
    unknownFutureField: true,
  }), runtime);

  assert.equal(environment.name, 'prod');
  assert.equal(environment.dashboards.length, 1);
  assert.equal(environment.dashboards[0].charts[0].metrics[0].expression, 'TEMP / 1000');
  assert.deepEqual(environment.sensorData.selectedSeries, ['series-a']);
});

test('validateImportedEnvironmentJson prompts for a missing environment name', () => {
  const environment = kiosk.validateImportedEnvironmentJson(JSON.stringify({
    version: 1,
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    dashboards: [],
  }), runtime, {
    promptFn: () => 'Imported Name',
  });

  assert.equal(environment.name, 'Imported Name');
});

test('renderDashboardFrame keeps kiosk dashboards read-only', () => {
  const html = kiosk.renderDashboardFrame(runtime, runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{
        name: 'Primary',
        metrics: [{ id: 'metric-1', displayName: 'Temperature', expression: 'TEMP / 1000', color: '#123456' }],
      }],
      timeRange: { preset: '24h', start: null, end: null },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  }), 0);

  assert.match(html, /dashboard-page--read-only/);
  assert.doesNotMatch(html, /Add Chart/);
  assert.doesNotMatch(html, /Add Variable/);
  assert.doesNotMatch(html, /Rename/);
  assert.doesNotMatch(html, /Edit/);
  assert.doesNotMatch(html, /Delete/);
  assert.doesNotMatch(html, /id="dashboard-time-range"/);
  assert.doesNotMatch(html, /type="datetime-local"/);
  assert.match(html, /Last 24 Hours/);
  assert.doesNotMatch(html, /Read-only/);
});

test('buildDashboardRefreshRequest preserves the imported dashboard time range', () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  const request = kiosk.buildDashboardRefreshRequest(environment, environment.dashboards[0], runtime, 50_000);
  assert.equal(request.startMs, 1_000);
  assert.equal(request.endMs, 9_000);
  assert.deepEqual(request.variables, [{ nodeId: 'NODE_001', readingType: 'temp_mc' }]);
});

test('cacheTelemetryRefreshResponse reuses telemetry across dashboards sharing a source', () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP_A', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }, {
      name: 'Detail',
      variables: [{ name: 'TEMP_B', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  kiosk.APP_STATE.telemetryCache.clear();
  kiosk.cacheTelemetryRefreshResponse(environment, {
    startMs: 1_000,
    endMs: 9_000,
  }, {
    refreshedAtMs: 2_000,
    series: [{
      nodeId: 'NODE_001',
      readingType: 'temp_mc',
      points: [
        { timestampMs: 2_000, value: 21.5 },
        { timestampMs: 7_000, value: 22.0 },
      ],
    }],
  });

  const cached = kiosk.buildCachedVariableData(runtime, environment, environment.dashboards[1], 9_000);
  assert.deepEqual(cached.TEMP_B, [
    { timestampMs: 2_000, value: 21.5 },
    { timestampMs: 7_000, value: 22.0 },
  ]);
});

test('cacheTelemetryRefreshResponse ignores malformed series identifiers', () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  kiosk.APP_STATE.telemetryCache.clear();
  kiosk.cacheTelemetryRefreshResponse(environment, { startMs: 1_000, endMs: 9_000 }, {
    refreshedAtMs: 2_000,
    series: [{ nodeId: 'NODE_001', points: [{ timestampMs: 2_000, value: 1 }] }],
  });

  assert.equal(kiosk.APP_STATE.telemetryCache.size, 0);
});

test('cacheTelemetryRefreshResponse reports how many usable series were cached', () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  kiosk.APP_STATE.telemetryCache.clear();
  const cachedSeriesCount = kiosk.cacheTelemetryRefreshResponse(environment, { startMs: 1_000, endMs: 9_000 }, {
    refreshedAtMs: 2_000,
    series: [
      { nodeId: 'NODE_001', readingType: 'temp_mc', points: [{ timestampMs: 2_000, value: 1 }] },
      { nodeId: 'NODE_002', points: [{ timestampMs: 2_000, value: 1 }] },
    ],
  });

  assert.equal(cachedSeriesCount, 1);
});

test('buildDashboardRefreshRequest de-duplicates sources without delimiter collisions', () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables: [
        { name: 'A', nodeId: 'node\none', readingType: 'temp' },
        { name: 'B', nodeId: 'node', readingType: 'one\ntemp' },
      ],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  const request = kiosk.buildDashboardRefreshRequest(environment, environment.dashboards[0], runtime, 50_000);
  assert.deepEqual(request.variables, [
    { nodeId: 'node\none', readingType: 'temp' },
    { nodeId: 'node', readingType: 'one\ntemp' },
  ]);
});

test('setTelemetryNotice preserves muted dashboard status styling for info notices', () => {
  const dashboardStatus = makeElement();
  const pageStatus = makeElement();
  global.document.getElementById = (id) => (id === 'dashboard-status' ? dashboardStatus : makeElement());
  global.document.querySelector = (selector) => (selector === '.dashboard-page-status' ? pageStatus : null);

  kiosk.setTelemetryNotice('Waiting for live telemetry refresh.', 'info');

  assert.equal(dashboardStatus.className, 'status-pill');
  assert.equal(pageStatus.className, 'dashboard-page-status text-muted');
  assert.equal(pageStatus.textContent, 'Waiting for live telemetry refresh.');
});

test('triggerDashboardRefresh caches live telemetry from the injected fetcher', async () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache.clear();

  await kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async (request) => {
      assert.equal(request.storageAccount, 'prodstorage');
      assert.deepEqual(request.variables, [{ nodeId: 'NODE_001', readingType: 'temp_mc' }]);
      return {
        refreshedAtMs: 9_000,
        series: [{
          nodeId: 'NODE_001',
          readingType: 'temp_mc',
          points: [{ timestampMs: 8_000, value: 20.25 }],
        }],
      };
    },
  });

  const cached = kiosk.buildCachedVariableData(runtime, environment, environment.dashboards[0], 9_000);
  assert.deepEqual(cached.TEMP, [{ timestampMs: 8_000, value: 20.25 }]);
  assert.match(kiosk.APP_STATE.telemetryNotice, /Live data refreshed/);
});

test('triggerDashboardRefresh rejects invalid telemetry payloads', async () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache.clear();

  await kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async () => null,
  });

  assert.match(kiosk.APP_STATE.telemetryNotice, /invalid response payload/i);
});

test('triggerDashboardRefresh rejects refreshes with no usable telemetry series', async () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache.clear();

  await kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async () => ({
      refreshedAtMs: 9_000,
      series: [{ nodeId: 'NODE_001', points: [{ timestampMs: 8_000, value: 20.25 }] }],
    }),
  });

  assert.match(kiosk.APP_STATE.telemetryNotice, /no usable series/i);
});

test('triggerDashboardRefresh does not persist telemetry after reset clears the active environment', async () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  let releaseRefresh;
  let persisted = false;
  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache.clear();
  kiosk.APP_STATE.refreshInFlightPromise = null;

  const refreshPromise = kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async () => {
      await new Promise((resolve) => {
        releaseRefresh = resolve;
      });
      return {
        refreshedAtMs: 9_000,
        series: [{
          nodeId: 'NODE_001',
          readingType: 'temp_mc',
          points: [{ timestampMs: 8_000, value: 20.25 }],
        }],
      };
    },
    invoke: async (command) => {
      if (command === 'save_telemetry_cache_json') {
        persisted = true;
      }
      return null;
    },
  });

  kiosk.APP_STATE.activeEnvironment = null;
  kiosk.APP_STATE.refreshGeneration += 1;
  releaseRefresh();
  await refreshPromise;

  assert.equal(persisted, false);
  assert.equal(kiosk.APP_STATE.telemetryCache.size, 0);
});

test('interpretDashboardGesture distinguishes refresh from horizontal navigation', () => {
  assert.equal(kiosk.interpretDashboardGesture(-120, 10), 'next');
  assert.equal(kiosk.interpretDashboardGesture(120, 10), 'previous');
  assert.equal(kiosk.interpretDashboardGesture(20, 120), 'refresh');
  assert.equal(kiosk.interpretDashboardGesture(10, 10), null);
});

test('startBackgroundRefreshLoop uses the kiosk refresh cadence', () => {
  let scheduledMs = null;
  kiosk.APP_STATE.refreshTimer = null;

  kiosk.startBackgroundRefreshLoop({
    setIntervalFn: (_fn, ms) => {
      scheduledMs = ms;
      return 42;
    },
    clearIntervalFn() {},
  });

  assert.equal(scheduledMs, kiosk.BACKGROUND_REFRESH_INTERVAL_MS);
  assert.equal(kiosk.APP_STATE.refreshTimer, 42);
});

test('background refresh skips starting a second request while one is in flight', async () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  let releaseRefresh;
  let callCount = 0;
  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache.clear();
  kiosk.APP_STATE.refreshInFlightPromise = null;

  const deps = {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async () => {
      callCount += 1;
      await new Promise((resolve) => {
        releaseRefresh = resolve;
      });
      return {
        refreshedAtMs: 9_000,
        series: [{ nodeId: 'NODE_001', readingType: 'temp_mc', points: [] }],
      };
    },
  };

  const firstRefresh = kiosk.triggerDashboardRefresh('background', deps);
  const secondRefresh = kiosk.triggerDashboardRefresh('background', deps);
  assert.equal(callCount, 1);

  releaseRefresh();
  await Promise.all([firstRefresh, secondRefresh]);
});

test('telemetry cache JSON round-trips through parse and serialize helpers', () => {
  kiosk.APP_STATE.telemetryCache = new Map([
    ['cache-key', {
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: 9_000,
    }],
  ]);

  const parsed = kiosk.parseTelemetryCacheJson(kiosk.serializeTelemetryCache());
  assert.deepEqual(Array.from(parsed.entries()), [[
    'cache-key',
    {
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: 9_000,
    },
  ]]);
});

test('loadStoredTelemetryCache clears corrupted persisted cache and recovers with an empty map', async () => {
  const invoked = [];
  const cache = await kiosk.loadStoredTelemetryCache({
    invoke: async (command) => {
      invoked.push(command);
      if (command === 'get_telemetry_cache_json') {
        return '{not valid json';
      }
      return null;
    },
  });

  assert.equal(cache.size, 0);
  assert.deepEqual(invoked, ['get_telemetry_cache_json', 'clear_telemetry_cache_json']);
});

test('validateSetupLoginMetadata accepts additive kiosk setup fields', () => {
  const result = kiosk.validateSetupLoginMetadata(buildEnvironment());

  assert.deepEqual(result, {
    valid: true,
    loginEndpoint: 'https://login.microsoftonline.com',
    kioskSetupClientId: '33333333-3333-3333-3333-333333333333',
  });
});

test('validateSetupLoginMetadata rejects incomplete kiosk setup metadata', () => {
  const result = kiosk.validateSetupLoginMetadata(buildEnvironment({ kioskSetupClientId: '' }));

  assert.equal(result.valid, false);
  assert.match(result.error, /missing kiosk setup login metadata/i);
});

test('validateSetupLoginMetadata rejects login endpoints with paths', () => {
  const result = kiosk.validateSetupLoginMetadata(
    buildEnvironment({ loginEndpoint: 'https://login.microsoftonline.com/common' }),
  );

  assert.equal(result.valid, false);
  assert.match(result.error, /authority url/i);
});

test('importEnvironmentFromText clears prior kiosk identity and stays in setup mode', async () => {
  const calls = [];
  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.identitySummary = { clientId: 'old-client' };
  kiosk.APP_STATE.activeEnvironment = null;

  await kiosk.importEnvironmentFromText(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    loginEndpoint: 'https://login.microsoftonline.com',
    kioskSetupClientId: '33333333-3333-3333-3333-333333333333',
    dashboards: [],
  }), {
    runtime,
    invoke: async (command, payload) => {
      calls.push({ command, payload });
      return null;
    },
  });

  assert.equal(kiosk.APP_STATE.identitySummary, null);
  assert.equal(kiosk.APP_STATE.activeEnvironment?.name, 'prod');
  assert.equal(kiosk.APP_STATE.activeDashboardIndex, 0);
  assert.equal(kiosk.APP_STATE.deviceCodeSession, null);
  assert.match(kiosk.APP_STATE.setupStatusMessage, /complete operator sign-in/i);
  assert.deepEqual(calls.map(({ command }) => command), [
    'clear_kiosk_identity_local_state',
    'save_environment_json',
    'clear_telemetry_cache_json',
  ]);
});

test('pollUntilDeviceCodeComplete reuses the completed session id for renewal', async () => {
  const calls = [];
  kiosk.APP_STATE.activeEnvironment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [],
      charts: [],
      timeRange: { preset: '24h', start: null, end: null },
    }],
  });
  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.deviceCodeSession = {
    sessionId: 'session-123',
    userCode: 'ABCDEF',
    verificationUri: 'https://microsoft.com/devicelogin',
    verificationUriComplete: 'https://microsoft.com/devicelogin?code=ABCDEF',
    pollIntervalSeconds: 0,
    message: 'Use code ABCDEF',
  };

  await kiosk.pollUntilDeviceCodeComplete('renew', {
    setTimeoutFn: (fn) => fn(),
    setIntervalFn: () => 1,
    clearIntervalFn() {},
    invoke: async (command, payload) => {
      calls.push({ command, payload });
      if (command === 'poll_device_code_sign_in') {
        return { status: 'complete', message: 'Signed in.' };
      }
      if (command === 'renew_kiosk_certificate') {
        return {
          message: 'Renewed.',
          cleanupStatus: 'removed_previous',
          summary: {
            tenantId: '22222222-2222-2222-2222-222222222222',
            sharedAppClientId: '11111111-1111-1111-1111-111111111111',
            registeredAt: '2026-06-21T00:00:00Z',
            expiresAt: '2026-12-21T00:00:00Z',
            renewalRequired: false,
          },
        };
      }
      if (command === 'sign_in_kiosk_application') {
        return {
          message: 'Application sign-in succeeded.',
          summary: {
            tenantId: '22222222-2222-2222-2222-222222222222',
            sharedAppClientId: '11111111-1111-1111-1111-111111111111',
            registeredAt: '2026-06-21T00:00:00Z',
            expiresAt: '2026-12-21T00:00:00Z',
            renewalRequired: false,
          },
        };
      }
      if (command === 'fetch_dashboard_variable_data') {
        return { refreshedAtMs: 9_000, series: [] };
      }
      throw new Error(`unexpected command ${command}`);
    },
  });

  assert.equal(kiosk.APP_STATE.deviceCodeSession, null);
  assert.deepEqual(
    calls.filter(({ command }) => command === 'renew_kiosk_certificate').map(({ payload }) => payload.request.sessionId),
    ['session-123'],
  );
});
