// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

function makeElement() {
  const listeners = new Map();
  return {
    innerHTML: '',
    textContent: '',
    value: '',
    files: [],
    src: '',
    className: '',
    href: '',
    disabled: false,
    classList: { add() {}, remove() {}, toggle() {} },
    addEventListener(type, listener) {
      if (!listeners.has(type)) {
        listeners.set(type, []);
      }
      listeners.get(type).push(listener);
    },
    dispatch(type, event = {}) {
      for (const listener of listeners.get(type) || []) {
        listener(event);
      }
    },
    appendChild() {},
    click() {},
  };
}

function createTrackedElement() {
  const element = makeElement();
  const classes = new Set();
  element.classList = {
    add(...names) {
      for (const name of names) {
        classes.add(name);
      }
      element.className = Array.from(classes).join(' ');
    },
    remove(...names) {
      for (const name of names) {
        classes.delete(name);
      }
      element.className = Array.from(classes).join(' ');
    },
    toggle(name, force) {
      if (force === undefined ? !classes.has(name) : force) {
        classes.add(name);
      } else {
        classes.delete(name);
      }
      element.className = Array.from(classes).join(' ');
    },
  };
  return element;
}

function createDomFixture() {
  const elements = new Map();
  const ensureElement = (id) => {
    if (!elements.has(id)) {
      elements.set(id, createTrackedElement());
    }
    return elements.get(id);
  };
  const pageStatus = createTrackedElement();
  global.document.getElementById = (id) => ensureElement(id);
  global.document.querySelector = (selector) => (selector === '.dashboard-page-status' ? pageStatus : null);
  return { elements, pageStatus };
}

