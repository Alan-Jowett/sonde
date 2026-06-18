// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { webcrypto } = require('node:crypto');

function makeStorage() {
  const store = new Map();
  return {
    getItem(key) {
      return store.has(key) ? store.get(key) : null;
    },
    setItem(key, value) {
      store.set(key, String(value));
    },
    removeItem(key) {
      store.delete(key);
    },
    key(index) {
      return [...store.keys()][index] || null;
    },
    get length() {
      return store.size;
    },
  };
}

function makeElement() {
  return {
    innerHTML: '',
    textContent: '',
    title: '',
    value: '',
    checked: false,
    disabled: false,
    dataset: {},
    style: {},
    focus() {},
    remove() {},
    appendChild() {},
    removeChild() {},
    insertAdjacentHTML() {},
    addEventListener() {},
    querySelector() { return null; },
    querySelectorAll() { return []; },
    classList: { toggle() {} },
  };
}

function resetStorages() {
  global.localStorage = makeStorage();
  global.sessionStorage = makeStorage();
}

global.window = {
  __SONDE_TEST__: true,
  opener: null,
  location: { origin: 'https://example.test', pathname: '/', hash: '' },
  prompt: () => null,
  confirm: () => false,
};
global.document = {
  addEventListener() {},
  getElementById() { return makeElement(); },
  querySelectorAll() { return []; },
  createElement() { return makeElement(); },
  body: makeElement(),
};
resetStorages();
global.crypto = webcrypto;
global.atob = (value) => Buffer.from(value, 'base64').toString('binary');
global.btoa = (value) => Buffer.from(value, 'binary').toString('base64');

const app = require(path.resolve(__dirname, '..', 'deploy', 'web-ui', 'app.js'));

test.beforeEach(() => {
  resetStorages();
  global.window.confirm = () => false;
  global.window.prompt = () => null;
  global.alert = () => {};
  app.clearSessionTelemetryCache();
  app.SENSOR_STATE.timeRange = '24h';
  app.SENSOR_STATE.viewMode = 'graph';
  app.SENSOR_STATE.selectedSeries = new Set();
  app.SENSOR_STATE.seriesInitialized = false;
  app.SENSOR_STATE.autoRefresh = false;
  app.SENSOR_STATE.exportStartMs = null;
  app.SENSOR_STATE.exportEndMs = null;
  app.SENSOR_STATE.exportFormat = 'jsonl';
  app.SENSOR_STATE.exportBusy = false;
  app.SENSOR_STATE.exportMessage = null;
  app.APP.account = null;
  app.APP.msalApp = null;
  app.APP_DASHBOARD_STATE.activeDashboardIndex = 0;
  app.APP_DASHBOARD_STATE.metricCharts = {};
  app.APP_DASHBOARD_STATE.unsavedEnvironment = null;
});

test('sensorDataFilter uses reverse-timestamp bounds for the requested range', () => {
  const max = BigInt('0xffffffffffffffff');
  const startMs = 1_000;
  const endMs = 2_000;
  const expectedStart = (max - BigInt(endMs)).toString(16).padStart(16, '0');
  const expectedEnd = (max - BigInt(startMs)).toString(16).padStart(16, '0');

  assert.equal(
    app.sensorDataFilter('n:abc', startMs, endMs),
    `PartitionKey eq 'n:abc' and RowKey ge '${expectedStart}' and RowKey le '${expectedEnd}~'`,
  );
});

test('sensorDataFilter escapes single quotes in partition keys', () => {
  const max = BigInt('0xffffffffffffffff');
  const startMs = 1_000;
  const endMs = 2_000;
  const expectedStart = (max - BigInt(endMs)).toString(16).padStart(16, '0');
  const expectedEnd = (max - BigInt(startMs)).toString(16).padStart(16, '0');

  assert.equal(
    app.sensorDataFilter("n:o'hare", startMs, endMs),
    `PartitionKey eq 'n:o''hare' and RowKey ge '${expectedStart}' and RowKey le '${expectedEnd}~'`,
  );
});

test('actualStateFilter uses reverse-timestamp bounds for the requested range', () => {
  const max = BigInt('0xffffffffffffffff');
  const startMs = 1_000;
  const endMs = 2_000;
  const expectedStart = (max - BigInt(endMs)).toString(16).padStart(16, '0');
  const expectedEnd = (max - BigInt(startMs)).toString(16).padStart(16, '0');

  assert.equal(
    app.actualStateFilter('n:abc', startMs, endMs),
    `PartitionKey eq 'n:abc' and RowKey ge '${expectedStart}' and RowKey le '${expectedEnd}~'`,
  );
});

test('buildSensorExportCsv emits header, sorts by timestamp, and escapes JSON payloads', () => {
  const csv = app.buildSensorExportCsv([
    {
      timestamp_ms: '2000',
      node_id: 'node-b',
      program_hash: 'bb',
      raw_payload: 'plain',
      decoded_readings: '{"temp_mc":25000}',
    },
    {
      timestamp_ms: '1000',
      node_id: 'node-a',
      program_hash: 'aa',
      raw_payload: 'raw,1',
      decoded_readings: '{"temp_mc":"9007199254740993"}',
    },
  ]);

  const lines = csv.split('\r\n');
  assert.equal(lines[0], 'timestamp_ms,node_id,program_hash,raw_payload,decoded_readings_json');
  assert.equal(lines[1], '1000,node-a,aa,"raw,1","{""temp_mc"":""9007199254740993""}"');
  assert.equal(lines[2], '2000,node-b,bb,plain,"{""temp_mc"":25000}"');
});

test('buildSensorExportJsonl emits parsed readings objects and null for empty readings', () => {
  const jsonl = app.buildSensorExportJsonl([
    {
      timestamp_ms: '2000',
      node_id: 'node-b',
      program_hash: 'bb',
      raw_payload: 'plain',
      decoded_readings: '',
    },
    {
      timestamp_ms: '1000',
      node_id: 'node-a',
      program_hash: 'aa',
      raw_payload: 'raw',
      decoded_readings: '{"temp_mc":"9007199254740993"}',
    },
  ]);

  const lines = jsonl.split('\n').map((line) => JSON.parse(line));
  assert.deepEqual(lines[0], {
    timestamp_ms: '1000',
    node_id: 'node-a',
    program_hash: 'aa',
    raw_payload: 'raw',
    decoded_readings: { temp_mc: '9007199254740993' },
  });
  assert.deepEqual(lines[1], {
    timestamp_ms: '2000',
    node_id: 'node-b',
    program_hash: 'bb',
    raw_payload: 'plain',
    decoded_readings: null,
  });
});

test('buildDeviceExportCsv emits header, sorts by timestamp, and leaves missing values empty', () => {
  const csv = app.buildDeviceExportCsv([
    {
      timestamp_ms: '2000',
      node_id: 'node-b',
      battery_mv: '3300',
      wake_rssi_dbm: '-61',
      firmware_version: '1.2.3',
      firmware_abi_version: '4',
      observed_schedule_interval_s: '300',
      observed_current_program_hash: 'bb',
      observed_assigned_program_hash: '',
    },
    {
      timestamp_ms: '1000',
      node_id: 'node-a',
      battery_mv: '',
      wake_rssi_dbm: '',
      firmware_version: '',
      firmware_abi_version: '',
      observed_schedule_interval_s: '',
      observed_current_program_hash: '',
      observed_assigned_program_hash: '',
    },
  ]);

  const lines = csv.split('\r\n');
  assert.equal(
    lines[0],
    'timestamp_ms,node_id,battery_mv,wake_rssi_dbm,firmware_version,firmware_abi_version,observed_schedule_interval_s,observed_current_program_hash,observed_assigned_program_hash',
  );
  assert.equal(lines[1], '1000,node-a,,,,,,,');
  assert.equal(lines[2], '2000,node-b,3300,-61,1.2.3,4,300,bb,');
});

