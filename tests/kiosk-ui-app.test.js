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