function buildCacheKey(environment, nodeId, readingType) {
  return JSON.stringify({
    clientId: environment.clientId,
    storageAccount: environment.storageAccount,
    nodeId,
    readingType,
  });
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

test('kiosk tauri config uses kiosk-owned icon assets', () => {
  const tauriConfigPath = path.resolve(__dirname, '..', 'crates', 'sonde-kiosk-ui', 'src-tauri', 'tauri.conf.json');
  const tauriConfig = JSON.parse(fs.readFileSync(tauriConfigPath, 'utf8'));

  assert.deepEqual(tauriConfig.bundle.icon, [
    'icons/icon.png',
    'icons/32x32.png',
    'icons/128x128.png',
    'icons/128x128@2x.png',
    'icons/icon.icns',
    'icons/icon.ico',
  ]);

  for (const iconPath of tauriConfig.bundle.icon) {
    assert.equal(
      fs.existsSync(path.resolve(path.dirname(tauriConfigPath), iconPath)),
      true,
      `missing kiosk icon asset: ${iconPath}`,
    );
  }
});

test('kiosk tauri backend keeps explicit default capability and android launcher assets', () => {
  const srcTauriPath = path.resolve(__dirname, '..', 'crates', 'sonde-kiosk-ui', 'src-tauri');
  const capabilityPath = path.join(srcTauriPath, 'capabilities', 'default.json');
  const capability = JSON.parse(fs.readFileSync(capabilityPath, 'utf8'));

  assert.equal(capability.identifier, 'default');
  assert.deepEqual(capability.windows, ['main']);
  assert.deepEqual(capability.permissions, ['core:default']);

  const androidLauncherPath = path.join(
    srcTauriPath,
    'icons',
    'android',
    'mipmap-anydpi-v26',
    'ic_launcher.xml',
  );
  assert.equal(fs.existsSync(androidLauncherPath), true);
});

test('kiosk android manifest requests internet access', () => {
  const manifestPath = path.resolve(__dirname, '..', 'crates', 'sonde-kiosk-ui', 'android', 'AndroidManifest.xml');
  const manifest = fs.readFileSync(manifestPath, 'utf8');

  assert.match(manifest, /android\.permission\.INTERNET/);
  assert.match(manifest, /androidx\.core\.content\.FileProvider/);
});

test('kiosk android startup initializes the keyring ndk context', () => {
  const mainActivityPath = path.resolve(
    __dirname,
    '..',
    'crates',
    'sonde-kiosk-ui',
    'android',
    'src',
    'main',
    'java',
    'com',
    'sonde',
    'kiosk',
    'MainActivity.kt',
  );
  const keyringPath = path.resolve(
    __dirname,
    '..',
    'crates',
    'sonde-kiosk-ui',
    'android',
    'src',
    'main',
    'java',
    'io',
    'crates',
    'keyring',
    'Keyring.kt',
  );
  const mainActivity = fs.readFileSync(mainActivityPath, 'utf8');
  const keyringSource = fs.readFileSync(keyringPath, 'utf8');

  assert.match(mainActivity, /class MainActivity : TauriActivity\(\)/);
  assert.match(mainActivity, /Keyring\.initializeNdkContext\(applicationContext\)/);
  assert.match(keyringSource, /System\.loadLibrary\("sonde_kiosk_ui_backend"\)/);
  assert.match(keyringSource, /external fun initializeNdkContext\(context: Context\)/);
});

test('android workflow builds the kiosk tauri app', () => {
  const workflowPath = path.resolve(__dirname, '..', '.github', 'workflows', 'tauri-android.yml');
  const workflow = fs.readFileSync(workflowPath, 'utf8');
  const debugJobMatch = workflow.match(/build-kiosk-debug-apk:[\s\S]*?(?=\n  build-kiosk-release-apk:)/);
  const releaseJobMatch = workflow.match(/build-kiosk-release-apk:[\s\S]*$/);

  assert.match(workflow, /crates\/sonde-kiosk-ui\/\*\*/);
  assert.ok(debugJobMatch, 'missing build-kiosk-debug-apk job');
  assert.ok(releaseJobMatch, 'missing build-kiosk-release-apk job');

  const debugJob = debugJobMatch[0];
  const releaseJob = releaseJobMatch[0];

  assert.match(debugJob, /working-directory: crates\/sonde-kiosk-ui/);
  assert.match(debugJob, /cargo tauri android init/);
  assert.match(debugJob, /crates\/sonde-kiosk-ui\/android\/AndroidManifest\.xml/);
  assert.match(debugJob, /crates\/sonde-kiosk-ui\/android\/src\/main\/java/);
  assert.match(debugJob, /cargo tauri android build --debug/);
  assert.match(debugJob, /crates\/sonde-kiosk-ui\/src-tauri\/icons\/android/);
  assert.match(debugJob, /androidx\.security:security-crypto:1\.1\.0-alpha06/);

  assert.match(releaseJob, /working-directory: crates\/sonde-kiosk-ui/);
  assert.match(releaseJob, /cargo tauri android init/);
  assert.match(releaseJob, /crates\/sonde-kiosk-ui\/android\/AndroidManifest\.xml/);
  assert.match(releaseJob, /crates\/sonde-kiosk-ui\/android\/src\/main\/java/);
  assert.match(releaseJob, /cargo tauri android build/);
  assert.match(releaseJob, /sonde-kiosk-android-release/);
});

test('describeError surfaces string rejections from the Tauri bridge', () => {
  assert.equal(kiosk.describeError('bridge failure'), 'bridge failure');
  assert.equal(kiosk.describeError(new Error('typed failure')), 'typed failure');
});

test('kiosk tauri backend defines the mobile entry point required for Android builds', () => {
  const backendPath = path.resolve(__dirname, '..', 'crates', 'sonde-kiosk-ui', 'src-tauri', 'src', 'lib.rs');
  const backendSource = fs.readFileSync(backendPath, 'utf8');

  assert.match(backendSource, /#\[cfg\(mobile\)\]\s*#\[tauri::mobile_entry_point\]\s*fn main\(\)\s*\{\s*run\(\);/s);
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

test('validateImportedEnvironmentJson rejects imports missing function app metadata', () => {
  assert.throws(() => kiosk.validateImportedEnvironmentJson(JSON.stringify({
    version: 1,
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    dashboards: [],
  }), runtime), /Function App Name is required/);
});

test('renderDashboardFrame renders only the active kiosk chart surface', () => {
  const html = kiosk.renderDashboardFrame(runtime, runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [
      {
        name: 'Overview',
        variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
        charts: [{
          name: 'Primary',
          metrics: [{ id: 'metric-1', displayName: 'Temperature', expression: 'TEMP / 1000', color: '#123456' }],
        }, {
          name: 'Secondary',
          metrics: [{ id: 'metric-2', displayName: 'Humidity', expression: 'TEMP / 2000', color: '#654321' }],
        }],
        timeRange: { preset: '24h', start: null, end: null },
      },
      {
        name: 'Diagnostics',
        variables: [{ name: 'VBAT', nodeId: 'NODE_001', readingType: 'vbat_mv' }],
        charts: [{
          name: 'Battery',
          metrics: [{ id: 'metric-3', displayName: 'Battery', expression: 'VBAT / 1000', color: '#00ff00' }],
        }],
        timeRange: { preset: '24h', start: null, end: null },
      },
    ],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  }), 0, 1);

  assert.match(html, /kiosk-chart-page/);
  assert.match(html, /id="metric-chart-0"/);
  assert.doesNotMatch(html, /variables-table/);
  assert.doesNotMatch(html, /Last 24 Hours/);
  assert.doesNotMatch(html, /Secondary/);
  assert.doesNotMatch(html, /Battery/);
});

test('buildChartPages follows imported dashboard and chart order', () => {
  const environment = buildEnvironment({
    dashboards: [
      {
        name: 'Overview',
        variables: [],
        charts: [
          { name: 'First', metrics: [{ id: 'm1', expression: '1', color: '#111111' }] },
          { name: 'Second', metrics: [{ id: 'm2', expression: '1', color: '#222222' }] },
        ],
        timeRange: { preset: '24h', start: null, end: null },
      },
      {
        name: 'Diagnostics',
        variables: [],
        charts: [
          { name: 'Third', metrics: [{ id: 'm3', expression: '1', color: '#333333' }] },
        ],
        timeRange: { preset: '24h', start: null, end: null },
      },
    ],
  });

  assert.deepEqual(
    kiosk.buildChartPages(environment).map((page) => [page.dashboardIndex, page.chartIndex, page.dashboard.name, page.chart.name]),
    [
      [0, 0, 'Overview', 'First'],
      [0, 1, 'Overview', 'Second'],
      [1, 0, 'Diagnostics', 'Third'],
    ],
  );
});

test('convertCachedPointsToRuntimeTimeSeries maps timestampMs points into runtime timestamps', () => {
  assert.deepEqual(
    kiosk.convertCachedPointsToRuntimeTimeSeries([
      { timestampMs: 8_000, value: 20.25 },
      { timestampMs: 'bad', value: 5 },
    ]),
    [{ timestamp: 8_000, value: 20.25 }],
  );
});

test('buildEnvironmentRefreshRequest uses the largest imported dashboard time range', () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [
      {
        name: 'Overview',
        variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
        charts: [],
        timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
      },
      {
        name: 'Detail',
        variables: [{ name: 'HUMID', nodeId: 'NODE_002', readingType: 'rh_mpermille' }],
        charts: [],
        timeRange: { preset: 'custom', start: 500, end: 15_000 },
      },
    ],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  const request = kiosk.buildEnvironmentRefreshRequest(environment, runtime, 50_000);
  assert.equal(request.startMs, 500);
  assert.equal(request.endMs, 15_000);
  assert.equal(request.fullStartMs, 500);
  assert.equal(request.fullEndMs, 15_000);
  assert.equal(request.incremental, false);
  assert.deepEqual(request.variables, [
    { nodeId: 'NODE_001', readingType: 'temp_mc' },
    { nodeId: 'NODE_002', readingType: 'rh_mpermille' },
  ]);
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
    variables: [{ nodeId: 'NODE_001', readingType: 'temp_mc' }],
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
  kiosk.cacheTelemetryRefreshResponse(environment, {
    startMs: 1_000,
    endMs: 9_000,
    variables: [{ nodeId: 'NODE_001', readingType: 'temp_mc' }],
  }, {
    refreshedAtMs: 2_000,
    series: [{ nodeId: 'NODE_001', points: [{ timestampMs: 2_000, value: 1 }] }],
  });

  assert.equal(kiosk.APP_STATE.telemetryCache.size, 1);
  assert.deepEqual(
    kiosk.APP_STATE.telemetryCache.get(buildCacheKey(environment, 'NODE_001', 'temp_mc')).points,
    [],
  );
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
  const cachedSeriesCount = kiosk.cacheTelemetryRefreshResponse(environment, {
    startMs: 1_000,
    endMs: 9_000,
    variables: [{ nodeId: 'NODE_001', readingType: 'temp_mc' }],
  }, {
    refreshedAtMs: 2_000,
    series: [
      { nodeId: 'NODE_001', readingType: 'temp_mc', points: [{ timestampMs: 2_000, value: 1 }] },
      { nodeId: 'NODE_002', points: [{ timestampMs: 2_000, value: 1 }] },
    ],
  });

  assert.equal(cachedSeriesCount, 1);
});

test('cacheTelemetryRefreshResponse ignores unrequested telemetry series', () => {
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
  const cachedSeriesCount = kiosk.cacheTelemetryRefreshResponse(environment, {
    startMs: 1_000,
    endMs: 9_000,
    variables: [{ nodeId: 'NODE_001', readingType: 'temp_mc' }],
  }, {
    refreshedAtMs: 2_000,
    series: [
      { nodeId: 'NODE_001', readingType: 'temp_mc', points: [{ timestampMs: 2_000, value: 1 }] },
      { nodeId: 'NODE_999', readingType: 'temp_mc', points: [{ timestampMs: 2_000, value: 9 }] },
    ],
  });

  assert.equal(cachedSeriesCount, 1);
  assert.equal(kiosk.APP_STATE.telemetryCache.size, 1);
  assert.equal(kiosk.APP_STATE.telemetryCache.has(buildCacheKey(environment, 'NODE_001', 'temp_mc')), true);
  assert.equal(kiosk.APP_STATE.telemetryCache.has(buildCacheKey(environment, 'NODE_999', 'temp_mc')), false);
});

test('cacheTelemetryRefreshResponse preserves the active refresh when it exceeds the base series cap', () => {
  const variables = Array.from({ length: 129 }, (_unused, index) => ({
    name: `TEMP_${index}`,
    nodeId: `NODE_${index.toString().padStart(3, '0')}`,
    readingType: 'temp_mc',
  }));
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [{
      name: 'Overview',
      variables,
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  kiosk.APP_STATE.telemetryCache = new Map([[
    buildCacheKey(environment, 'STALE_NODE', 'temp_mc'),
    {
      points: [{ timestampMs: 100, value: 1 }],
      coverageStartMs: 0,
      coverageEndMs: 100,
      refreshedAtMs: 100,
      lastAccessedAtMs: 100,
    },
  ]]);

  const cachedSeriesCount = kiosk.cacheTelemetryRefreshResponse(environment, {
    startMs: 1_000,
    endMs: 9_000,
    variables: variables.map((variable) => ({
      nodeId: variable.nodeId,
      readingType: variable.readingType,
    })),
  }, {
    refreshedAtMs: 9_000,
    series: variables.map((variable, index) => ({
      nodeId: variable.nodeId,
      readingType: variable.readingType,
      points: [{ timestampMs: 8_000 + index, value: index }],
    })),
  });

  assert.equal(cachedSeriesCount, 129);
  assert.equal(kiosk.APP_STATE.telemetryCache.size, 129);
  assert.equal(kiosk.APP_STATE.telemetryCache.has(buildCacheKey(environment, 'STALE_NODE', 'temp_mc')), false);
  assert.equal(kiosk.APP_STATE.telemetryCache.has(buildCacheKey(environment, 'NODE_000', 'temp_mc')), true);
  assert.equal(kiosk.APP_STATE.telemetryCache.has(buildCacheKey(environment, 'NODE_128', 'temp_mc')), true);
});

test('cacheTelemetryRefreshResponse appends incremental telemetry without dropping cached history', () => {
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  });
  kiosk.APP_STATE.telemetryCache = new Map([[
    buildCacheKey(environment, 'NODE_001', 'temp_mc'),
    {
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: 9_000,
      lastAccessedAtMs: 9_000,
    },
  ]]);

  kiosk.cacheTelemetryRefreshResponse(environment, {
    startMs: 9_000,
    endMs: 10_000,
    fullStartMs: 1_000,
    fullEndMs: 10_000,
    incremental: true,
    variables: [{ nodeId: 'NODE_001', readingType: 'temp_mc' }],
  }, {
    refreshedAtMs: 10_000,
    series: [{
      nodeId: 'NODE_001',
      readingType: 'temp_mc',
      points: [{ timestampMs: 9_500, value: 20.5 }],
    }],
  });

  const cached = kiosk.APP_STATE.telemetryCache.get(buildCacheKey(environment, 'NODE_001', 'temp_mc'));
  assert.deepEqual(cached.points, [
    { timestampMs: 8_000, value: 20.25 },
    { timestampMs: 9_500, value: 20.5 },
  ]);
  assert.equal(cached.coverageStartMs, 1_000);
  assert.equal(cached.coverageEndMs, 10_000);
});

test('buildEnvironmentRefreshRequest de-duplicates sources without delimiter collisions', () => {
  const environment = runtime.normalizeEnvironmentRecord({
    name: 'prod',
    clientId: '11111111-1111-1111-1111-111111111111',
    tenantId: '22222222-2222-2222-2222-222222222222',
    storageAccount: 'prodstorage',
    functionAppName: 'prod-func',
    sensorData: kiosk.createDefaultSensorDataPreferences(),
    dashboards: [
      {
        name: 'Overview',
        variables: [
          { name: 'A', nodeId: 'node\none', readingType: 'temp' },
          { name: 'B', nodeId: 'node', readingType: 'one\ntemp' },
        ],
        charts: [],
        timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
      },
      {
        name: 'Detail',
        variables: [
          { name: 'C', nodeId: 'node\none', readingType: 'temp' },
          { name: 'D', nodeId: 'node', readingType: 'one\ntemp' },
        ],
        charts: [],
        timeRange: { preset: 'custom', start: 2_000, end: 8_000 },
      },
    ],
  }, {
    sanitizeSensorDataPreferences: (preferences) => preferences ?? kiosk.createDefaultSensorDataPreferences(),
    validateExpressionFn: runtime.validateExpression,
  });

  const request = kiosk.buildEnvironmentRefreshRequest(environment, runtime, 50_000);
  assert.deepEqual(request.variables, [
    { nodeId: 'node\none', readingType: 'temp' },
    { nodeId: 'node', readingType: 'one\ntemp' },
  ]);
});

test('buildEnvironmentRefreshRequest uses incremental refresh bounds when cache coverage exists', () => {
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }, {
      name: 'Detail',
      variables: [{ name: 'HUMID', nodeId: 'NODE_002', readingType: 'rh_mpermille' }],
      charts: [],
      timeRange: { preset: 'custom', start: 2_000, end: 7_000 },
    }],
  });
  kiosk.APP_STATE.telemetryCache = new Map([
    [buildCacheKey(environment, 'NODE_001', 'temp_mc'), {
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: 9_000,
      lastAccessedAtMs: 9_000,
    }],
    [buildCacheKey(environment, 'NODE_002', 'rh_mpermille'), {
      points: [{ timestampMs: 8_500, value: 60 }],
      coverageStartMs: 1_000,
      coverageEndMs: 8_500,
      refreshedAtMs: 8_500,
      lastAccessedAtMs: 8_500,
    }],
  ]);

  const request = kiosk.buildEnvironmentRefreshRequest(environment, runtime, 9_500);
  assert.equal(request.incremental, true);
  assert.equal(request.startMs, 8_500);
  assert.equal(request.endMs, 9_000);
});