test('buildDeviceExportJsonl emits null for missing optional values', () => {
  const jsonl = app.buildDeviceExportJsonl([
    {
      timestamp_ms: '2000',
      node_id: 'node-b',
      battery_mv: null,
      wake_rssi_dbm: null,
      firmware_version: null,
      firmware_abi_version: null,
      observed_schedule_interval_s: null,
      observed_current_program_hash: null,
      observed_assigned_program_hash: null,
    },
    {
      timestamp_ms: '1000',
      node_id: 'node-a',
      battery_mv: '3300',
      wake_rssi_dbm: '-58',
      firmware_version: '1.0.0',
      firmware_abi_version: '2',
      observed_schedule_interval_s: '120',
      observed_current_program_hash: 'aa',
      observed_assigned_program_hash: 'ab',
    },
  ]);

  const lines = jsonl.split('\n').map((line) => JSON.parse(line));
  assert.deepEqual(lines[0], {
    timestamp_ms: '1000',
    node_id: 'node-a',
    battery_mv: '3300',
    wake_rssi_dbm: '-58',
    firmware_version: '1.0.0',
    firmware_abi_version: '2',
    observed_schedule_interval_s: '120',
    observed_current_program_hash: 'aa',
    observed_assigned_program_hash: 'ab',
  });
  assert.deepEqual(lines[1], {
    timestamp_ms: '2000',
    node_id: 'node-b',
    battery_mv: null,
    wake_rssi_dbm: null,
    firmware_version: null,
    firmware_abi_version: null,
    observed_schedule_interval_s: null,
    observed_current_program_hash: null,
    observed_assigned_program_hash: null,
  });
});

test('parseSensorReadingsForExport rejects invalid JSON', () => {
  assert.throws(
    () => app.parseSensorReadingsForExport('not-json'),
    /decoded_readings.*valid JSON/i,
  );
});

test('querySensorDataRange follows continuation tokens until a partition is exhausted', async () => {
  const originalFetch = global.fetch;
  app.CONFIG.storageAccount = 'exampleacct';
  app.APP.account = { username: 'test@example.com' };
  app.APP.msalApp = {
    async acquireTokenSilent() {
      return { accessToken: 'token-123' };
    },
    setActiveAccount() {},
  };

  const urls = [];
  const authHeaders = [];
  global.fetch = async (url, options) => {
    urls.push(url);
    authHeaders.push(options.headers.Authorization);
    const parsed = new URL(url);
    const nextPartitionKey = parsed.searchParams.get('NextPartitionKey');

    if (!nextPartitionKey) {
      return {
        ok: true,
        async json() {
          return { value: [{ timestamp_ms: '1000', node_id: 'node-a' }] };
        },
        async text() {
          return '';
        },
        headers: {
          get(name) {
            if (name === 'x-ms-continuation-NextPartitionKey') return 'page-2';
            if (name === 'x-ms-continuation-NextRowKey') return 'row-2';
            return null;
          },
        },
      };
    }

    return {
      ok: true,
      async json() {
        return { value: [{ timestamp_ms: '2000', node_id: 'node-a' }] };
      },
      async text() {
        return '';
      },
      headers: { get() { return null; } },
    };
  };

  try {
    const rows = await app.querySensorDataRange(['n:abc'], 1_000, 2_000, {
      topPerPage: 1000,
      maxPagesPerPartition: 10,
    });

    assert.equal(rows.length, 2);
    assert.equal(new URL(urls[0]).searchParams.get('$top'), '1000');
    assert.equal(new URL(urls[1]).searchParams.get('NextPartitionKey'), 'page-2');
    assert.equal(new URL(urls[1]).searchParams.get('NextRowKey'), 'row-2');
    assert.deepEqual(authHeaders, ['Bearer token-123', 'Bearer token-123']);
  } finally {
    global.fetch = originalFetch;
  }
});

test('queryActualStateRange follows continuation tokens until a partition is exhausted', async () => {
  const originalFetch = global.fetch;
  app.CONFIG.storageAccount = 'exampleacct';
  app.APP.account = { username: 'test@example.com' };
  app.APP.msalApp = {
    async acquireTokenSilent() {
      return { accessToken: 'token-123' };
    },
    setActiveAccount() {},
  };

  const urls = [];
  global.fetch = async (url) => {
    urls.push(url);
    const parsed = new URL(url);
    const nextPartitionKey = parsed.searchParams.get('NextPartitionKey');

    if (!nextPartitionKey) {
      return {
        ok: true,
        async json() {
          return { value: [{ timestamp_ms: '1000', node_id: 'node-a', battery_mv: '3300' }] };
        },
        async text() {
          return '';
        },
        headers: {
          get(name) {
            if (name === 'x-ms-continuation-NextPartitionKey') return 'page-2';
            if (name === 'x-ms-continuation-NextRowKey') return 'row-2';
            return null;
          },
        },
      };
    }

    return {
      ok: true,
      async json() {
        return { value: [{ timestamp_ms: '2000', node_id: 'node-a', battery_mv: '3200' }] };
      },
      async text() {
        return '';
      },
      headers: { get() { return null; } },
    };
  };

  try {
    const rows = await app.queryActualStateRange(['n:abc'], 1_000, 2_000, {
      topPerPage: 1000,
      maxPagesPerPartition: 10,
    });

    assert.equal(rows.length, 2);
    assert.ok(urls[0].includes('/actualstate()'));
    assert.equal(new URL(urls[1]).searchParams.get('NextPartitionKey'), 'page-2');
    assert.equal(new URL(urls[1]).searchParams.get('NextRowKey'), 'row-2');
  } finally {
    global.fetch = originalFetch;
  }
});

test('querySensorDataRange rejects repeated continuation tokens for complete exports', async () => {
  const originalFetch = global.fetch;
  app.CONFIG.storageAccount = 'exampleacct';
  app.APP.account = { username: 'test@example.com' };
  app.APP.msalApp = {
    async acquireTokenSilent() {
      return { accessToken: 'token-123' };
    },
    setActiveAccount() {},
  };

  global.fetch = async () => ({
    ok: true,
    async json() {
      return { value: [{ timestamp_ms: '1000', node_id: 'node-a' }] };
    },
    async text() {
      return '';
    },
    headers: {
      get(name) {
        if (name === 'x-ms-continuation-NextPartitionKey') return 'page-2';
        if (name === 'x-ms-continuation-NextRowKey') return 'row-2';
        return null;
      },
    },
  });

  try {
    await assert.rejects(
      () => app.querySensorDataRange(['n:abc'], 1_000, 2_000, {
        topPerPage: 1000,
        maxPagesPerPartition: 10,
        requireComplete: true,
      }),
      /repeated continuation token/i,
    );
  } finally {
    global.fetch = originalFetch;
  }
});

test('loadActiveEnvironment migrates legacy series overrides into the active environment', () => {
  localStorage.setItem('sonde_environments', JSON.stringify([
    {
      name: 'prod',
      clientId: 'client',
      tenantId: 'tenant',
      storageAccount: 'storage',
      functionAppName: 'func',
    },
  ]));
  localStorage.setItem('sonde_active_environment', 'prod');
  localStorage.setItem('sonde_series_overrides', JSON.stringify({
    'n:abc|deadbeef|temp_mc': {
      displayName: 'Office Temperature',
      scaleDivisor: 1000,
      unitSuffix: '°C',
    },
  }));

  const env = app.loadActiveEnvironment();
  const stored = app.loadEnvironments();

  assert.equal(env.name, 'prod');
  assert.deepEqual(stored[0].sensorData.seriesOverrides, {
    'n:abc|deadbeef|temp_mc': {
      displayName: 'Office Temperature',
      scaleDivisor: 1000,
      unitSuffix: '°C',
    },
  });
  assert.equal(localStorage.getItem('sonde_series_overrides'), null);
  assert.equal(app.SENSOR_STATE.viewMode, 'graph');
  assert.equal(app.SENSOR_STATE.timeRange, '24h');
});

