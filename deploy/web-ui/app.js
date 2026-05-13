// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

// 1. Configuration
const CONFIG = {
  msalClientId: '',
  msalAuthority: '',
  storageAccount: '',
  functionAppName: '',
  actualStateTable: 'actualstate',
  desiredStateTable: 'desiredstate',
  programsTable: 'programs',
  programRouteTable: 'programroute',
  refreshIntervalMs: 30000,
};

async function loadConfig() {
  // Try loading from config.json first (deployed config)
  try {
    const resp = await fetch('config.json');
    if (resp.ok) {
      const cfg = await resp.json();
      Object.assign(CONFIG, cfg);
      return;
    }
  } catch { /* fall through to URL params */ }

  // Fall back to URL query parameters (development)
  const params = new URLSearchParams(location.search);
  for (const key of ['msalClientId', 'msalAuthority', 'storageAccount', 'functionAppName']) {
    const val = params.get(key) || params.get(key.replace(/[A-Z]/g, (c) => c.toLowerCase()));
    if (val) CONFIG[key] = val;
  }
}

const STORAGE_SCOPES = ['https://storage.azure.com/.default'];
const TAB_IDS = ['dashboard', 'desired-state', 'programs', 'routes'];
const APP = {
  msalApp: null,
  account: null,
  activeTab: 'dashboard',
  refreshHandle: null,
  refreshToken: 0,
  viewMessage: null,
};

const contentEl = document.getElementById('content');
const authControlsEl = document.getElementById('auth-controls');

// 8. Utility Functions
async function sha256hex(text) {
  const data = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest('SHA-256', data);
  return [...new Uint8Array(digest)].map((value) => value.toString(16).padStart(2, '0')).join('');
}

function truncHash(hex) {
  return hex ? String(hex).slice(0, 8) : '—';
}

function relativeTime(timestampMs) {
  const value = Number(timestampMs);
  if (!Number.isFinite(value) || value <= 0) {
    return '—';
  }

  const diffMs = Date.now() - value;
  const diffSeconds = Math.max(0, Math.floor(diffMs / 1000));
  const steps = [
    ['d', 86400],
    ['h', 3600],
    ['m', 60],
  ];

  for (const [suffix, size] of steps) {
    if (diffSeconds >= size) {
      return `${Math.floor(diffSeconds / size)}${suffix} ago`;
    }
  }
  return `${diffSeconds}s ago`;
}

function randomHex(bytes) {
  const data = new Uint8Array(bytes);
  crypto.getRandomValues(data);
  return [...data].map((value) => value.toString(16).padStart(2, '0')).join('');
}