test('buildEnvironmentRefreshRequest falls back to a full refresh when cached coverage ends before the current horizon', () => {
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 4_000, end: 9_000 },
    }],
  });
  kiosk.APP_STATE.telemetryCache = new Map([[
    buildCacheKey(environment, 'NODE_001', 'temp_mc'),
    {
      points: [{ timestampMs: 3_500, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 3_500,
      refreshedAtMs: 3_500,
      lastAccessedAtMs: 3_500,
    },
  ]]);

  const request = kiosk.buildEnvironmentRefreshRequest(environment, runtime, 9_500);
  assert.equal(request.incremental, false);
  assert.equal(request.startMs, 4_000);
  assert.equal(request.fullStartMs, 4_000);
  assert.equal(request.endMs, 9_000);
});

test('setTelemetryNotice preserves muted dashboard status styling for info notices', () => {
  const dashboardStatus = makeElement();
  const statusOverlay = makeElement();
  global.document.getElementById = (id) => {
    if (id === 'dashboard-status') return dashboardStatus;
    if (id === 'dashboard-status-overlay') return statusOverlay;
    return makeElement();
  };

  kiosk.setTelemetryNotice('Waiting for live telemetry refresh.', 'info');

  assert.equal(dashboardStatus.className, 'status-pill');
  assert.equal(statusOverlay.className, 'dashboard-status-overlay');
  assert.equal(statusOverlay.textContent, 'Waiting for live telemetry refresh.');
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

  kiosk.APP_STATE.runtime = {
    ...runtime,
    renderMetricCharts: async () => {},
  };
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache.clear();

  await kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async (request) => {
      assert.equal(request.storageAccount, 'prodstorage');
      assert.deepEqual(request.variables, [{ nodeId: 'NODE_001', readingType: 'temp_mc' }]);
      return {
        complete: true,
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

test('triggerDashboardRefresh accepts empty telemetry series for a valid no-data refresh', async () => {
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

  kiosk.APP_STATE.runtime = {
    ...runtime,
    renderMetricCharts: async () => {},
  };
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache.clear();

  await kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async () => ({
      complete: true,
      refreshedAtMs: 9_000,
      series: [{
        nodeId: 'NODE_001',
        readingType: 'temp_mc',
        points: [],
      }],
    }),
  });

  const cacheEntry = kiosk.APP_STATE.telemetryCache.get(buildCacheKey(environment, 'NODE_001', 'temp_mc'));
  assert.deepEqual(cacheEntry.points, []);
  assert.equal(cacheEntry.coverageStartMs, 1_000);
  assert.equal(cacheEntry.coverageEndMs, 9_000);
  assert.doesNotMatch(kiosk.APP_STATE.telemetryNotice, /no usable series/i);
});

test('triggerDashboardRefresh marks partial telemetry refreshes without claiming full coverage', async () => {
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

  kiosk.APP_STATE.runtime = {
    ...runtime,
    renderMetricCharts: async () => {},
  };
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache.clear();

  await kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async () => ({
      complete: false,
      refreshedAtMs: 9_000,
      series: [{
        nodeId: 'NODE_001',
        readingType: 'temp_mc',
        points: [{ timestampMs: 8_000, value: 20.25 }],
      }],
    }),
  });

  const [[cacheKey, cacheEntry]] = Array.from(kiosk.APP_STATE.telemetryCache.entries());
  assert.match(cacheKey, /NODE_001/);
  assert.equal(cacheEntry.coverageStartMs, null);
  assert.equal(cacheEntry.coverageEndMs, null);
  assert.match(kiosk.APP_STATE.telemetryNotice, /Partial live data refreshed/);
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

  kiosk.APP_STATE.runtime = {
    ...runtime,
    renderMetricCharts: async () => {},
  };
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

test('triggerDashboardRefresh reports cached offline status when live refresh fails but cached data exists', async () => {
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
  kiosk.APP_STATE.telemetryCache = new Map([[
    buildCacheKey(environment, 'NODE_001', 'temp_mc'),
    {
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: 9_000,
      lastAccessedAtMs: 9_000,
    },
  ]]);

  await kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async () => {
      throw new Error('network down');
    },
  });

  assert.match(kiosk.APP_STATE.telemetryNotice, /Showing cached data\. Live refresh unavailable: network down/);
});