test('handleImportedJson overwrites sensorData preferences instead of merging them', () => {
  global.window.confirm = () => true;
  localStorage.setItem('sonde_environments', JSON.stringify([
    {
      name: 'other',
      clientId: 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
      tenantId: 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb',
      storageAccount: 'otherstorage',
      functionAppName: 'other-func',
      sensorData: {
        viewMode: 'graph',
        timeRange: '24h',
        selectedSeries: ['keep-me'],
        seriesOverrides: {
          'old|series|key': { displayName: 'Old', scaleDivisor: 1, unitSuffix: 'x' },
        },
      },
    },
    {
      name: 'prod',
      clientId: 'cccccccc-cccc-cccc-cccc-cccccccccccc',
      tenantId: 'dddddddd-dddd-dddd-dddd-dddddddddddd',
      storageAccount: 'prodstorage',
      functionAppName: 'prod-func',
      sensorData: {
        viewMode: 'graph',
        timeRange: '24h',
        selectedSeries: ['old-series'],
        seriesOverrides: {
          'old-series': { displayName: 'Old', scaleDivisor: 10, unitSuffix: 'x' },
        },
      },
    },
  ]));
  localStorage.setItem('sonde_active_environment', 'other');

  app.handleImportedJson(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'importedprod',
    functionAppName: 'imported-func',
    sensorData: {
      viewMode: 'table',
      timeRange: '7d',
      selectedSeries: ['new-series'],
      selectedSeriesInitialized: true,
      seriesOverrides: {
        'new-series': {
          displayName: 'Imported',
          scaleDivisor: 1000,
          unitSuffix: '°C',
        },
      },
    },
  }));

  const prod = app.loadEnvironments().find((env) => env.name === 'prod');
  assert.equal(prod.storageAccount, 'importedprod');
  assert.deepEqual(prod.sensorData, {
    viewMode: 'table',
    timeRange: '7d',
    selectedSeries: ['new-series'],
    selectedSeriesInitialized: true,
    seriesOverrides: {
      'new-series': {
        displayName: 'Imported',
        scaleDivisor: 1000,
        unitSuffix: '°C',
      },
    },
  });
});

test('handleImportedJson initializes default sensorData preferences when omitted', () => {
  app.handleImportedJson(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
  }));

  assert.deepEqual(app.loadEnvironments()[0].sensorData, app.createDefaultSensorDataPreferences());
});

test('handleImportedJson preserves an explicit empty selectedSeries preference', () => {
  localStorage.setItem('sonde_environments', JSON.stringify([
    {
      name: 'other',
      clientId: 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
      tenantId: 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb',
      storageAccount: 'otherstorage',
      functionAppName: 'other-func',
      sensorData: app.createDefaultSensorDataPreferences(),
    },
  ]));
  localStorage.setItem('sonde_active_environment', 'other');

  app.handleImportedJson(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: {
      viewMode: 'graph',
      timeRange: '24h',
      selectedSeries: [],
      seriesOverrides: {},
    },
  }));

  localStorage.setItem('sonde_active_environment', 'prod');
  app.loadActiveEnvironment();

  const prod = app.loadEnvironments().find((env) => env.name === 'prod');
  assert.deepEqual(prod.sensorData, {
    viewMode: 'graph',
    timeRange: '24h',
    selectedSeries: [],
    selectedSeriesInitialized: true,
    seriesOverrides: {},
  });
  assert.deepEqual([...app.SENSOR_STATE.selectedSeries], []);
  assert.equal(app.SENSOR_STATE.seriesInitialized, true);
});

test('handleImportedJson normalizes dashboard custom time ranges from import payloads', () => {
  app.handleImportedJson(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    dashboards: [{
      name: 'Imported Dashboard',
      variables: [],
      charts: [],
      timeRange: {
        preset: 'custom',
        start: '0',
        end: '1000',
      },
    }],
  }));

  const prod = app.loadEnvironments().find((env) => env.name === 'prod');
  assert.deepEqual(prod.dashboards[0].timeRange, {
    preset: 'custom',
    start: 0,
    end: 1000,
  });
});

test('handleImportedJson migrates legacy dashboard metrics into a default chart', () => {
  app.handleImportedJson(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    dashboards: [{
      name: 'Imported Dashboard',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      metrics: [{ displayName: 'Temp', expression: 'TEMP / 1000', color: '#123456' }],
      timeRange: { preset: '24h', start: null, end: null },
    }],
  }));

  const prod = app.loadEnvironments().find((env) => env.name === 'prod');
  assert.equal(prod.dashboards[0].charts.length, 1);
  assert.equal(prod.dashboards[0].charts[0].name, 'Chart 1');
  assert.equal(prod.dashboards[0].charts[0].metrics[0].expression, 'TEMP / 1000');
  assert.equal(prod.dashboards[0].variablesCollapsed, false);
  assert.equal(prod.dashboards[0].charts[0].metricsCollapsed, false);
});

test('handleImportedJson restores persisted dashboard pane states', () => {
  app.handleImportedJson(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    dashboards: [{
      name: 'Imported Dashboard',
      variablesCollapsed: true,
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{
        name: 'Chart 1',
        metricsCollapsed: true,
        metrics: [{ displayName: 'Temp', expression: 'TEMP / 1000', color: '#123456' }],
      }],
      timeRange: { preset: '24h', start: null, end: null },
    }],
  }));

  const prod = app.loadEnvironments().find((env) => env.name === 'prod');
  assert.equal(prod.dashboards[0].variablesCollapsed, true);
  assert.equal(prod.dashboards[0].charts[0].metricsCollapsed, true);
});

test('handleImportedJson rejects dashboard variables that fail editor validation', () => {
  assert.throws(
    () => app.handleImportedJson(JSON.stringify({
      version: 1,
      name: 'prod',
      clientId: '11111111-1111-1111-1111-111111111111',
      tenantId: '22222222-2222-2222-2222-222222222222',
      storageAccount: 'prodstorage',
      functionAppName: 'prod-func',
      dashboards: [{
        name: 'Imported Dashboard',
        variables: [{ name: '__proto__', nodeId: 'NODE_001', readingType: 'temp_mc' }],
        charts: [],
        timeRange: { preset: '24h', start: null, end: null },
      }],
    })),
    /reserved/i,
  );
});

test('validateImportedSensorDataPreferences rejects malformed selectedSeries arrays', () => {
  assert.throws(
    () => app.validateImportedSensorDataPreferences({ selectedSeries: ['ok', 42] }),
    /selectedSeries/,
  );
});

test('validateImportedSensorDataPreferences rejects reserved series override keys', () => {
  const payload = JSON.parse('{"seriesOverrides":{"__proto__":{"displayName":"bad"}}}');
  assert.throws(
    () => app.validateImportedSensorDataPreferences(payload),
    /reserved key/,
  );
});

