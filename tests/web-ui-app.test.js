// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

const test = require('node:test');
const assert = require('node:assert/strict');
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
    addEventListener() {},
    querySelector() { return null; },
    querySelectorAll() { return []; },
    classList: { toggle() {} },
  };
}

global.window = {
  __SONDE_TEST__: true,
  opener: null,
  location: { origin: 'https://example.test', pathname: '/', hash: '' },
};
global.document = {
  addEventListener() {},
  getElementById() { return makeElement(); },
  querySelectorAll() { return []; },
  createElement() { return makeElement(); },
  body: makeElement(),
};
global.localStorage = makeStorage();
global.sessionStorage = makeStorage();
global.crypto = webcrypto;
global.atob = (value) => Buffer.from(value, 'base64').toString('binary');
global.btoa = (value) => Buffer.from(value, 'binary').toString('base64');

const app = require(path.resolve(__dirname, '..', 'deploy', 'web-ui', 'app.js'));

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
      maxPagesPerPartition: Number.POSITIVE_INFINITY,
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