test('triggerDashboardRefresh preserves warm cached telemetry when a full refresh returns no series', async () => {
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }, {
      name: 'Detail',
      variables: [{ name: 'HUMID', nodeId: 'NODE_002', readingType: 'rh_mpermille' }],
      charts: [],
      timeRange: { preset: 'custom', start: 2_000, end: 7_000 },
    }],
  });
  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache = new Map([[
    buildCacheKey(environment, 'NODE_001', 'temp_mc'),
    {
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: 9_000,
      lastAccessedAtMs: 9_000,
    },
  ]]);

  await kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async (request) => {
      assert.equal(request.incremental, false);
      return {
        complete: true,
        refreshedAtMs: 9_000,
        series: [],
      };
    },
  });

  const cached = kiosk.buildCachedVariableData(runtime, environment, environment.dashboards[0], 9_000);
  assert.deepEqual(cached.TEMP, [{ timestampMs: 8_000, value: 20.25 }]);
  assert.match(kiosk.APP_STATE.telemetryNotice, /Live data refreshed at/);
});

test('triggerDashboardRefresh refreshes cache metadata when a full refresh returns no series', async () => {
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  });
  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache = new Map([[
    buildCacheKey(environment, 'NODE_001', 'temp_mc'),
    {
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_500,
      coverageEndMs: 8_500,
      refreshedAtMs: 8_500,
      lastAccessedAtMs: 8_500,
    },
  ]]);

  await kiosk.triggerDashboardRefresh('manual', {
    nowFn: () => 9_000,
    fetchDashboardVariableDataFn: async (request) => {
      assert.equal(request.incremental, false);
      return {
        complete: true,
        refreshedAtMs: 9_000,
        series: [],
      };
    },
  });

  const cacheEntry = kiosk.APP_STATE.telemetryCache.get(buildCacheKey(environment, 'NODE_001', 'temp_mc'));
  assert.deepEqual(cacheEntry.points, [{ timestampMs: 8_000, value: 20.25 }]);
  assert.equal(cacheEntry.coverageStartMs, 1_000);
  assert.equal(cacheEntry.coverageEndMs, 9_000);
  assert.equal(cacheEntry.refreshedAtMs, 9_000);
  assert.equal(Number.isFinite(cacheEntry.lastAccessedAtMs), true);
  assert.ok(cacheEntry.lastAccessedAtMs > 8_500);
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