test('persistActiveSensorDataPreferences stores environment-scoped Sensor Data view state', () => {
  localStorage.setItem('sonde_environments', JSON.stringify([
    {
      name: 'prod',
      clientId: '11111111-1111-1111-1111-111111111111',
      tenantId: '22222222-2222-2222-2222-222222222222',
      storageAccount: 'prodstorage',
      functionAppName: 'prod-func',
      sensorData: app.createDefaultSensorDataPreferences(),
    },
  ]));
  localStorage.setItem('sonde_active_environment', 'prod');

  assert.equal(app.saveSeriesOverrides({
    'n:abc|deadbeef|temp_mc': {
      displayName: 'Temp',
      scaleDivisor: 1000,
      unitSuffix: '°C',
    },
  }), true);

  app.SENSOR_STATE.viewMode = 'table';
  app.SENSOR_STATE.timeRange = '7d';
  app.SENSOR_STATE.selectedSeries = new Set(['n:abc|deadbeef|temp_mc']);

  assert.equal(app.persistActiveSensorDataPreferences(), true);

  assert.deepEqual(app.loadEnvironments()[0].sensorData, {
    viewMode: 'table',
    timeRange: '7d',
    selectedSeries: ['n:abc|deadbeef|temp_mc'],
    selectedSeriesInitialized: true,
    seriesOverrides: {
      'n:abc|deadbeef|temp_mc': {
        displayName: 'Temp',
        scaleDivisor: 1000,
        unitSuffix: '°C',
      },
    },
  });
});

test('clearPersistedSelectedSeriesPreference restores default-selection semantics', () => {
  localStorage.setItem('sonde_environments', JSON.stringify([
    {
      name: 'prod',
      clientId: '11111111-1111-1111-1111-111111111111',
      tenantId: '22222222-2222-2222-2222-222222222222',
      storageAccount: 'prodstorage',
      functionAppName: 'prod-func',
      sensorData: {
        viewMode: 'graph',
        timeRange: '24h',
        selectedSeries: ['stale-series'],
        selectedSeriesInitialized: true,
        seriesOverrides: {},
      },
    },
  ]));
  localStorage.setItem('sonde_active_environment', 'prod');

  assert.equal(app.clearPersistedSelectedSeriesPreference(), true);
  assert.deepEqual(app.loadEnvironments()[0].sensorData, {
    viewMode: 'graph',
    timeRange: '24h',
    selectedSeries: [],
    selectedSeriesInitialized: false,
    seriesOverrides: {},
  });
});

test('buildEnvironmentExportData includes environment-scoped Sensor Data preferences', () => {
  assert.deepEqual(
    app.buildEnvironmentExportData({
      name: 'prod',
      clientId: '11111111-1111-1111-1111-111111111111',
      tenantId: '22222222-2222-2222-2222-222222222222',
      storageAccount: 'prodstorage',
      functionAppName: 'prod-func',
      sensorData: {
        viewMode: 'table',
        timeRange: '1h',
        selectedSeries: ['series-a'],
        seriesOverrides: {
          'series-a': { displayName: 'Series A', scaleDivisor: 2, unitSuffix: 'V' },
        },
      },
    }),
    {
      version: 1,
      name: 'prod',
      clientId: '11111111-1111-1111-1111-111111111111',
      tenantId: '22222222-2222-2222-2222-222222222222',
      storageAccount: 'prodstorage',
      functionAppName: 'prod-func',
      sensorData: {
        viewMode: 'table',
        timeRange: '1h',
        selectedSeries: ['series-a'],
        seriesOverrides: {
          'series-a': { displayName: 'Series A', scaleDivisor: 2, unitSuffix: 'V' },
        },
      },
      dashboards: [],
    },
  );
});

test('buildEnvironmentExportData omits dashboard validation annotations', () => {
  const exported = app.buildEnvironmentExportData({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: app.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Imported Dashboard',
      variablesCollapsed: true,
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{
        name: 'Chart 1',
        metricsCollapsed: true,
        metrics: [{ id: 'metric-1', displayName: 'Warns', expression: 'TEMP + UNKNOWN', color: '#123456' }],
      }],
      timeRange: { preset: '24h', start: null, end: null },
    }],
  });

  assert.deepEqual(exported.dashboards, [{
    name: 'Imported Dashboard',
    variablesCollapsed: true,
    variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
    charts: [{
      name: 'Chart 1',
      metricsCollapsed: true,
      metrics: [{ id: 'metric-1', displayName: 'Warns', expression: 'TEMP + UNKNOWN', color: '#123456' }],
    }],
    timeRange: { preset: '24h', start: null, end: null },
  }]);
});

test('renderDashboardContent defaults the Variables pane to expanded', () => {
  const html = app.renderDashboardContent({
    name: 'Dashboard',
    variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
    charts: [],
    timeRange: { preset: '24h', start: null, end: null },
  });

  assert.match(html, /id="dashboard-variables-pane" open/);
});

test('renderChartCard keeps the graph visible when Metrics pane is collapsed', () => {
  const html = app.renderChartCard({
    name: 'Chart 1',
    metricsCollapsed: true,
    metrics: [{ id: 'metric-1', displayName: 'Temp', expression: 'TEMP / 1000', color: '#123456' }],
  }, 0, 1);

  assert.doesNotMatch(html, /data-chart-metrics-pane="0" open/);
  assert.match(html, /class="metric-chart-container"/);
  assert.match(html, /data-add-metric="0"/);
});

test('activateEnvironmentState loads saved Sensor Data preferences for the selected environment', () => {
  app.SENSOR_STATE.timeRange = '24h';
  app.SENSOR_STATE.viewMode = 'graph';
  app.SENSOR_STATE.selectedSeries = new Set(['old-series']);
  app.SENSOR_STATE.seriesInitialized = true;

  app.activateEnvironmentState('prod', {
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: {
      viewMode: 'table',
      timeRange: '7d',
      selectedSeries: ['new-series'],
      selectedSeriesInitialized: true,
      seriesOverrides: {},
    },
  });

  assert.equal(app.CONFIG.storageAccount, 'prodstorage');
  assert.equal(app.SENSOR_STATE.viewMode, 'table');
  assert.equal(app.SENSOR_STATE.timeRange, '7d');
  assert.deepEqual([...app.SENSOR_STATE.selectedSeries], ['new-series']);
  assert.equal(app.SENSOR_STATE.seriesInitialized, true);
});

test('activateEnvironmentState resets transient Sensor Data state on environment switch', () => {
  app.SENSOR_STATE.autoRefresh = true;
  app.SENSOR_STATE.exportStartMs = 111;
  app.SENSOR_STATE.exportEndMs = 222;
  app.SENSOR_STATE.exportFormat = 'csv';
  app.SENSOR_STATE.exportBusy = true;
  app.SENSOR_STATE.exportMessage = { kind: 'error', text: 'stale' };

  app.activateEnvironmentState('prod', {
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: app.createDefaultSensorDataPreferences(),
  });

  assert.equal(app.SENSOR_STATE.autoRefresh, false);
  assert.equal(app.SENSOR_STATE.exportStartMs, null);
  assert.equal(app.SENSOR_STATE.exportEndMs, null);
  assert.equal(app.SENSOR_STATE.exportFormat, 'jsonl');
  assert.equal(app.SENSOR_STATE.exportBusy, false);
  assert.equal(app.SENSOR_STATE.exportMessage, null);
});

test('pruneUnavailableSelectedSeries removes stale saved selections without touching valid ones', () => {
  const selectedSeries = new Set(['keep', 'drop']);
  const changed = app.pruneUnavailableSelectedSeries(selectedSeries, new Set(['keep']));
  assert.equal(changed, true);
  assert.deepEqual([...selectedSeries], ['keep']);
});

test('pruning all stale saved selections re-enables default auto-selection behavior', () => {
  app.SENSOR_STATE.selectedSeries = new Set(['stale-series']);
  app.SENSOR_STATE.seriesInitialized = true;

  const changed = app.pruneUnavailableSelectedSeries(app.SENSOR_STATE.selectedSeries, new Set());
  if (changed && app.SENSOR_STATE.selectedSeries.size === 0) {
    app.SENSOR_STATE.seriesInitialized = false;
  }

  assert.equal(changed, true);
  assert.deepEqual([...app.SENSOR_STATE.selectedSeries], []);
  assert.equal(app.SENSOR_STATE.seriesInitialized, false);
});