function escapeHtml(value) {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function showViewMessage(kind, text) {
  APP.viewMessage = { kind, text };
}

function consumeViewMessage() {
  const message = APP.viewMessage;
  APP.viewMessage = null;
  return message;
}

function messageHtml(message) {
  if (!message) {
    return '';
  }
  const cssClass = message.kind === 'success' ? 'success' : 'error';
  return `<div class="alert ${cssClass}">${escapeHtml(message.text)}</div>`;
}

function renderCard(title, innerHtml) {
  const message = consumeViewMessage();
  contentEl.innerHTML = `
    <section class="stack">
      <div class="card stack">
        <div>
          <h1>${escapeHtml(title)}</h1>
        </div>
        ${messageHtml(message)}
        ${innerHtml}
      </div>
    </section>
  `;
}

function renderError(title, error) {
  const text = error instanceof Error ? error.message : String(error);
  renderCard(title, `<div class="alert error">${escapeHtml(text)}</div>`);
}

function clearRefresh() {
  APP.refreshToken += 1;
  if (APP.refreshHandle != null) {
    clearTimeout(APP.refreshHandle);
    APP.refreshHandle = null;
  }
}

function setAutoRefresh(callback) {
  clearRefresh();
  const refreshToken = APP.refreshToken;

  async function tick() {
    try {
      await callback();
    } catch (error) {
      renderError('Refresh failed', error);
    } finally {
      if (APP.refreshToken === refreshToken) {
        APP.refreshHandle = setTimeout(tick, CONFIG.refreshIntervalMs);
      }
    }
  }

  APP.refreshHandle = setTimeout(tick, CONFIG.refreshIntervalMs);
}

function latestByPartition(entities) {
  const grouped = new Map();
  for (const entity of entities) {
    const existing = grouped.get(entity.PartitionKey);
    if (!existing || String(entity.RowKey) < String(existing.RowKey)) {
      grouped.set(entity.PartitionKey, entity);
    }
  }
  return [...grouped.values()];
}

function sortByDateDesc(entities, field) {
  return [...entities].sort((left, right) => String(right[field] ?? '').localeCompare(String(left[field] ?? '')));
}

function requireConfig(key, label) {
  if (!CONFIG[key]) {
    throw new Error(`${label} is not configured. Set it in config.json or pass it as a URL parameter.`);
  }
}

function formatHashCell(hash) {
  if (!hash) {
    return '—';
  }
  return `<code title="${escapeHtml(hash)}">${escapeHtml(truncHash(hash))}</code>`;
}

function parseErrorPayload(payload, fallback) {
  if (!payload) {
    return fallback;
  }
  if (payload instanceof Error) {
    return payload.message || fallback;
  }
  if (typeof payload === 'string') {
    return payload;
  }
  if (payload.error) {
    return typeof payload.error === 'string' ? payload.error : JSON.stringify(payload.error);
  }
  if (payload.message) {
    return payload.message;
  }
  return JSON.stringify(payload);
}

// 2. MSAL Authentication
async function initMsal() {
  if (!window.msal || !CONFIG.msalClientId || !CONFIG.msalAuthority) {
    updateAuthUi();
    return;
  }

  APP.msalApp = new msal.PublicClientApplication({
    auth: {
      clientId: CONFIG.msalClientId,
      authority: CONFIG.msalAuthority,
    },
    cache: {
      cacheLocation: 'sessionStorage',
    },
  });

  try {
    await APP.msalApp.handleRedirectPromise();
  } catch (error) {
    showViewMessage('error', parseErrorPayload(error, 'Authentication initialization failed.'));
  }

  const account = APP.msalApp.getActiveAccount?.() || APP.msalApp.getAllAccounts()[0] || null;
  if (account) {
    APP.account = account;
    APP.msalApp.setActiveAccount?.(account);
  }
  updateAuthUi();
}

async function login() {
  requireConfig('msalClientId', 'MSAL clientId');
  requireConfig('msalAuthority', 'MSAL authority');
  if (!APP.msalApp) {
    throw new Error('MSAL is not available.');
  }

  const result = await APP.msalApp.loginPopup({ scopes: STORAGE_SCOPES });
  APP.account = result.account || APP.msalApp.getAllAccounts()[0] || null;
  APP.msalApp.setActiveAccount?.(APP.account);
  updateAuthUi();
  return APP.account;
}

async function getToken() {
  if (!APP.account) {
    await login();
  }
  if (!APP.msalApp || !APP.account) {
    throw new Error('Sign in is required before calling Azure APIs.');
  }

  try {
    const result = await APP.msalApp.acquireTokenSilent({
      account: APP.account,
      scopes: STORAGE_SCOPES,
    });
    return result.accessToken;
  } catch {
    const result = await APP.msalApp.acquireTokenPopup({
      account: APP.account,
      scopes: STORAGE_SCOPES,
    });
    APP.account = result.account || APP.account;
    APP.msalApp.setActiveAccount?.(APP.account);
    updateAuthUi();
    return result.accessToken;
  }
}

function updateAuthUi() {
  if (!authControlsEl) {
    return;
  }

  if (APP.account) {
    authControlsEl.innerHTML = `
      <div class="kv small">
        <strong>${escapeHtml(APP.account.name || APP.account.username || 'Signed in')}</strong>
        <span class="muted">${escapeHtml(APP.account.username || '')}</span>
      </div>
    `;
    return;
  }

  const configMissing = !CONFIG.msalClientId || !CONFIG.msalAuthority;
  authControlsEl.innerHTML = configMissing
    ? '<span class="muted">Authentication is not configured.</span>'
    : '<button type="button" class="secondary" id="login-button">Sign in</button>';

  const button = document.getElementById('login-button');
  if (button) {
    button.addEventListener('click', async () => {
      try {
        await login();
        await renderActiveTab();
      } catch (error) {
        showViewMessage('error', parseErrorPayload(error, 'Sign-in failed.'));
        await renderActiveTab();
      }
    });
  }
}

function requireAuthenticatedView(title) {
  renderCard(title, '<p class="muted">Sign in to load this view.</p>');
}

// 3. Azure Tables API Helper
function tableBaseUrl(tableName) {
  requireConfig('storageAccount', 'Storage account');
  return `https://${CONFIG.storageAccount}.table.core.windows.net/${tableName}`;
}

function tableQueryUrl(tableName) {
  return `${tableBaseUrl(tableName)}()`;
}

function entityUrl(tableName, partitionKey, rowKey) {
  const encodedPartition = encodeURIComponent(String(partitionKey).replaceAll("'", "''"));
  const encodedRow = encodeURIComponent(String(rowKey).replaceAll("'", "''"));
  return `https://${CONFIG.storageAccount}.table.core.windows.net/${tableName}(PartitionKey='${encodedPartition}',RowKey='${encodedRow}')`;
}

async function fetchJson(url, options) {
  const response = await fetch(url, options);
  const text = await response.text();
  let payload = null;

  if (text) {
    try {
      payload = JSON.parse(text);
    } catch {
      payload = text;
    }
  }

  if (!response.ok) {
    throw new Error(parseErrorPayload(payload, `${response.status} ${response.statusText}`));
  }

  return payload;
}

async function queryTable(tableName, filter) {
  const token = await getToken();
  let allEntities = [];
  let nextPartitionKey = null;
  let nextRowKey = null;
  const maxPages = 10;

  for (let page = 0; page < maxPages; page++) {
    const url = new URL(tableQueryUrl(tableName));
    if (filter) url.searchParams.set('$filter', filter);
    if (nextPartitionKey) {
      url.searchParams.set('NextPartitionKey', nextPartitionKey);
      if (nextRowKey) url.searchParams.set('NextRowKey', nextRowKey);
    }

    const response = await fetch(url.toString(), {
      method: 'GET',
      headers: {
        Accept: 'application/json;odata=nometadata',
        Authorization: `Bearer ${token}`,
        'x-ms-version': '2019-02-02',
      },
    });

    if (!response.ok) {
      const text = await response.text();
      throw new Error(`Table query failed (${response.status}): ${text}`);
    }

    const payload = await response.json();
    if (Array.isArray(payload.value)) {
      allEntities = allEntities.concat(payload.value);
    }

    nextPartitionKey = response.headers.get('x-ms-continuation-NextPartitionKey');
    nextRowKey = response.headers.get('x-ms-continuation-NextRowKey');
    if (!nextPartitionKey) break;
  }

  return allEntities;
}

async function insertEntity(tableName, entity) {
  const token = await getToken();
  return fetchJson(tableBaseUrl(tableName), {
    method: 'POST',
    headers: {
      Accept: 'application/json;odata=nometadata',
      'Content-Type': 'application/json',
      Authorization: `Bearer ${token}`,
      'x-ms-version': '2019-02-02',
    },
    body: JSON.stringify(entity),
  });
}

async function upsertEntity(tableName, partitionKey, rowKey, entity) {
  const token = await getToken();
  return fetchJson(entityUrl(tableName, partitionKey, rowKey), {
    method: 'PUT',
    headers: {
      Accept: 'application/json;odata=nometadata',
      'Content-Type': 'application/json',
      Authorization: `Bearer ${token}`,
      'If-Match': '*',
      'x-ms-version': '2019-02-02',
    },
    body: JSON.stringify(entity),
  });
}

async function listPrograms() {
  return sortByDateDesc(await queryTable(CONFIG.programsTable, "PartitionKey eq 'program'"), 'created_at');
}

// 4. Dashboard Tab
async function renderDashboard() {
  if (!APP.account) {
    requireAuthenticatedView('Dashboard');
    return;
  }

  renderCard('Dashboard', '<p class="muted">Loading dashboard…</p>');

  try {
    const [actualRows, desiredRows] = await Promise.all([
      queryTable(CONFIG.actualStateTable, ''),
      queryTable(CONFIG.desiredStateTable, ''),
    ]);

    const latestActual = latestByPartition(actualRows).sort((left, right) => String(left.node_id || '').localeCompare(String(right.node_id || '')));
    const desiredByPartition = new Map(latestByPartition(desiredRows).map((row) => [row.PartitionKey, row]));

    const rowsHtml = latestActual.map((actual) => {
      const desired = desiredByPartition.get(actual.PartitionKey);
      const desiredProgram = desired?.desired_assigned_program_hash || '';
      const actualProgram = actual.observed_current_program_hash || '';
      const desiredSchedule = desired?.desired_schedule_interval_s;
      const actualSchedule = actual.observed_schedule_interval_s;
      const diverged = (desiredProgram && desiredProgram !== actualProgram)
        || (desiredSchedule != null && desiredSchedule !== actualSchedule);
      const scheduleDisplay = desiredSchedule ?? actualSchedule ?? '—';
      const assignedProgram = desiredProgram || actual.observed_assigned_program_hash || '';
      const scheduleTitle = `Observed: ${actualSchedule ?? '—'} | Desired: ${desiredSchedule ?? '—'}`;
      return `
        <tr>
          <td>${escapeHtml(actual.node_id || '—')}</td>
          <td>${escapeHtml(actual.battery_mv ?? '—')}</td>
          <td>${escapeHtml(actual.firmware_version || '—')}</td>
          <td>${escapeHtml(actual.firmware_abi_version ?? '—')}</td>
          <td title="${escapeHtml(scheduleTitle)}">${escapeHtml(scheduleDisplay)}</td>
          <td>${formatHashCell(actualProgram)}</td>
          <td>${formatHashCell(assignedProgram)}</td>
          <td>${escapeHtml(relativeTime(actual.timestamp_ms))}</td>
          <td><span class="badge ${diverged ? 'warning' : 'success'}">${diverged ? 'Diverged' : 'Aligned'}</span></td>
        </tr>
      `;
    }).join('');

    renderCard('Dashboard', `
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Node ID</th>
              <th>Battery (mV)</th>
              <th>Firmware</th>
              <th>ABI</th>
              <th>Schedule (s)</th>
              <th>Current Program</th>
              <th>Assigned Program</th>
              <th>Last Seen</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>${rowsHtml || '<tr><td colspan="9" class="muted">No node state found.</td></tr>'}</tbody>
        </table>
      </div>
    `);
  } catch (error) {
    renderError('Dashboard', error);
  }

  setAutoRefresh(async () => {
    if (APP.activeTab === 'dashboard') {
      await renderDashboard();
    }
  });
}

// 5. Desired State Tab
let desiredRowKeySequence = 0;
function desiredRowKey(nowMs) {
  const seq = desiredRowKeySequence++;
  const invTs = (BigInt('0xffffffffffffffff') - BigInt(nowMs)).toString(16).padStart(16, '0');
  const invSeq = (BigInt('0xffffffffffffffff') - BigInt(seq)).toString(16).padStart(16, '0');
  return `${invTs}:${invSeq}:${randomHex(8)}`;
}

function desiredRowsTable(rows) {
  const sorted = latestByPartition(rows).sort((left, right) => String(left.node_id || '').localeCompare(String(right.node_id || '')));
  return `
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Node ID</th>
            <th>Schedule (s)</th>
            <th>Program Hash</th>
            <th>Updated</th>
          </tr>
        </thead>
        <tbody>
          ${sorted.map((row) => `
            <tr>
              <td>${escapeHtml(row.node_id || '—')}</td>
              <td>${escapeHtml(row.desired_schedule_interval_s ?? '—')}</td>
              <td>${formatHashCell(row.desired_assigned_program_hash || '')}</td>
              <td>${escapeHtml(relativeTime(row.timestamp_ms))}</td>
            </tr>
          `).join('') || '<tr><td colspan="4" class="muted">No desired state entries found.</td></tr>'}
        </tbody>
      </table>
    </div>
  `;
}

async function renderDesiredState() {
  if (!APP.account) {
    requireAuthenticatedView('Desired State');
    return;
  }

  const savedMessage = APP.viewMessage;
  renderCard('Desired State', '<p class="muted">Loading desired state…</p>');
  APP.viewMessage = savedMessage;

  try {
    const [programs, desiredRows] = await Promise.all([
      listPrograms(),
      queryTable(CONFIG.desiredStateTable, ''),
    ]);

    const programOptions = [
      '<option value="">No program target</option>',
      ...programs.map((program) => `<option value="${escapeHtml(program.RowKey)}">${escapeHtml(truncHash(program.RowKey))} — ${escapeHtml(program.source_filename || 'unnamed')}</option>`),
    ].join('');

    renderCard('Desired State', `
      <div class="panel stack">
        <form id="desired-state-form" class="form-grid">
          <label>Node ID
            <input name="nodeId" type="text" required>
          </label>
          <label>Schedule Interval (s)
            <input name="scheduleInterval" type="number" min="1" step="1" placeholder="60">
          </label>
          <label>Program Hash
            <select name="programHash">${programOptions}</select>
          </label>
          <div>
            <button type="submit" class="primary">Save Desired State</button>
          </div>
        </form>
      </div>
      <div class="panel stack">
        <h2>Latest Desired State</h2>
        ${desiredRowsTable(desiredRows)}
      </div>
    `);

    const form = document.getElementById('desired-state-form');
    form?.addEventListener('submit', async (event) => {
      event.preventDefault();
      const formData = new FormData(form);
      const nodeId = String(formData.get('nodeId') || '').trim();
      const scheduleValue = String(formData.get('scheduleInterval') || '').trim();
      const programHash = String(formData.get('programHash') || '').trim();

      if (!nodeId) {
        showViewMessage('error', 'Node ID is required.');
        await renderDesiredState();
        return;
      }

      try {
        const nowMs = Date.now();
        const partitionKey = `n:${await sha256hex(nodeId)}`;
        const rowKey = desiredRowKey(nowMs);
        const entity = {
          PartitionKey: partitionKey,
          RowKey: rowKey,
          node_id: nodeId,
          timestamp_ms: String(nowMs),
          'timestamp_ms@odata.type': 'Edm.Int64',
        };

        if (scheduleValue) {
          entity.desired_schedule_interval_s = Number(scheduleValue);
          entity['desired_schedule_interval_s@odata.type'] = 'Edm.Int32';
        }
        if (programHash) {
          entity.desired_assigned_program_hash = programHash.toLowerCase();
        }

        await insertEntity(CONFIG.desiredStateTable, entity);
        showViewMessage('success', 'Desired state saved.');
      } catch (error) {
        showViewMessage('error', parseErrorPayload(error, 'Failed to save desired state.'));
      }

      await renderDesiredState();
    });
  } catch (error) {
    renderError('Desired State', error);
  }
}

// 6. Programs Tab
function programRowsTable(programs) {
  return `
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Hash</th>
            <th>Filename</th>
            <th>ABI</th>
            <th>Size</th>
            <th>Created</th>
          </tr>
        </thead>
        <tbody>
          ${programs.map((program) => `
            <tr>
              <td>${formatHashCell(program.RowKey)}</td>
              <td>${escapeHtml(program.source_filename || '—')}</td>
              <td>${escapeHtml(program.abi_version ?? '—')}</td>
              <td>${escapeHtml(program.size_bytes ?? '—')}</td>
              <td>${escapeHtml(program.created_at || '—')}</td>
            </tr>
          `).join('') || '<tr><td colspan="5" class="muted">No programs found.</td></tr>'}
        </tbody>
      </table>
    </div>
  `;
}

async function renderPrograms() {
  if (!APP.account) {
    requireAuthenticatedView('Programs');
    return;
  }

  const savedMessage = APP.viewMessage;
  renderCard('Programs', '<p class="muted">Loading programs…</p>');
  APP.viewMessage = savedMessage;

  try {
    const programs = await listPrograms();

    renderCard('Programs', `
      <div class="panel stack">
        <form id="program-upload-form" class="form-grid">
          <label>ELF File
            <input name="elf" type="file" accept=".o,.elf" required>
          </label>
          <label>Source Filename
            <input name="sourceFilename" type="text" required>
          </label>
          <label>ABI Version
            <input name="abiVersion" type="number" min="1" step="1" value="1" required>
          </label>
          <label>Verification Profile
            <select name="verificationProfile">
              <option value="resident">resident</option>
              <option value="ephemeral">ephemeral</option>
            </select>
          </label>
          <div>
            <button type="submit" class="primary">Upload Program</button>
          </div>
        </form>
      </div>
      <div class="panel stack">
        <h2>Programs</h2>
        ${programRowsTable(programs)}
      </div>
    `);

    const form = document.getElementById('program-upload-form');
    const fileInput = form?.querySelector('input[name="elf"]');
    const nameInput = form?.querySelector('input[name="sourceFilename"]');

    fileInput?.addEventListener('change', () => {
      const file = fileInput.files?.[0];
      if (file && nameInput && !nameInput.value) {
        nameInput.value = file.name;
      } else if (file && nameInput) {
        nameInput.value = file.name;
      }
    });

    form?.addEventListener('submit', async (event) => {
      event.preventDefault();
      const formData = new FormData(form);
      const file = fileInput?.files?.[0];
      if (!file) {
        showViewMessage('error', 'Select an ELF file to upload.');
        await renderPrograms();
        return;
      }

      try {
        requireConfig('functionAppName', 'Function app name');
        const token = await getToken();
        const arrayBuf = await file.arrayBuffer();
        const bytes = new Uint8Array(arrayBuf);
        let binary = '';
        for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
        const elfBase64 = btoa(binary);

        const payload = {
          elf: elfBase64,
          source_filename: String(formData.get('sourceFilename') || file.name),
          abi_version: Number(formData.get('abiVersion') || 1),
          verification_profile: String(formData.get('verificationProfile') || 'resident'),
        };

        const response = await fetch(`https://${CONFIG.functionAppName}.azurewebsites.net/api/programs/ingest`, {
          method: 'POST',
          headers: {
            Authorization: `Bearer ${token}`,
            'Content-Type': 'application/json',
          },
          body: JSON.stringify(payload),
        });

        const responseText = await response.text();
        let result = null;
        if (responseText) {
          try {
            result = JSON.parse(responseText);
          } catch {
            result = responseText;
          }
        }
        if (!response.ok) {
          throw new Error(parseErrorPayload(result, 'Program ingest failed.'));
        }

        const programHash = result && typeof result === 'object' ? result.program_hash : '';
        showViewMessage('success', `Program uploaded: ${programHash || 'ok'}`);
      } catch (error) {
        showViewMessage('error', parseErrorPayload(error, 'Program ingest failed.'));
      }

      await renderPrograms();
    });
  } catch (error) {
    renderError('Programs', error);
  }
}

// 7. Routes Tab
function routeRowsTable(routes) {
  return `
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Program Hash</th>
            <th>Handler Queue</th>
          </tr>
        </thead>
        <tbody>
          ${routes.map((route) => `
            <tr>
              <td>${formatHashCell(route.RowKey)}</td>
              <td>${escapeHtml(route.handler_queue || '—')}</td>
            </tr>
          `).join('') || '<tr><td colspan="2" class="muted">No routes found.</td></tr>'}
        </tbody>
      </table>
    </div>
  `;
}

async function renderRoutes() {
  if (!APP.account) {
    requireAuthenticatedView('Routes');
    return;
  }

  const savedMessage = APP.viewMessage;
  renderCard('Routes', '<p class="muted">Loading routes…</p>');
  APP.viewMessage = savedMessage;

  try {
    const [programs, routes] = await Promise.all([
      listPrograms(),
      queryTable(CONFIG.programRouteTable, "PartitionKey eq 'program'"),
    ]);

    const routeMap = new Map(routes.map((route) => [String(route.RowKey || '').toLowerCase(), route]));
    const programOptions = programs.map((program) => `<option value="${escapeHtml(program.RowKey)}">${escapeHtml(truncHash(program.RowKey))} — ${escapeHtml(program.source_filename || 'unnamed')}</option>`).join('');

    renderCard('Routes', `
      <div class="panel stack">
        <form id="route-form" class="form-grid">
          <label>Program Hash
            <select name="programHash" required>${programOptions}</select>
          </label>
          <label>Handler Queue
            <input name="handlerQueue" type="text" required>
          </label>
          <div>
            <button type="submit" class="primary">Save Route</button>
          </div>
        </form>
      </div>
      <div class="panel stack">
        <h2>Program Routes</h2>
        ${routeRowsTable(routes)}
      </div>
    `);

    const form = document.getElementById('route-form');
    const programSelect = form?.querySelector('select[name="programHash"]');
    const queueInput = form?.querySelector('input[name="handlerQueue"]');

    const syncQueue = () => {
      const selected = String(programSelect?.value || '').toLowerCase();
      const route = routeMap.get(selected);
      if (queueInput) {
        queueInput.value = route?.handler_queue || '';
      }
    };

    programSelect?.addEventListener('change', syncQueue);
    syncQueue();

    form?.addEventListener('submit', async (event) => {
      event.preventDefault();
      const selectedHash = String(programSelect?.value || '').trim().toLowerCase();
      const handlerQueue = String(queueInput?.value || '').trim();
      if (!selectedHash || !handlerQueue) {
        showViewMessage('error', 'Program hash and handler queue are required.');
        await renderRoutes();
        return;
      }

      try {
        const entity = {
          PartitionKey: 'program',
          RowKey: selectedHash,
          handler_queue: handlerQueue,
        };
        await upsertEntity(CONFIG.programRouteTable, 'program', selectedHash, entity);
        showViewMessage('success', 'Program route saved.');
      } catch (error) {
        showViewMessage('error', parseErrorPayload(error, 'Failed to save program route.'));
      }

      await renderRoutes();
    });
  } catch (error) {
    renderError('Routes', error);
  }
}

// 9. Tab Router
function setActiveTab(tabId) {
  APP.activeTab = TAB_IDS.includes(tabId) ? tabId : 'dashboard';
  for (const button of document.querySelectorAll('.tab-button')) {
    button.classList.toggle('active', button.dataset.tab === APP.activeTab);
  }
}

async function renderActiveTab() {
  clearRefresh();
  const requestedTab = location.hash.replace(/^#/, '') || 'dashboard';
  setActiveTab(requestedTab);

  switch (APP.activeTab) {
    case 'desired-state':
      await renderDesiredState();
      break;
    case 'programs':
      await renderPrograms();
      break;
    case 'routes':
      await renderRoutes();
      break;
    case 'dashboard':
    default:
      await renderDashboard();
      break;
  }
}

function attachTabHandlers() {
  for (const button of document.querySelectorAll('.tab-button')) {
    button.addEventListener('click', () => {
      const nextTab = button.dataset.tab || 'dashboard';
      if (location.hash.replace(/^#/, '') === nextTab) {
        renderActiveTab().catch((error) => renderError('Navigation failed', error));
        return;
      }
      location.hash = nextTab;
    });
  }

  window.addEventListener('hashchange', () => {
    renderActiveTab().catch((error) => renderError('Navigation failed', error));
  });
}

async function init() {
  attachTabHandlers();
  await loadConfig();
  await initMsal();
  if (!location.hash) {
    location.hash = 'dashboard';
  }
  await renderActiveTab();
}

document.addEventListener('DOMContentLoaded', () => {
  init().catch((error) => renderError('Application failed to start', error));
});