test('background refresh keeps the prior status text on success', async () => {
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  });
  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.telemetryCache = new Map([[
    buildCacheKey(environment, 'NODE_001', 'temp_mc'),
    {
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: 9_000,
      lastAccessedAtMs: 9_000,
    },
  ]]);
  kiosk.setTelemetryNotice('Showing cached data.', 'info');

  await kiosk.triggerDashboardRefresh('background', {
    nowFn: () => 10_000,
    fetchDashboardVariableDataFn: async (request) => {
      assert.equal(request.incremental, true);
      return {
        complete: true,
        refreshedAtMs: 10_000,
        series: [{
          nodeId: 'NODE_001',
          readingType: 'temp_mc',
          points: [{ timestampMs: 9_500, value: 20.5 }],
        }],
      };
    },
  });

  assert.equal(kiosk.APP_STATE.telemetryNotice, 'Showing cached data.');
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
        series: [{ nodeId: 'NODE_001', readingType: 'temp_mc', points: [{ timestampMs: 8_000, value: 20.25 }] }],
      };
    },
  };

  const firstRefresh = kiosk.triggerDashboardRefresh('background', deps);
  const secondRefresh = kiosk.triggerDashboardRefresh('background', deps);
  assert.equal(callCount, 1);

  releaseRefresh();
  await Promise.all([firstRefresh, secondRefresh]);
});