test('buildEnvironmentExportData and import validation distinguish omitted and empty selectedSeries', () => {
  const defaultExport = app.buildEnvironmentExportData({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: app.createDefaultSensorDataPreferences(),
  });
  assert.equal('selectedSeries' in defaultExport.sensorData, false);
  assert.equal(app.validateImportedSensorDataPreferences(defaultExport.sensorData).selectedSeriesInitialized, false);

  const emptySelectionExport = app.buildEnvironmentExportData({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: {
      viewMode: 'graph',
      timeRange: '24h',
      selectedSeries: [],
      selectedSeriesInitialized: true,
      seriesOverrides: {},
    },
  });
  assert.deepEqual(emptySelectionExport.sensorData.selectedSeries, []);
  assert.equal(app.validateImportedSensorDataPreferences(emptySelectionExport.sensorData).selectedSeriesInitialized, true);
});

test('validateVariableName rejects blocked object keys', () => {
  const validation = app.validateVariableName('__proto__', []);
  assert.equal(validation.valid, false);
  assert.match(validation.error, /reserved/i);
});

test('validateExpression reports undefined variables as warnings, not errors', () => {
  const originalExprEval = global.exprEval;
  global.exprEval = {
    Parser: class {
      parse() {
        return {
          variables() { return ['TEMP', 'UNKNOWN']; },
        };
      }
    }
  };

  try {
    const validation = app.validateExpression('TEMP + UNKNOWN', ['TEMP']);
    assert.equal(validation.valid, true);
    assert.equal(validation.error, undefined);
    assert.equal(validation.warning, 'Undefined variables: UNKNOWN');
  } finally {
    global.exprEval = originalExprEval;
  }
});

test('index.html pins expr-eval with the current CDN SHA-384 hash', () => {
  const indexHtml = fs.readFileSync(path.resolve(__dirname, '..', 'deploy', 'web-ui', 'index.html'), 'utf8');
  assert.match(
    indexHtml,
    /<script src="https:\/\/cdn\.jsdelivr\.net\/npm\/expr-eval@2\.0\.2\/dist\/bundle\.min\.js" integrity="sha384-\/4W8jQ\+DhKy3zhz8s0HRh\/l4kEliBAJQmAWqrrtJr7BAuODvB2MKsjR8h5MGefjl" crossorigin="anonymous"><\/script>/,
  );
});

// Dashboard runtime behavior tests

test('fetchReadingTypesForNode discovers numeric reading types for the selected node', async () => {
  const nowMs = 9_000_000;
  const readingTypes = await app.fetchReadingTypesForNode('NODE_001', {
    nowFn: () => nowMs,
    fetchActualStateNodesFn: async () => [
      { nodeId: 'NODE_001', partitionKey: 'n:abc123' },
      { nodeId: 'NODE_002', partitionKey: 'n:def456' },
    ],
    querySensorDataRangeFn: async (partitionKeys, startMs, endMs, options) => {
      assert.deepEqual(partitionKeys, ['n:abc123']);
      assert.equal(startMs, nowMs - (7 * 24 * 60 * 60 * 1000));
      assert.equal(endMs, nowMs);
      assert.equal(options.maxPagesPerPartition, 5);
      return [
        {
          PartitionKey: 'n:abc123',
          RowKey: 'row-1',
          timestamp_ms: String(nowMs - 1000),
          decoded_readings: '{"temp_mc":25000,"humidity_pct":"45","status":"ok"}',
        },
        {
          PartitionKey: 'n:abc123',
          RowKey: 'row-2',
          timestamp_ms: String(nowMs - 500),
          decoded_readings: '{"pressure_pa":92500,"temp_mc":26000}',
        },
      ];
    },
  });

  assert.deepEqual(readingTypes, ['humidity_pct', 'pressure_pa', 'temp_mc']);
});

