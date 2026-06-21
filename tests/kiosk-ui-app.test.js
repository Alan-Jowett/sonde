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
  }), 0, 'Read-only');

  assert.match(html, /dashboard-page--read-only/);
  assert.doesNotMatch(html, /Add Chart/);
  assert.doesNotMatch(html, /Add Variable/);
  assert.doesNotMatch(html, /Rename/);
  assert.doesNotMatch(html, /Edit/);
  assert.doesNotMatch(html, /Delete/);
  assert.doesNotMatch(html, /id="dashboard-time-range"/);
  assert.doesNotMatch(html, /type="datetime-local"/);
  assert.match(html, /Last 24 Hours/);
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