test('renderActiveDashboard keeps the active dashboard name visible in the header title', async () => {
  const { elements } = createDomFixture();
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [],
      charts: [{ name: 'Primary', metrics: [{ id: 'm1', expression: '1', color: '#111111' }] }],
      timeRange: { preset: '24h', start: null, end: null },
    }],
  });
  elements.set('dashboard-page-host', createTrackedElement());
  elements.set('dashboard-status', createTrackedElement());
  elements.set('dashboard-overlay', createTrackedElement());
  kiosk.APP_STATE.runtime = {
    ...runtime,
    renderMetricCharts: async () => {},
  };
  kiosk.APP_STATE.activeEnvironment = environment;
  kiosk.APP_STATE.activeDashboardIndex = 0;
  kiosk.APP_STATE.activeChartIndex = 0;

  await kiosk.renderActiveDashboard({ nowFn: () => 10_000 });

  assert.equal(elements.get('dashboard-overlay').textContent, 'Overview');
  assert.doesNotMatch(elements.get('dashboard-overlay').className, /hidden/);
});

test('telemetry cache JSON round-trips through parse and serialize helpers', () => {
  kiosk.APP_STATE.telemetryCache = new Map([
    ['cache-key', {
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: 9_000,
      lastAccessedAtMs: 8_500,
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
      lastAccessedAtMs: 8_500,
    },
  ]]);
});