test('fetchVariableData validates reading types and honors the 6h preset window', async () => {
  const nowMs = 40_000_000;
  const result = await app.fetchVariableData([
    { name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' },
    { name: 'PRESS', nodeId: 'NODE_001', readingType: 'pressure_pa' },
  ], { preset: '6h' }, {
    nowFn: () => nowMs,
    fetchActualStateNodesFn: async () => [
      { nodeId: 'NODE_001', partitionKey: 'n:abc123' },
    ],
    querySensorDataRangeFn: async (partitionKeys, startMs, endMs, options) => {
      assert.deepEqual(partitionKeys, ['n:abc123']);
      assert.equal(startMs, nowMs - (6 * 60 * 60 * 1000));
      assert.equal(endMs, nowMs);
      assert.equal(options.maxPagesPerPartition, 10);
      return [
        {
          PartitionKey: 'n:abc123',
          RowKey: 'row-1',
          timestamp_ms: String(nowMs - 1000),
          decoded_readings: '{"temp_mc":25000}',
        },
      ];
    },
  });

  assert.deepEqual(result.data.TEMP, [{ timestamp: nowMs - 1000, value: 25000 }]);
  assert.deepEqual(result.data.PRESS, []);
  assert.deepEqual(result.errors, ['Reading type "pressure_pa" not found for node "NODE_001"']);
});

test('fetchVariableData honors custom dashboard time ranges', async () => {
  await app.fetchVariableData([
    { name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' },
  ], {
    preset: 'custom',
    start: 1111,
    end: 2222,
  }, {
    fetchActualStateNodesFn: async () => [
      { nodeId: 'NODE_001', partitionKey: 'n:abc123' },
    ],
    querySensorDataRangeFn: async (partitionKeys, startMs, endMs) => {
      assert.deepEqual(partitionKeys, ['n:abc123']);
      assert.equal(startMs, 1111);
      assert.equal(endMs, 2222);
      return [];
    },
  });
});

test('fetchActualStateNodes reuses the session cache and discovers new nodes from delta refresh', async () => {
  const rows = [
    [{
      PartitionKey: 'n:abc123',
      RowKey: 'ffff',
      node_id: 'NODE_001',
      timestamp_ms: '1000',
    }],
    [{
      PartitionKey: 'n:def456',
      RowKey: 'fffe',
      node_id: 'NODE_002',
      timestamp_ms: '2000',
    }],
  ];
  const filters = [];
  let callIndex = 0;

  const first = await app.fetchActualStateNodes({
    nowFn: () => 1_500,
    queryTableFn: async (_tableName, filter) => {
      filters.push(filter);
      return rows[callIndex++] || [];
    },
  });
  const second = await app.fetchActualStateNodes({
    nowFn: () => 2_500,
    queryTableFn: async (_tableName, filter) => {
      filters.push(filter);
      return rows[callIndex++] || [];
    },
  });

  assert.deepEqual(first, [
    { partitionKey: 'n:abc123', nodeId: 'NODE_001' },
  ]);
  assert.deepEqual(second, [
    { partitionKey: 'n:abc123', nodeId: 'NODE_001' },
    { partitionKey: 'n:def456', nodeId: 'NODE_002' },
  ]);
  assert.equal(filters[0], '');
  assert.notEqual(filters[1], '');
});

test('getCachedSensorDataRows fetches only uncovered historical intervals', async () => {
  const calls = [];
  const rowsByCall = [
    [{
      PartitionKey: 'n:abc123',
      RowKey: 'row-2',
      timestamp_ms: '2000',
      decoded_readings: '{"temp_mc":25000}',
    }],
    [{
      PartitionKey: 'n:abc123',
      RowKey: 'row-1',
      timestamp_ms: '1000',
      decoded_readings: '{"temp_mc":24000}',
    }],
  ];

  const deps = {
    querySensorDataRangeFn: async (partitionKeys, startMs, endMs) => {
      calls.push({ partitionKeys, startMs, endMs });
      return rowsByCall[calls.length - 1] || [];
    },
  };

  await app.getCachedSensorDataRows(['n:abc123'], 2000, 3000, {
    topPerPage: 1000,
    maxPagesPerPartition: 10,
  }, deps);
  const expanded = await app.getCachedSensorDataRows(['n:abc123'], 1000, 3000, {
    topPerPage: 1000,
    maxPagesPerPartition: 10,
  }, deps);
  await app.getCachedSensorDataRows(['n:abc123'], 1000, 3000, {
    topPerPage: 1000,
    maxPagesPerPartition: 10,
  }, deps);

  assert.deepEqual(calls, [
    { partitionKeys: ['n:abc123'], startMs: 2000, endMs: 3000 },
    { partitionKeys: ['n:abc123'], startMs: 1000, endMs: 2000 },
  ]);
  assert.deepEqual(
    expanded.map((row) => row.RowKey).sort(),
    ['row-1', 'row-2'],
  );
});

test('getCachedSensorDataRows does not mark truncated fetches as full historical coverage', async () => {
  const calls = [];
  const firstRows = [{
    PartitionKey: 'n:abc123',
    RowKey: 'row-2',
    timestamp_ms: '2000',
    decoded_readings: '{"temp_mc":25000}',
  }];
  Object.defineProperty(firstRows, '__complete', {
    value: false,
    enumerable: false,
    configurable: true,
  });

  const deps = {
    querySensorDataRangeFn: async (partitionKeys, startMs, endMs) => {
      calls.push({ partitionKeys, startMs, endMs });
      if (calls.length === 1) {
        return firstRows;
      }
      return [{
        PartitionKey: 'n:abc123',
        RowKey: 'row-1',
        timestamp_ms: '1000',
        decoded_readings: '{"temp_mc":24000}',
      }];
    },
  };

  await app.getCachedSensorDataRows(['n:abc123'], 2000, 3000, {
    topPerPage: 1000,
    maxPagesPerPartition: 1,
  }, deps);
  await app.getCachedSensorDataRows(['n:abc123'], 1000, 3000, {
    topPerPage: 1000,
    maxPagesPerPartition: 1,
  }, deps);

  assert.deepEqual(calls, [
    { partitionKeys: ['n:abc123'], startMs: 2000, endMs: 3000 },
    { partitionKeys: ['n:abc123'], startMs: 1000, endMs: 3000 },
  ]);
});

test('getCachedSensorDataRows does not treat truncated exact-range fetches as exact cache hits', async () => {
  const calls = [];
  const incompleteRows = [{
    PartitionKey: 'n:abc123',
    RowKey: 'row-2',
    timestamp_ms: '2000',
    decoded_readings: '{"temp_mc":25000}',
  }];
  Object.defineProperty(incompleteRows, '__complete', {
    value: false,
    enumerable: false,
    configurable: true,
  });

  const deps = {
    querySensorDataRangeFn: async (partitionKeys, startMs, endMs) => {
      calls.push({ partitionKeys, startMs, endMs });
      return incompleteRows;
    },
  };

  await app.getCachedSensorDataRows(['n:abc123'], 2000, 3000, {
    topPerPage: 1000,
    maxPagesPerPartition: 1,
  }, deps);
  await app.getCachedSensorDataRows(['n:abc123'], 2000, 3000, {
    topPerPage: 1000,
    maxPagesPerPartition: 1,
  }, deps);

  assert.deepEqual(calls, [
    { partitionKeys: ['n:abc123'], startMs: 2000, endMs: 3000 },
    { partitionKeys: ['n:abc123'], startMs: 2000, endMs: 3000 },
  ]);
});

test('fetchVariableData reuses one cached sensor fetch for overlapping variables on the same partition', async () => {
  let queryCount = 0;
  const deps = {
    nowFn: () => 40_000_000,
    fetchActualStateNodesFn: async () => [
      { nodeId: 'NODE_001', partitionKey: 'n:abc123' },
    ],
    querySensorDataRangeFn: async () => {
      queryCount += 1;
      return [{
        PartitionKey: 'n:abc123',
        RowKey: 'row-1',
        timestamp_ms: String(40_000_000 - 1000),
        decoded_readings: '{"temp_mc":25000,"pressure_pa":92500}',
      }];
    },
  };

  const result = await app.fetchVariableData([
    { name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' },
    { name: 'PRESS', nodeId: 'NODE_001', readingType: 'pressure_pa' },
  ], { preset: '6h' }, deps);

  assert.equal(queryCount, 1);
  assert.deepEqual(result.data.TEMP, [{ timestamp: 40_000_000 - 1000, value: 25000 }]);
  assert.deepEqual(result.data.PRESS, [{ timestamp: 40_000_000 - 1000, value: 92500 }]);
});

test('evaluateMetricTimeSeries propagates fetch failures as user-visible errors', async () => {
  const result = await app.evaluateMetricTimeSeries({
    expression: 'TEMP / 1000',
  }, [
    { name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' },
  ], { preset: '24h' }, {
    parserFactory: () => ({
      parse(expression) {
        assert.equal(expression, 'TEMP / 1000');
        return {
          variables() { return ['TEMP']; },
          evaluate() { return 0; },
        };
      },
    }),
    fetchVariableDataFn: async () => ({
      data: {},
      errors: ['Failed to fetch data for node(s) NODE_001: Network timeout'],
    }),
  });

  assert.equal(result.error, 'Failed to fetch data for node(s) NODE_001: Network timeout');
  assert.deepEqual(result.points, []);
});

test('evaluateMetricTimeSeries computes time-series points from real fetched data', async () => {
  const result = await app.evaluateMetricTimeSeries({
    expression: 'TEMP / 1000',
  }, [
    { name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' },
  ], { preset: '24h' }, {
    parserFactory: () => ({
      parse(expression) {
        assert.equal(expression, 'TEMP / 1000');
        return {
          variables() { return ['TEMP']; },
          evaluate(context) { return context.TEMP / 1000; },
        };
      },
    }),
    fetchVariableDataFn: async () => ({
      data: {
        TEMP: [
          { timestamp: 1000, value: 25000 },
          { timestamp: 2000, value: 25500 },
        ],
      },
      errors: [],
    }),
  });

  assert.deepEqual(result.points, [
    { timestamp: 1000, value: 25 },
    { timestamp: 2000, value: 25.5 },
  ]);
});

test('evaluateMetricTimeSeries reports parser construction failures as expression errors', async () => {
  const result = await app.evaluateMetricTimeSeries({
    expression: 'TEMP / 1000',
  }, [
    { name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' },
  ], { preset: '24h' }, {
    parserFactory: () => {
      throw new Error('exprEval is not defined');
    },
    fetchVariableDataFn: async () => {
      throw new Error('fetchVariableData should not run when parser construction fails');
    },
  });

  assert.equal(result.error, 'Expression error: exprEval is not defined');
  assert.deepEqual(result.points, []);
});

test('evaluateMetricTimeSeries reports undefined variables explicitly', async () => {
  const result = await app.evaluateMetricTimeSeries({
    expression: 'TEMP + UNKNOWN',
  }, [
    { name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' },
  ], { preset: '24h' }, {
    parserFactory: () => ({
      parse() {
        return {
          variables() { return ['TEMP', 'UNKNOWN']; },
          evaluate() { return 0; },
        };
      },
    }),
    fetchVariableDataFn: async () => {
      throw new Error('fetchVariableData should not run when variables are undefined');
    },
  });

  assert.equal(result.error, 'Undefined variables: UNKNOWN');
  assert.deepEqual(result.points, []);
});

test('renderMetricCharts shows a no-data message when evaluation yields zero points', async () => {
  const originalGetElementById = global.document.getElementById;
  const parent = makeElement();
  const canvas = makeElement();
  canvas.parentElement = parent;
  let destroyed = 0;
  app.APP_DASHBOARD_STATE.metricCharts[0] = {
    destroy() { destroyed += 1; },
  };
  global.document.getElementById = (id) => {
    if (id === 'metric-chart-0') return canvas;
    return makeElement();
  };

  try {
    await app.renderMetricCharts({
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{ name: 'Chart 1', metrics: [{ displayName: 'Temp', expression: 'TEMP / 1000' }] }],
      timeRange: { preset: '24h' },
    }, {
      evaluateMetricTimeSeriesFn: async () => ({ points: [] }),
      chartFactory: () => {
        throw new Error('chartFactory should not run for empty metrics');
      },
    });

    assert.match(parent.innerHTML, /No data in selected time range\./);
    assert.equal(destroyed, 1);
    assert.equal(app.APP_DASHBOARD_STATE.metricCharts[0], undefined);
  } finally {
    global.document.getElementById = originalGetElementById;
  }
});

test('renderMetricCharts prefers errors over no-data fallback messages', async () => {
  const originalGetElementById = global.document.getElementById;
  const parent = makeElement();
  const canvas = makeElement();
  canvas.parentElement = parent;
  global.document.getElementById = (id) => {
    if (id === 'metric-chart-0') return canvas;
    return makeElement();
  };

  try {
    await app.renderMetricCharts({
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{
        name: 'Chart 1',
        metrics: [
          { displayName: 'No Data', expression: 'TEMP' },
          { displayName: 'Broken', expression: 'BROKEN' },
        ],
      }],
      timeRange: { preset: '24h' },
    }, {
      evaluateMetricTimeSeriesFn: async (metric) => (
        metric.expression === 'TEMP'
          ? { points: [] }
          : { error: 'Expression error: bad parse', points: [] }
      ),
      chartFactory: () => {
        throw new Error('chartFactory should not run when all metrics fail');
      },
    });

    assert.match(parent.innerHTML, /Expression error: bad parse/);
    assert.doesNotMatch(parent.innerHTML, /No data in selected time range/);
  } finally {
    global.document.getElementById = originalGetElementById;
  }
});

test('renderMetricCharts downsamples dashboard datasets to 500 points before charting', async () => {
  const originalGetElementById = global.document.getElementById;
  const parent = makeElement();
  const canvas = makeElement();
  canvas.parentElement = parent;
  global.document.getElementById = (id) => {
    if (id === 'metric-chart-0') return canvas;
    return makeElement();
  };

  const rawPoints = Array.from({ length: 1200 }, (_, index) => ({
    timestamp: 1000 + index,
    value: index,
  }));
  let capturedConfig = null;

  try {
    await app.renderMetricCharts({
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{ name: 'Chart 1', metrics: [{ displayName: 'Dense', expression: 'TEMP' }] }],
      timeRange: { preset: '24h' },
    }, {
      evaluateMetricTimeSeriesFn: async () => ({ points: rawPoints }),
      chartFactory: (chartCanvas, config) => {
        assert.equal(chartCanvas, canvas);
        capturedConfig = config;
        return { destroy() {} };
      },
    });

    assert.ok(capturedConfig);
    assert.equal(capturedConfig.data.datasets[0].data.length, 500);
    assert.deepEqual(capturedConfig.data.datasets[0].data[0], { x: 1000, y: 0 });
    assert.deepEqual(capturedConfig.data.datasets[0].data.at(-1), { x: 2199, y: 1199 });
  } finally {
    global.document.getElementById = originalGetElementById;
  }
});

test('renderMetricCharts combines multiple metrics on the same chart into multiple datasets', async () => {
  const originalGetElementById = global.document.getElementById;
  const parent = makeElement();
  const canvas = makeElement();
  canvas.parentElement = parent;
  global.document.getElementById = (id) => {
    if (id === 'metric-chart-0') return canvas;
    return makeElement();
  };

  const resultsByExpression = {
    TEMP: { points: [{ timestamp: 1000, value: 1 }] },
    PRESS_DERIVED: { points: [{ timestamp: 1000, value: 2 }] },
  };
  let capturedConfig = null;

  try {
    await app.renderMetricCharts({
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{
        name: 'Chart 1',
        metrics: [
          { displayName: 'Temp', expression: 'TEMP', color: '#111111' },
          { displayName: 'Press', expression: 'PRESS', color: '#222222', _validationWarning: 'Undefined variables: PRESS' },
          { displayName: 'Press Derived', expression: 'PRESS_DERIVED', color: '#333333' },
        ],
      }],
      timeRange: { preset: '24h' },
    }, {
      evaluateMetricTimeSeriesFn: async (metric) => resultsByExpression[metric.expression] || { points: [] },
      chartFactory: (chartCanvas, config) => {
        assert.equal(chartCanvas, canvas);
        capturedConfig = config;
        return { destroy() {} };
      },
    });

    assert.ok(capturedConfig);
    assert.equal(capturedConfig.data.datasets.length, 2);
    assert.deepEqual(capturedConfig.data.datasets.map((dataset) => dataset.label), ['Temp', 'Press Derived']);
  } finally {
    global.document.getElementById = originalGetElementById;
  }
});

test('renderMetricCharts formats tooltip titles as local date-time strings', async () => {
  const originalGetElementById = global.document.getElementById;
  const parent = makeElement();
  const canvas = makeElement();
  canvas.parentElement = parent;
  global.document.getElementById = (id) => {
    if (id === 'metric-chart-0') return canvas;
    return makeElement();
  };

  let capturedConfig = null;
  const timestamp = 1_781_692_819_928;

  try {
    await app.renderMetricCharts({
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{ name: 'Chart 1', metrics: [{ displayName: 'Temperature', expression: 'TEMP' }] }],
      timeRange: { preset: '24h' },
    }, {
      evaluateMetricTimeSeriesFn: async () => ({ points: [{ timestamp, value: 53.254 }] }),
      chartFactory: (chartCanvas, config) => {
        assert.equal(chartCanvas, canvas);
        capturedConfig = config;
        return { destroy() {} };
      },
    });

    assert.ok(capturedConfig);
    assert.equal(
      capturedConfig.options.plugins.tooltip.callbacks.title([{ parsed: { x: timestamp } }]),
      new Date(timestamp).toLocaleString(),
    );
  } finally {
    global.document.getElementById = originalGetElementById;
  }
});

test('renderMetricCharts formats x-axis ticks as time-only for 24h and date + time for longer dashboard ranges', async () => {
  const originalGetElementById = global.document.getElementById;
  const parent = makeElement();
  const canvas = makeElement();
  canvas.parentElement = parent;
  global.document.getElementById = (id) => {
    if (id === 'metric-chart-0') return canvas;
    return makeElement();
  };

  const basePoint = { timestamp: Date.UTC(2026, 5, 17, 20, 5, 0), value: 1 };
  const capturedConfigs = [];
  const date = new Date(basePoint.timestamp);
  const expectedTimeOnly = `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
  const expectedDateTime = `${date.getMonth() + 1}/${date.getDate()} ${expectedTimeOnly}`;

  try {
    for (const timeRange of [
      { preset: '24h' },
      { preset: '7d' },
      { preset: 'custom', start: 0, end: (24 * 60 * 60 * 1000) + 1 },
    ]) {
      await app.renderMetricCharts({
        variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
        charts: [{ name: 'Chart 1', metrics: [{ displayName: 'Temperature', expression: 'TEMP' }] }],
        timeRange,
      }, {
        evaluateMetricTimeSeriesFn: async () => ({ points: [basePoint] }),
        chartFactory: (_chartCanvas, config) => {
          capturedConfigs.push(config);
          return { destroy() {} };
        },
      });
    }

    assert.equal(capturedConfigs.length, 3);
    assert.equal(
      capturedConfigs[0].options.scales.x.ticks.callback(basePoint.timestamp),
      expectedTimeOnly,
    );
    assert.equal(
      capturedConfigs[1].options.scales.x.ticks.callback(basePoint.timestamp),
      expectedDateTime,
    );
    assert.equal(
      capturedConfigs[2].options.scales.x.ticks.callback(basePoint.timestamp),
      expectedDateTime,
    );
  } finally {
    global.document.getElementById = originalGetElementById;
  }
});

test('persistDashboardEnvironment preserves edited dashboards in memory after quota failures', () => {
  const env = {
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: app.createDefaultSensorDataPreferences(),
    dashboards: [{ name: 'Original', variables: [], charts: [], timeRange: { preset: '24h', start: null, end: null } }],
  };
  app.activateEnvironmentState(env.name, env);

  const originalSetItem = global.localStorage.setItem;
  global.localStorage.setItem = () => {
    const error = new Error('quota');
    error.name = 'QuotaExceededError';
    throw error;
  };

  try {
    const editedEnv = {
      ...env,
      dashboards: [{ name: 'Edited', variables: [], charts: [], timeRange: { preset: '24h', start: null, end: null } }],
    };
    const ok = app.persistDashboardEnvironment(editedEnv, [env]);
    assert.equal(ok, false);
    const loaded = app.loadActiveEnvironment();
    assert.equal(loaded.dashboards[0].name, 'Edited');
    assert.equal(app.APP_DASHBOARD_STATE.unsavedEnvironment.dashboards[0].name, 'Edited');
  } finally {
    global.localStorage.setItem = originalSetItem;
  }
});

test('normalizeEnvironmentRecord migrates legacy top-level dashboard metrics on load', () => {
  const normalized = app.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    dashboards: [{
      name: 'Legacy',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      metrics: [{ displayName: 'Temp', expression: 'TEMP', color: '#123456' }],
      timeRange: { preset: '24h', start: null, end: null },
    }],
  });

  assert.equal(normalized.dashboards[0].charts.length, 1);
  assert.equal(normalized.dashboards[0].charts[0].name, 'Chart 1');
  assert.equal(normalized.dashboards[0].charts[0].metrics[0].expression, 'TEMP');
  assert.equal(normalized.dashboards[0].variablesCollapsed, false);
  assert.equal(normalized.dashboards[0].charts[0].metricsCollapsed, false);
});

test('normalizeEnvironmentRecord clears stale metric validation annotations before revalidating', () => {
  const originalExprEval = global.exprEval;
  global.exprEval = {
    Parser: class {
      parse() {
        return {
          variables() {
            return ['TEMP'];
          },
        };
      }
    },
  };

  try {
    const normalized = app.normalizeEnvironmentRecord({
      name: 'prod',
      clientId: '11111111-1111-1111-1111-111111111111',
      tenantId: '22222222-2222-2222-2222-222222222222',
      storageAccount: 'prodstorage',
      functionAppName: 'prod-func',
      dashboards: [{
        name: 'Dashboard',
        variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
        charts: [{
          name: 'Chart 1',
          metrics: [{
            id: 'metric-1',
            displayName: 'Temp',
            expression: 'TEMP',
            color: '#123456',
            _validationWarning: 'Undefined variables: TEMP',
            _validationError: 'Syntax error: stale',
          }],
        }],
        timeRange: { preset: '24h', start: null, end: null },
      }],
    });

    assert.equal(normalized.dashboards[0].charts[0].metrics[0]._validationWarning, undefined);
    assert.equal(normalized.dashboards[0].charts[0].metrics[0]._validationError, undefined);
  } finally {
    global.exprEval = originalExprEval;
  }
});

test('destroyAllDashboardCharts destroys and clears all retained dashboard charts', () => {
  let destroyed = 0;
  app.APP_DASHBOARD_STATE.metricCharts = {
    0: { destroy() { destroyed += 1; } },
    1: { destroy() { destroyed += 1; } },
  };

  app.destroyAllDashboardCharts();

  assert.equal(destroyed, 2);
  assert.deepEqual(app.APP_DASHBOARD_STATE.metricCharts, {});
});

test('persistDashboardEnvironment strips dashboard validation annotations before saving', () => {
  const env = {
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: app.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Persisted',
      variablesCollapsed: true,
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [{
        name: 'Chart 1',
        metricsCollapsed: true,
        metrics: [{ id: 'metric-1', displayName: 'Warns', expression: 'TEMP + UNKNOWN', color: '#123456' }],
      }],
      timeRange: { preset: '24h', start: null, end: null },
    }],
  };

  assert.equal(app.persistDashboardEnvironment(env, []), true);
  const stored = JSON.parse(localStorage.getItem('sonde_environments'));
  assert.deepEqual(stored[0].dashboards, [{
    name: 'Persisted',
    variablesCollapsed: true,
    variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
    charts: [{
      name: 'Chart 1',
      metricsCollapsed: true,
      metrics: [{ id: 'metric-1', displayName: 'Warns', expression: 'TEMP + UNKNOWN', color: '#123456' }],
    }],
    timeRange: { preset: '24h', start: null, end: null },
  }]);
});

test('same-name environment import clears stale unsaved dashboard fallback', () => {
  const originalEnv = {
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: app.createDefaultSensorDataPreferences(),
    dashboards: [{ name: 'Persisted', variables: [], charts: [], timeRange: { preset: '24h', start: null, end: null } }],
  };
  localStorage.setItem('sonde_environments', JSON.stringify([originalEnv]));
  app.activateEnvironmentState(originalEnv.name, originalEnv);
  app.APP_DASHBOARD_STATE.unsavedEnvironment = {
    ...originalEnv,
    dashboards: [{ name: 'Unsaved Shadow', variables: [], charts: [], timeRange: { preset: '24h', start: null, end: null } }],
  };
  global.window.confirm = () => true;

  app.handleImportedJson(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    dashboards: [{ name: 'Imported Replacement', variables: [], charts: [], timeRange: { preset: '24h', start: null, end: null } }],
  }));

  const loaded = app.loadActiveEnvironment();
  assert.equal(app.APP_DASHBOARD_STATE.unsavedEnvironment, null);
  assert.equal(loaded.dashboards[0].name, 'Imported Replacement');
});

test('activateEnvironmentState clears session telemetry cache on environment switch', async () => {
  const env = {
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: app.createDefaultSensorDataPreferences(),
  };
  app.activateEnvironmentState(env.name, env);

  await app.fetchActualStateNodes({
    nowFn: () => 1_500,
    queryTableFn: async () => [{
      PartitionKey: 'n:abc123',
      RowKey: 'ffff',
      node_id: 'NODE_001',
      timestamp_ms: '1000',
    }],
  });
  await app.getCachedSensorDataRows(['n:abc123'], 1000, 2000, {}, {
    querySensorDataRangeFn: async () => [{
      PartitionKey: 'n:abc123',
      RowKey: 'row-1',
      timestamp_ms: '1500',
      decoded_readings: '{"temp_mc":25000}',
    }],
  });

  assert.equal(app.SESSION_TELEMETRY_CACHE.actualState.rowsByKey.size, 1);
  assert.equal(app.SESSION_TELEMETRY_CACHE.sensorDataByPartition.size, 1);

  app.activateEnvironmentState('staging', {
    ...env,
    name: 'staging',
    storageAccount: 'stagingstorage',
  });

  assert.equal(app.SESSION_TELEMETRY_CACHE.environmentName, 'staging');
  assert.equal(app.SESSION_TELEMETRY_CACHE.actualState.loaded, false);
  assert.equal(app.SESSION_TELEMETRY_CACHE.actualState.rowsByKey.size, 0);
  assert.equal(app.SESSION_TELEMETRY_CACHE.sensorDataByPartition.size, 0);
});