test('enforceTelemetryCacheBounds trims old points and evicts least recently used series', () => {
  kiosk.APP_STATE.telemetryCache = new Map();
  for (let index = 0; index < 130; index += 1) {
    kiosk.APP_STATE.telemetryCache.set(`NODE_${index}\ntemp_mc`, {
      points: Array.from({ length: 2100 }, (_unused, pointIndex) => ({
        timestampMs: pointIndex,
        value: pointIndex,
      })),
      coverageStartMs: 0,
      coverageEndMs: 2_099,
      refreshedAtMs: index,
      lastAccessedAtMs: index,
    });
  }

  kiosk.enforceTelemetryCacheBounds();

  assert.equal(kiosk.APP_STATE.telemetryCache.size, 128);
  assert.equal(kiosk.APP_STATE.telemetryCache.has('NODE_0\ntemp_mc'), false);
  assert.equal(kiosk.APP_STATE.telemetryCache.has('NODE_1\ntemp_mc'), false);
  const retainedEntry = kiosk.APP_STATE.telemetryCache.get('NODE_129\ntemp_mc');
  assert.equal(retainedEntry.points.length, 2048);
  assert.deepEqual(retainedEntry.points[0], { timestampMs: 52, value: 52 });
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

test('initKioskApp falls back to cached dashboard mode when startup sign-in fails', async () => {
  createDomFixture();
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  });

  const persistedCache = JSON.stringify({
    version: 1,
    entries: [{
      key: buildCacheKey(environment, 'NODE_001', 'temp_mc'),
      points: [{ timestampMs: 8_000, value: 20.25 }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: 9_000,
      lastAccessedAtMs: 9_000,
    }],
  });
  const invoked = [];

  await kiosk.initKioskApp({
    runtime,
    invoke: async (command) => {
      invoked.push(command);
      if (command === 'get_telemetry_cache_json') {
        return persistedCache;
      }
      if (command === 'get_environment_json') {
        return JSON.stringify(environment);
      }
      if (command === 'get_kiosk_identity_summary') {
        return {
          tenantId: environment.tenantId,
          sharedAppClientId: environment.clientId,
          renewalRequired: false,
        };
      }
      if (command === 'sign_in_kiosk_application') {
        throw new Error('network down');
      }
      if (command === 'fetch_dashboard_variable_data') {
        throw new Error('network down');
      }
      return null;
    },
    setIntervalFn: () => 42,
    clearIntervalFn() {},
  });

  assert.equal(kiosk.APP_STATE.activeEnvironment?.name, 'prod');
  assert.match(kiosk.APP_STATE.telemetryNotice, /Showing cached data\. Live refresh unavailable: network down/);
  assert.deepEqual(invoked.slice(0, 3), [
    'get_telemetry_cache_json',
    'get_environment_json',
    'get_kiosk_identity_summary',
  ]);
});

test('initKioskApp normalizes non-Error sign-in failures when cached startup fallback is unavailable', async () => {
  createDomFixture();
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  });

  await kiosk.initKioskApp({
    runtime,
    invoke: async (command) => {
      if (command === 'get_telemetry_cache_json') {
        return JSON.stringify({ version: 1, entries: [] });
      }
      if (command === 'get_environment_json') {
        return JSON.stringify(environment);
      }
      if (command === 'get_kiosk_identity_summary') {
        return {
          tenantId: environment.tenantId,
          sharedAppClientId: environment.clientId,
          renewalRequired: false,
        };
      }
      if (command === 'sign_in_kiosk_application') {
        throw 'network down';
      }
      return null;
    },
    setIntervalFn: () => 42,
    clearIntervalFn() {},
  });

  assert.match(kiosk.APP_STATE.setupStatusMessage, /Application sign-in failed: network down/);
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

test('setup auth flow uses renewal when identity already exists', async () => {
  const calls = [];
  kiosk.APP_STATE.activeEnvironment = buildEnvironment();
  kiosk.APP_STATE.identitySummary = {
    tenantId: '22222222-2222-2222-2222-222222222222',
    sharedAppClientId: '11111111-1111-1111-1111-111111111111',
    renewalRequired: true,
  };
  kiosk.APP_STATE.deviceCodeSession = null;

  await kiosk.beginDeviceCodeFlow('renew', {
    invoke: async (command, payload) => {
      calls.push({ command, payload });
      if (command === 'start_device_code_sign_in') {
        return {
          sessionId: 'renew-1',
          purpose: 'renew',
          userCode: 'ABCDEF',
          verificationUri: 'https://microsoft.com/devicelogin',
          verificationUriComplete: null,
          pollIntervalSeconds: 0,
          expiresAtMs: 9_999,
          message: 'Use code ABCDEF',
        };
      }
      if (command === 'poll_device_code_sign_in') {
        return { status: 'error', message: 'stop after start' };
      }
      throw new Error(`unexpected command ${command}`);
    },
    setTimeoutFn: (fn) => fn(),
  }).catch((error) => {
    assert.match(error.message, /stop after start/i);
  });

  assert.equal(calls[0].command, 'start_device_code_sign_in');
  assert.equal(calls[0].payload.request.purpose, 'renew');
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

test('pollUntilDeviceCodeComplete reports reset remote cleanup results and clears kiosk state', async () => {
  const { elements } = createDomFixture();
  kiosk.APP_STATE.activeEnvironment = buildEnvironment();
  kiosk.APP_STATE.identitySummary = { sharedAppClientId: 'old-client' };
  kiosk.APP_STATE.telemetryCache = new Map([[buildCacheKey(buildEnvironment(), 'NODE_001', 'temp_mc'), {
    points: [{ timestampMs: 8_000, value: 20.25 }],
    coverageStartMs: 1_000,
    coverageEndMs: 9_000,
    refreshedAtMs: 9_000,
    lastAccessedAtMs: 9_000,
  }]]);
  kiosk.APP_STATE.deviceCodeSession = {
    sessionId: 'session-reset',
    userCode: 'RESET01',
    verificationUri: 'https://microsoft.com/devicelogin',
    verificationUriComplete: null,
    pollIntervalSeconds: 0,
    message: 'Use code RESET01',
  };

  await kiosk.pollUntilDeviceCodeComplete('reset', {
    invoke: async (command, payload) => {
      if (command === 'poll_device_code_sign_in') {
        return { status: 'complete', message: 'Signed in.' };
      }
      if (command === 'reset_kiosk_app_state') {
        assert.equal(payload.request.sessionId, 'session-reset');
        return {
          message: 'Kiosk state cleared and the remote kiosk certificate was removed.',
          remoteCleanupStatus: 'removed',
        };
      }
      throw new Error(`unexpected command ${command}`);
    },
  });

  assert.equal(kiosk.APP_STATE.identitySummary, null);
  assert.equal(kiosk.APP_STATE.activeEnvironment, null);
  assert.equal(kiosk.APP_STATE.telemetryCache.size, 0);
  assert.match(elements.get('setup-status').textContent, /remote kiosk certificate was removed/i);
});

test('pollUntilDeviceCodeComplete surfaces certificate renewal failures without continuing', async () => {
  kiosk.APP_STATE.activeEnvironment = buildEnvironment();
  kiosk.APP_STATE.deviceCodeSession = {
    sessionId: 'session-renew-fail',
    userCode: 'RENEW1',
    verificationUri: 'https://microsoft.com/devicelogin',
    verificationUriComplete: null,
    pollIntervalSeconds: 0,
    message: 'Use code RENEW1',
  };

  await assert.rejects(
    kiosk.pollUntilDeviceCodeComplete('renew', {
      invoke: async (command) => {
        if (command === 'poll_device_code_sign_in') {
          return { status: 'complete', message: 'Signed in.' };
        }
        if (command === 'renew_kiosk_certificate') {
          throw new Error('permission denied to rotate credentials');
        }
        if (command === 'sign_in_kiosk_application') {
          throw new Error('should not continue to application sign-in');
        }
        throw new Error(`unexpected command ${command}`);
      },
    }),
    /permission denied to rotate credentials/i,
  );
});

test('setup, renewal, and reset permission failures remain actionable', async () => {
  const permissionError = 'permission denied on shared app credentials';
  const { elements } = createDomFixture();
  kiosk.APP_STATE.runtime = runtime;
  kiosk.APP_STATE.activeEnvironment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [],
      charts: [],
      timeRange: { preset: '24h', start: null, end: null },
    }],
  });

  kiosk.APP_STATE.deviceCodeSession = {
    sessionId: 'session-initial',
    userCode: 'INIT01',
    verificationUri: 'https://microsoft.com/devicelogin',
    verificationUriComplete: null,
    pollIntervalSeconds: 0,
    message: 'Use code INIT01',
  };
  await assert.rejects(
    kiosk.pollUntilDeviceCodeComplete('initial', {
      invoke: async (command) => {
        if (command === 'poll_device_code_sign_in') {
          return { status: 'complete', message: 'Signed in.' };
        }
        if (command === 'complete_kiosk_setup') {
          throw new Error(permissionError);
        }
        throw new Error(`unexpected command ${command}`);
      },
    }),
    /permission denied on shared app credentials/i,
  );

  kiosk.showSetupMode(`Kiosk setup failed: ${permissionError}`);
  assert.match(elements.get('setup-status').textContent, /permission denied on shared app credentials/i);

  kiosk.showSetupMode(`Certificate renewal failed: ${permissionError}`);
  assert.match(elements.get('setup-status').textContent, /Certificate renewal failed: permission denied on shared app credentials/i);

  kiosk.showSetupMode(`Kiosk state cleared. Reset fallback detail: ${permissionError}`, { clearEnvironment: true });
  assert.match(elements.get('setup-status').textContent, /Reset fallback detail: permission denied on shared app credentials/i);
});

test('operator controls require a long press and stay hidden on a short press', async () => {
  const { elements } = createDomFixture();
  const previousSetTimeout = global.setTimeout;
  const previousClearTimeout = global.clearTimeout;
  const pendingTimeouts = [];
  global.setTimeout = (fn) => {
    pendingTimeouts.push(fn);
    return pendingTimeouts.length;
  };
  global.clearTimeout = () => {};
  try {
    elements.set('operator-panel', createTrackedElement());
    elements.get('operator-panel').classList.add('hidden');
    await kiosk.initKioskApp({
      runtime,
      invoke: async (command) => {
        if (command === 'get_telemetry_cache_json') {
          return JSON.stringify({ version: 1, entries: [] });
        }
        return null;
      },
      setIntervalFn: () => 42,
      clearIntervalFn() {},
    });
    const hotspot = elements.get('operator-hotspot');
    const panel = elements.get('operator-panel');
    assert.ok(hotspot, 'expected operator hotspot');
    assert.ok(panel, 'expected operator panel');
    assert.match(panel.className, /hidden/);

    hotspot.dispatch('pointerdown', {});
    hotspot.dispatch('pointerup', {});
    assert.match(panel.className, /hidden/);

    hotspot.dispatch('pointerdown', {});
    pendingTimeouts[pendingTimeouts.length - 1]();
    assert.doesNotMatch(panel.className, /hidden/);
  } finally {
    global.setTimeout = previousSetTimeout;
    global.clearTimeout = previousClearTimeout;
  }
});

test('cache eviction preserves recent dashboard usefulness across restart-style reload', () => {
  const environment = buildEnvironment({
    dashboards: [{
      name: 'Overview',
      variables: [{ name: 'TEMP', nodeId: 'NODE_001', readingType: 'temp_mc' }],
      charts: [],
      timeRange: { preset: 'custom', start: 1_000, end: 9_000 },
    }],
  });
  kiosk.APP_STATE.telemetryCache = new Map();
  for (let index = 0; index < 130; index += 1) {
    kiosk.APP_STATE.telemetryCache.set(buildCacheKey(environment, `NODE_${index}`, 'temp_mc'), {
      points: [{ timestampMs: 8_000 + index, value: index }],
      coverageStartMs: 1_000,
      coverageEndMs: 9_000,
      refreshedAtMs: index,
      lastAccessedAtMs: index,
    });
  }
  kiosk.APP_STATE.telemetryCache.set(buildCacheKey(environment, 'NODE_001', 'temp_mc'), {
    points: [{ timestampMs: 8_000, value: 20.25 }],
    coverageStartMs: 1_000,
    coverageEndMs: 9_000,
    refreshedAtMs: 9_000,
    lastAccessedAtMs: 99_999,
  });

  kiosk.enforceTelemetryCacheBounds({
    protectedKeys: new Set([buildCacheKey(environment, 'NODE_001', 'temp_mc')]),
  });

  const reloaded = kiosk.parseTelemetryCacheJson(kiosk.serializeTelemetryCache());
  kiosk.replaceTelemetryCache(reloaded);
  assert.equal(
    kiosk.hasUsableCachedDashboardData(runtime, environment, environment.dashboards[0], 9_000),
    true,
  );
});

test('pollUntilDeviceCodeComplete falls back to default poll delay when interval is missing', async () => {
  const delays = [];
  kiosk.APP_STATE.deviceCodeSession = {
    sessionId: 'session-456',
    userCode: 'UVWXYZ',
    verificationUri: 'https://microsoft.com/devicelogin',
    verificationUriComplete: null,
    message: 'Use code UVWXYZ',
  };

  let pollCount = 0;
  await kiosk.pollUntilDeviceCodeComplete('initial', {
    setTimeoutFn: (fn, delay) => {
      delays.push(delay);
      fn();
    },
    invoke: async (command) => {
      if (command === 'poll_device_code_sign_in') {
        pollCount += 1;
        if (pollCount === 1) {
          return { status: 'pending', message: 'keep waiting' };
        }
        return { status: 'error', message: 'stop test' };
      }
      throw new Error(`unexpected command ${command}`);
    },
  }).catch((error) => {
    assert.match(error.message, /stop test/i);
  });

  assert.deepEqual(delays, [5000]);
});
