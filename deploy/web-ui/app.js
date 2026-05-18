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
  sensorDataTable: 'sensordata',
  refreshIntervalMs: 30000,
};

const ENV_STORAGE_KEY = 'sonde_environments';
const ENV_ACTIVE_KEY = 'sonde_active_environment';

function loadEnvironments() {
  try {
    const raw = localStorage.getItem(ENV_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function saveEnvironments(envs) {
  try {
    localStorage.setItem(ENV_STORAGE_KEY, JSON.stringify(envs));
    return true;
  } catch {
    return false;
  }
}

function getActiveEnvironmentName() {
  try {
    return localStorage.getItem(ENV_ACTIVE_KEY) || '';
  } catch {
    return '';
  }
}

function setActiveEnvironmentName(name) {
  try {
    localStorage.setItem(ENV_ACTIVE_KEY, name);
  } catch {
    // Storage disabled or quota exceeded.
  }
}

function applyEnvironment(env) {
  if (!env) return;
  CONFIG.msalClientId = env.clientId || '';
  CONFIG.msalAuthority = env.tenantId
    ? `https://login.microsoftonline.com/${env.tenantId}`
    : '';
  CONFIG.storageAccount = env.storageAccount || '';
  CONFIG.functionAppName = env.functionAppName || '';
}

function loadActiveEnvironment() {
  const envs = loadEnvironments();
  const activeName = getActiveEnvironmentName();
  const env = envs.find((e) => e.name === activeName) || envs[0] || null;
  if (env) {
    setActiveEnvironmentName(env.name);
    applyEnvironment(env);
  }
  return env;
}

const STORAGE_SCOPES = ['https://storage.azure.com/.default'];
function functionScopes() {
  return [`api://${CONFIG.msalClientId}/user_impersonation`];
}
const TAB_IDS = ['dashboard', 'desired-state', 'programs', 'sensor-data'];
const APP = {
  msalApp: null,
  account: null,
  activeTab: 'dashboard',
  refreshHandle: null,
  refreshToken: 0,
  viewMessage: null,
  sensorChart: null,
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
    throw new Error(`${label} is not configured. Open the environment manager to set it.`);
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

  // Normalize pathname to directory (strip filename like index.html) so the
  // redirect URI matches the registered value (e.g. /sonde/ not /sonde/index.html).
  const basePath = window.location.pathname.replace(/\/[^/]*\.[^/]*$/, '/');

  // The SPA uses hash-based routing (#dashboard, #sensor-data, etc.) but
  // MSAL reads window.location.hash during construction and handleRedirectPromise().
  // Temporarily clear the routing hash so MSAL doesn't try to parse it as an
  // auth response.  Auth hashes (containing code=, error=, etc.) are left in place.
  const currentHash = window.location.hash;
  const isAuthHash = currentHash && (currentHash.includes('code=') || currentHash.includes('error=') || currentHash.includes('access_token='));
  if (currentHash && !isAuthHash) {
    history.replaceState(null, '', window.location.pathname + window.location.search);
  }

  APP.msalApp = new msal.PublicClientApplication({
    auth: {
      clientId: CONFIG.msalClientId,
      authority: CONFIG.msalAuthority,
      redirectUri: window.location.origin + basePath,
      navigateToLoginRequestUrl: false,
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

  // Restore the routing hash after MSAL has finished processing.
  if (currentHash && !isAuthHash) {
    history.replaceState(null, '', window.location.pathname + window.location.search + currentHash);
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

async function getFunctionToken() {
  if (!APP.account) {
    await login();
  }
  if (!APP.msalApp || !APP.account) {
    throw new Error('Sign in is required before calling Azure APIs.');
  }

  const scopes = functionScopes();
  try {
    const result = await APP.msalApp.acquireTokenSilent({
      account: APP.account,
      scopes,
    });
    return result.accessToken;
  } catch {
    const result = await APP.msalApp.acquireTokenPopup({
      account: APP.account,
      scopes,
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
      const diverged = (desired != null && desiredProgram !== actualProgram)
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
    const [programs, desiredRows, actualRows] = await Promise.all([
      listPrograms(),
      queryTable(CONFIG.desiredStateTable, ''),
      queryTable(CONFIG.actualStateTable, ''),
    ]);

    const latestActual = latestByPartition(actualRows)
      .filter((node) => node.node_id)
      .sort((left, right) =>
        String(left.node_id || '').localeCompare(String(right.node_id || '')));
    const desiredByPartition = new Map(
      latestByPartition(desiredRows).map((row) => [row.PartitionKey, row]));

    const nodeOptions = [
      '<option value="" disabled selected>Select a node…</option>',
      ...latestActual.map((node) =>
        `<option value="${escapeHtml(node.node_id || '')}">${escapeHtml(node.node_id || '—')}</option>`),
    ].join('');

    const programOptions = [
      '<option value="">No program target</option>',
      ...programs.map((program) => `<option value="${escapeHtml(program.RowKey)}">${escapeHtml(truncHash(program.RowKey))} — ${escapeHtml(program.source_filename || 'unnamed')}</option>`),
    ].join('');

    renderCard('Desired State', `
      <div class="panel stack">
        <form id="desired-state-form" class="form-grid">
          <label>Node ID
            <select name="nodeId" required>${nodeOptions}</select>
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

    // Auto-populate fields when a node is selected (WEB-0206, WEB-0207)
    const nodeSelect = form?.querySelector('[name="nodeId"]');
    nodeSelect?.addEventListener('change', () => {
      const selectedNodeId = nodeSelect.value;
      if (!selectedNodeId) return;

      const actualNode = latestActual.find((node) => node.node_id === selectedNodeId);
      const desiredNode = desiredByPartition.get(actualNode?.PartitionKey);

      // Per-field desired-over-actual fallback: use the desired value for
      // each field when present, otherwise fall back to the latest actual
      // value.  We use ?? (not ||) so that a zero schedule or an explicit
      // empty-string hash from a future schema change won't be skipped.
      const scheduleValue = desiredNode?.desired_schedule_interval_s
        ?? actualNode?.observed_schedule_interval_s
        ?? '';
      const hashValue = (desiredNode?.desired_assigned_program_hash
        ?? actualNode?.observed_assigned_program_hash
        ?? '').toLowerCase();

      const scheduleInput = form.querySelector('[name="scheduleInterval"]');
      if (scheduleInput) scheduleInput.value = scheduleValue;

      const programSelect = form.querySelector('[name="programHash"]');
      if (programSelect) {
        const matchingOption = [...programSelect.options].find(
          (opt) => opt.value.toLowerCase() === hashValue);
        programSelect.value = matchingOption ? matchingOption.value : '';
      }
    });

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
            <input name="abiVersion" type="number" min="1" step="1" value="2" required>
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
        const token = await getFunctionToken();
        const arrayBuf = await file.arrayBuffer();
        const bytes = new Uint8Array(arrayBuf);
        const chunkSize = 8192;
        const chunks = [];
        for (let i = 0; i < bytes.length; i += chunkSize) {
          chunks.push(String.fromCharCode.apply(null, bytes.subarray(i, i + chunkSize)));
        }
        const elfBase64 = btoa(chunks.join(''));

        const payload = {
          elf: elfBase64,
          source_filename: String(formData.get('sourceFilename') || file.name),
          abi_version: Number(formData.get('abiVersion') || 2),
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

// 8. Sensor Data Tab (WEB-0700)

// Series display overrides persisted in localStorage.
// Shape: { [seriesKey]: { displayName, scaleDivisor, unitSuffix } }
const SERIES_OVERRIDES_KEY = 'sonde_series_overrides';

function loadSeriesOverrides() {
  try {
    const raw = localStorage.getItem(SERIES_OVERRIDES_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return {};
    return parsed;
  } catch { return {}; }
}

function saveSeriesOverrides(overrides) {
  try {
    localStorage.setItem(SERIES_OVERRIDES_KEY, JSON.stringify(overrides));
  } catch {
    // Storage disabled or quota exceeded — surface to caller via return value
    return false;
  }
  return true;
}

function getSeriesDisplayLabel(series, overrides) {
  const ov = overrides || loadSeriesOverrides();
  const o = ov[series.key];
  return (o && o.displayName) ? o.displayName : series.label;
}

function getSeriesScale(seriesKey, overrides) {
  const ov = overrides || loadSeriesOverrides();
  const o = ov[seriesKey];
  if (o && typeof o.scaleDivisor === 'number' && Number.isFinite(o.scaleDivisor) && o.scaleDivisor !== 0) {
    return o.scaleDivisor;
  }
  return null;
}

function getSeriesUnitSuffix(seriesKey, overrides) {
  const ov = overrides || loadSeriesOverrides();
  const o = ov[seriesKey];
  return (o && o.unitSuffix) ? o.unitSuffix : '';
}

const SENSOR_STATE = {
  timeRange: '24h',
  viewMode: 'graph',
  selectedSeries: new Set(),
  seriesInitialized: false,
  autoRefresh: false,
};

const TIME_RANGE_MS = {
  '1h': 60 * 60 * 1000,
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
};

function reverseTimestampHex(ms) {
  const max = BigInt('0xffffffffffffffff');
  return (max - BigInt(ms)).toString(16).padStart(16, '0');
}

async function querySensorData(partitionKeys, timeRangeMs) {
  const token = await getToken();
  const now = Date.now();
  const start = now - timeRangeMs;
  const rkStart = reverseTimestampHex(now);
  const rkEnd = reverseTimestampHex(start);

  const fetchPartition = async (pk) => {
    const filter = `PartitionKey eq '${pk}' and RowKey ge '${rkStart}' and RowKey le '${rkEnd}~'`;
    const url = new URL(tableQueryUrl(CONFIG.sensorDataTable));
    url.searchParams.set('$filter', filter);
    url.searchParams.set('$top', '1000');

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
      throw new Error(`SensorData query failed (${response.status}): ${text}`);
    }

    const payload = await response.json();
    return Array.isArray(payload.value) ? payload.value : [];
  };

  const allEntities = [];
  const batchSize = 6;
  for (let i = 0; i < partitionKeys.length; i += batchSize) {
    const batch = partitionKeys.slice(i, i + batchSize);
    const results = await Promise.all(batch.map(fetchPartition));
    for (const entities of results) {
      allEntities.push(...entities);
    }
  }
  return allEntities;
}

function parseSensorReadings(decodedReadings) {
  if (!decodedReadings || decodedReadings === '') {
    return null;
  }
  try {
    return JSON.parse(decodedReadings);
  } catch {
    return null;
  }
}

function toPlottableNumber(value) {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === 'string') {
    const num = Number(value);
    if (Number.isFinite(num) && Math.abs(num) <= Number.MAX_SAFE_INTEGER) {
      return num;
    }
  }
  return null;
}

function formatReadingValue(value) {
  if (typeof value === 'string') {
    return value;
  }
  if (typeof value === 'number') {
    return String(value);
  }
  return '—';
}

function extractSeries(rows, nodeIdMap) {
  const seriesMap = new Map();

  for (const row of rows) {
    const readings = parseSensorReadings(row.decoded_readings);
    if (!readings) continue;

    const nodeId = nodeIdMap.get(row.PartitionKey) || row.PartitionKey;
    const programHash = row.program_hash || '';
    const timestampMs = Number(row.timestamp_ms);
    if (!Number.isFinite(timestampMs)) continue;

    for (const [readingName, value] of Object.entries(readings)) {
      const key = `${row.PartitionKey}|${programHash}|${readingName}`;
      if (!seriesMap.has(key)) {
        seriesMap.set(key, {
          key,
          nodeId,
          programHash,
          readingName,
          label: `${truncHash(nodeId)} / ${truncHash(programHash)} / ${readingName}`,
          points: [],
        });
      }
      const plottable = toPlottableNumber(value);
      if (plottable !== null) {
        seriesMap.get(key).points.push({ x: timestampMs, y: plottable });
      }
    }
  }

  for (const series of seriesMap.values()) {
    series.points.sort((a, b) => a.x - b.x);
  }

  return [...seriesMap.values()];
}

function downsamplePoints(points, maxPoints) {
  if (points.length <= maxPoints) return points;
  const step = points.length / (maxPoints - 1);
  const result = [];
  for (let i = 0; i < maxPoints - 1; i++) {
    result.push(points[Math.floor(i * step)]);
  }
  result.push(points[points.length - 1]);
  return result;
}

const CHART_COLORS = [
  '#2f6fed', '#e74c3c', '#27ae60', '#f39c12', '#8e44ad',
  '#1abc9c', '#d35400', '#2c3e50', '#c0392b', '#16a085',
  '#e67e22', '#9b59b6', '#3498db', '#2ecc71', '#e74c3c',
  '#f1c40f', '#1abc9c', '#e91e63', '#00bcd4', '#ff9800',
];

function renderSensorChart(allSeries) {
  const selected = allSeries.filter((s) => SENSOR_STATE.selectedSeries.has(s.key));

  if (APP.sensorChart) {
    APP.sensorChart.destroy();
    APP.sensorChart = null;
  }

  if (selected.length === 0) {
    const chartArea = contentEl.querySelector('.sensor-chart-area');
    if (chartArea) {
      const plottableCount = allSeries.filter((s) => s.points.length > 0).length;
      let message;
      if (allSeries.length === 0) {
        message = 'No decoded sensor readings found for the selected time range.';
      } else if (plottableCount === 0) {
        message = 'All readings contain non-numeric values that cannot be plotted. Switch to table view to inspect the data.';
      } else {
        message = 'No series selected. Use the series picker above to select data to plot.';
      }
      chartArea.innerHTML = `<p class="muted">${message}</p>`;
    }
    return;
  }

  const chartArea = contentEl.querySelector('.sensor-chart-area');
  if (!chartArea) return;
  chartArea.innerHTML = '<canvas id="sensor-canvas"></canvas>';

  const canvas = document.getElementById('sensor-canvas');
  if (!canvas || typeof Chart === 'undefined') {
    chartArea.innerHTML = '<p class="alert error">Chart.js is not available. Switch to table view.</p>';
    return;
  }

  const overrides = loadSeriesOverrides();

  const datasets = selected.slice(0, 20).map((series, i) => {
    const divisor = getSeriesScale(series.key, overrides);
    const scaledPoints = downsamplePoints(series.points, 500).map((p) => ({
      x: p.x,
      y: divisor ? p.y / divisor : p.y,
    }));
    const suffix = getSeriesUnitSuffix(series.key, overrides);
    return {
      label: getSeriesDisplayLabel(series, overrides),
      nodeId: series.nodeId,
      programHash: series.programHash,
      readingName: series.readingName,
      seriesKey: series.key,
      unitSuffix: suffix,
      data: scaledPoints,
      borderColor: CHART_COLORS[i % CHART_COLORS.length],
      backgroundColor: 'transparent',
      borderWidth: 1.5,
      pointRadius: series.points.length > 100 ? 0 : 2,
      tension: 0.1,
    };
  });

  APP.sensorChart = new Chart(canvas, {
    type: 'line',
    data: { datasets },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      interaction: { mode: 'nearest', intersect: false },
      scales: {
        x: {
          type: 'linear',
          title: { display: true, text: 'Time' },
          ticks: {
            callback(value) {
              const d = new Date(value);
              const hh = d.getHours().toString().padStart(2, '0');
              const mm = d.getMinutes().toString().padStart(2, '0');
              if (SENSOR_STATE.timeRange === '7d') {
                return `${d.getMonth() + 1}/${d.getDate()} ${hh}:${mm}`;
              }
              return `${hh}:${mm}`;
            },
            maxTicksLimit: 12,
          },
        },
        y: {
          title: {
            display: true,
            text: (() => {
              const suffixes = [...new Set(datasets.map((d) => d.unitSuffix))];
              return suffixes.length === 1 && suffixes[0] ? `Value (${suffixes[0]})` : 'Value';
            })(),
          },
        },
      },
      plugins: {
        tooltip: {
          callbacks: {
            title(items) {
              if (!items.length) return '';
              return new Date(items[0].parsed.x).toLocaleString();
            },
            label(item) {
              const ds = item.dataset;
              const suffix = ds.unitSuffix || '';
              return `${ds.label}: ${item.parsed.y}${suffix}`;
            },
          },
        },
        legend: {
          position: 'bottom',
          labels: { boxWidth: 12, padding: 8 },
        },
      },
    },
  });
}

function renderSensorTable(rows, nodeIdMap) {
  const sorted = [...rows].sort((a, b) => {
    const ta = Number(a.timestamp_ms) || 0;
    const tb = Number(b.timestamp_ms) || 0;
    return tb - ta;
  });

  const rowsHtml = sorted.map((row) => {
    const ts = Number(row.timestamp_ms);
    const timeStr = Number.isFinite(ts) ? new Date(ts).toLocaleString() : '—';
    const nodeId = nodeIdMap.get(row.PartitionKey) || row.PartitionKey;
    const readings = parseSensorReadings(row.decoded_readings);
    let readingsDisplay = '—';
    if (readings) {
      readingsDisplay = Object.entries(readings)
        .map(([k, v]) => `${escapeHtml(k)}: ${escapeHtml(formatReadingValue(v))}`)
        .join(', ');
    }
    const rawPayload = row.raw_payload || '—';
    const truncatedRaw = rawPayload.length > 40 ? rawPayload.slice(0, 40) + '…' : rawPayload;

    return `
      <tr>
        <td>${escapeHtml(timeStr)}</td>
        <td>${escapeHtml(nodeId)}</td>
        <td>${formatHashCell(row.program_hash)}</td>
        <td>${readingsDisplay}</td>
        <td><code title="${escapeHtml(rawPayload)}">${escapeHtml(truncatedRaw)}</code></td>
      </tr>
    `;
  }).join('');

  return `
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Timestamp</th>
            <th>Node ID</th>
            <th>Program Hash</th>
            <th>Decoded Readings</th>
            <th>Raw Payload</th>
          </tr>
        </thead>
        <tbody>${rowsHtml || '<tr><td colspan="5" class="muted">No sensor data found.</td></tr>'}</tbody>
      </table>
    </div>
  `;
}

function showSeriesEditDialog(seriesKey, rawLabel) {
  // Remove any existing dialog
  const existing = document.getElementById('series-edit-dialog');
  if (existing) existing.remove();

  const overrides = loadSeriesOverrides();
  const current = overrides[seriesKey] || {};
  const safeDivisor = (typeof current.scaleDivisor === 'number' && Number.isFinite(current.scaleDivisor))
    ? current.scaleDivisor : '';

  const dialog = document.createElement('div');
  dialog.id = 'series-edit-dialog';
  dialog.className = 'series-edit-overlay';
  dialog.setAttribute('role', 'dialog');
  dialog.setAttribute('aria-modal', 'true');
  dialog.setAttribute('aria-label', 'Edit series display settings');
  dialog.innerHTML = `
    <div class="series-edit-panel panel">
      <h3>Edit Series Display</h3>
      <p class="muted small">Raw label: ${escapeHtml(rawLabel)}</p>
      <div class="stack">
        <label>
          Display Name
          <input type="text" id="series-edit-name" placeholder="${escapeHtml(rawLabel)}"
                 value="${escapeHtml(current.displayName || '')}">
        </label>
        <label>
          Scale Divisor
          <input type="number" id="series-edit-divisor" step="any" placeholder="1"
                 value="${safeDivisor}">
          <span class="muted small">e.g. 1000 to convert milli-units → units</span>
        </label>
        <label>
          Unit Suffix
          <input type="text" id="series-edit-unit" placeholder=""
                 value="${escapeHtml(current.unitSuffix || '')}">
          <span class="muted small">e.g. °C, %, hPa — appended to values</span>
        </label>
        <div style="display:flex;gap:0.5rem;justify-content:flex-end">
          <button type="button" class="secondary" id="series-edit-reset">Reset to Default</button>
          <button type="button" class="secondary" id="series-edit-cancel">Cancel</button>
          <button type="button" class="primary" id="series-edit-save">Save</button>
        </div>
      </div>
    </div>
  `;

  document.body.appendChild(dialog);

  const previousFocus = document.activeElement;
  const nameInput = document.getElementById('series-edit-name');
  if (nameInput) nameInput.focus();

  function closeDialog() {
    dialog.remove();
    if (previousFocus && typeof previousFocus.focus === 'function') {
      previousFocus.focus();
    }
  }

  // Focus trap: cycle through focusable elements within the dialog
  dialog.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      closeDialog();
      return;
    }
    if (e.key !== 'Tab') return;
    const focusable = dialog.querySelectorAll('input, button, [tabindex]:not([tabindex="-1"])');
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  });

  dialog.addEventListener('click', (e) => {
    if (e.target === dialog) closeDialog();
  });

  document.getElementById('series-edit-cancel').addEventListener('click', () => {
    closeDialog();
  });

  document.getElementById('series-edit-reset').addEventListener('click', async () => {
    const ov = loadSeriesOverrides();
    delete ov[seriesKey];
    if (!saveSeriesOverrides(ov)) {
      alert('Failed to save settings — browser storage may be full or disabled.');
      return;
    }
    closeDialog();
    await renderSensorData();
  });

  document.getElementById('series-edit-save').addEventListener('click', async () => {
    const ov = loadSeriesOverrides();
    const name = document.getElementById('series-edit-name').value.trim();
    const divisorStr = document.getElementById('series-edit-divisor').value.trim();
    const unit = document.getElementById('series-edit-unit').value.trim();

    const divisor = divisorStr ? Number(divisorStr) : 0;

    if (divisorStr && (!Number.isFinite(divisor) || divisor === 0)) {
      const divisorInput = document.getElementById('series-edit-divisor');
      if (divisorInput) divisorInput.focus();
      alert('Scale divisor must be a finite non-zero number.');
      return;
    }

    if (name || (divisor && divisor !== 0) || unit) {
      ov[seriesKey] = {
        displayName: name || '',
        scaleDivisor: (divisor && Number.isFinite(divisor) && divisor !== 0) ? divisor : 0,
        unitSuffix: unit || '',
      };
    } else {
      delete ov[seriesKey];
    }

    if (!saveSeriesOverrides(ov)) {
      alert('Failed to save settings — browser storage may be full or disabled.');
      return;
    }
    closeDialog();
    await renderSensorData();
  });
}

async function renderSensorData() {
  if (!APP.account) {
    requireAuthenticatedView('Sensor Data');
    return;
  }

  renderCard('Sensor Data', '<p class="muted">Loading sensor data…</p>');

  if (APP.sensorChart) {
    APP.sensorChart.destroy();
    APP.sensorChart = null;
  }

  try {
    const actualRows = await queryTable(CONFIG.actualStateTable, '');
    const latestActual = latestByPartition(actualRows).sort((a, b) =>
      String(a.node_id || '').localeCompare(String(b.node_id || ''))
    );

    const nodeIdMap = new Map(latestActual.map((r) => [r.PartitionKey, r.node_id]));
    const partitionKeys = latestActual.map((r) => r.PartitionKey);

    if (partitionKeys.length === 0) {
      renderCard('Sensor Data', '<p class="muted">No nodes have reported state yet.</p>');
      if (SENSOR_STATE.autoRefresh) {
        setAutoRefresh(async () => {
          if (APP.activeTab === 'sensor-data') {
            await renderSensorData();
          }
        });
      }
      return;
    }

    const rangeMs = TIME_RANGE_MS[SENSOR_STATE.timeRange] || TIME_RANGE_MS['24h'];
    const sensorRows = await querySensorData(partitionKeys, rangeMs);
    const allSeries = extractSeries(sensorRows, nodeIdMap);

    // Prune stale and non-plottable selections before auto-selection
    const currentPlottableKeys = new Set(
      allSeries.filter((s) => s.points.length > 0).map((s) => s.key)
    );
    const sizeBefore = SENSOR_STATE.selectedSeries.size;
    for (const key of [...SENSOR_STATE.selectedSeries]) {
      if (!currentPlottableKeys.has(key)) {
        SENSOR_STATE.selectedSeries.delete(key);
      }
    }
    const prunedCount = sizeBefore - SENSOR_STATE.selectedSeries.size;
    if (SENSOR_STATE.selectedSeries.size === 0 && prunedCount > 0) {
      SENSOR_STATE.seriesInitialized = false;
    }

    if (!SENSOR_STATE.seriesInitialized && currentPlottableKeys.size > 0) {
      SENSOR_STATE.seriesInitialized = true;
      const plottable = allSeries.filter((s) => s.points.length > 0);
      for (const s of plottable.slice(0, Math.min(plottable.length, 5))) {
        SENSOR_STATE.selectedSeries.add(s.key);
      }
    }

    const timeRangeButtons = Object.keys(TIME_RANGE_MS).map((range) => {
      const active = SENSOR_STATE.timeRange === range ? ' active' : '';
      return `<button type="button" class="secondary sensor-range-btn${active}" data-range="${range}">${escapeHtml(range)}</button>`;
    }).join('');

    const viewToggle = `
      <button type="button" class="secondary sensor-view-btn${SENSOR_STATE.viewMode === 'graph' ? ' active' : ''}" data-view="graph">Graph</button>
      <button type="button" class="secondary sensor-view-btn${SENSOR_STATE.viewMode === 'table' ? ' active' : ''}" data-view="table">Table</button>
    `;

    const pickerOverrides = loadSeriesOverrides();
    const seriesCheckboxes = allSeries.map((s) => {
      const checked = SENSOR_STATE.selectedSeries.has(s.key) ? ' checked' : '';
      const plottable = s.points.length > 0;
      const suffix = plottable ? '' : ' <span class="muted">(no numeric data)</span>';
      const displayLabel = getSeriesDisplayLabel(s, pickerOverrides);
      const hasOverride = displayLabel !== s.label;
      const overrideTitle = hasOverride ? ` title="Raw: ${escapeHtml(s.label)}"` : '';
      const ariaLabel = `Edit display settings for ${displayLabel}`;
      return `<span class="sensor-series-item"><label class="sensor-series-label"${overrideTitle}><input type="checkbox" value="${escapeHtml(s.key)}"${checked}${plottable ? '' : ' disabled'}> ${escapeHtml(displayLabel)}${suffix}</label><button type="button" class="sensor-series-edit-btn" data-series-key="${escapeHtml(s.key)}" data-series-label="${escapeHtml(s.label)}" title="Edit display settings" aria-label="${escapeHtml(ariaLabel)}">✏️</button></span>`;
    }).join('');

    const autoRefreshChecked = SENSOR_STATE.autoRefresh ? ' checked' : '';

    renderCard('Sensor Data', `
      <div class="panel sensor-controls">
        <div class="sensor-control-row">
          <span class="sensor-control-group">
            <strong>Time range:</strong> ${timeRangeButtons}
          </span>
          <span class="sensor-control-group">
            <strong>View:</strong> ${viewToggle}
          </span>
          <label class="sensor-control-group">
            <input type="checkbox" id="sensor-auto-refresh"${autoRefreshChecked}> Auto-refresh
          </label>
        </div>
        ${allSeries.length > 0 ? `
          <details class="sensor-series-picker" open>
            <summary><strong>Series</strong> (${allSeries.length} available, max 20 plotted)</summary>
            <div class="sensor-series-grid">${seriesCheckboxes}</div>
          </details>
        ` : ''}
      </div>
      <div class="panel">
        ${SENSOR_STATE.viewMode === 'graph'
          ? '<div class="sensor-chart-area chart-container"><p class="muted">Rendering chart…</p></div>'
          : renderSensorTable(sensorRows, nodeIdMap)}
      </div>
    `);

    if (SENSOR_STATE.viewMode === 'graph') {
      renderSensorChart(allSeries);
    }

    // Attach event handlers
    for (const btn of contentEl.querySelectorAll('.sensor-range-btn')) {
      btn.addEventListener('click', async () => {
        SENSOR_STATE.timeRange = btn.dataset.range;
        await renderSensorData();
      });
    }

    for (const btn of contentEl.querySelectorAll('.sensor-view-btn')) {
      btn.addEventListener('click', async () => {
        SENSOR_STATE.viewMode = btn.dataset.view;
        await renderSensorData();
      });
    }

    const seriesCheckboxEls = contentEl.querySelectorAll('.sensor-series-grid input[type="checkbox"]');
    for (const cb of seriesCheckboxEls) {
      cb.addEventListener('change', () => {
        if (cb.checked) {
          if (SENSOR_STATE.selectedSeries.size >= 20) {
            cb.checked = false;
            return;
          }
          SENSOR_STATE.selectedSeries.add(cb.value);
        } else {
          SENSOR_STATE.selectedSeries.delete(cb.value);
        }
        if (SENSOR_STATE.viewMode === 'graph') {
          renderSensorChart(allSeries);
        }
      });
    }

    for (const btn of contentEl.querySelectorAll('.sensor-series-edit-btn')) {
      btn.addEventListener('click', () => {
        const seriesKey = btn.dataset.seriesKey;
        const rawLabel = btn.dataset.seriesLabel;
        showSeriesEditDialog(seriesKey, rawLabel);
      });
    }

    const autoRefreshCb = document.getElementById('sensor-auto-refresh');
    if (autoRefreshCb) {
      autoRefreshCb.addEventListener('change', () => {
        SENSOR_STATE.autoRefresh = autoRefreshCb.checked;
        if (SENSOR_STATE.autoRefresh) {
          setAutoRefresh(async () => {
            if (APP.activeTab === 'sensor-data') {
              await renderSensorData();
            }
          });
        } else {
          clearRefresh();
        }
      });
    }

    if (SENSOR_STATE.autoRefresh) {
      setAutoRefresh(async () => {
        if (APP.activeTab === 'sensor-data') {
          await renderSensorData();
        }
      });
    }
  } catch (error) {
    renderError('Sensor Data', error);
    if (SENSOR_STATE.autoRefresh) {
      setAutoRefresh(async () => {
        if (APP.activeTab === 'sensor-data') {
          await renderSensorData();
        }
      });
    }
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
  if (APP.sensorChart) {
    APP.sensorChart.destroy();
    APP.sensorChart = null;
  }

  switch (APP.activeTab) {
    case 'desired-state':
      await renderDesiredState();
      break;
    case 'programs':
      await renderPrograms();
      break;
    case 'sensor-data':
      await renderSensorData();
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
      setActiveTab(nextTab);
      renderActiveTab().catch((error) => renderError('Navigation failed', error));
    });
  }
}

async function init() {
  attachTabHandlers();
  document.getElementById('env-gear-btn')?.addEventListener('click', () => showEnvironmentManager());
  const env = loadActiveEnvironment();
  if (!env) {
    showEnvironmentManager();
    return;
  }
  updateEnvironmentIndicator();
  await initMsal();
  setActiveTab('dashboard');
  await renderActiveTab();
}

function clearMsalSessionStorage() {
  // Only remove MSAL-related keys to avoid clearing unrelated session data
  // on shared origins (e.g. GitHub Pages project sites).
  try {
    const keysToRemove = [];
    for (let i = 0; i < sessionStorage.length; i++) {
      const key = sessionStorage.key(i);
      if (key && (key.startsWith('msal.') || key.includes('.login.') || key.includes('.acquireToken.'))) {
        keysToRemove.push(key);
      }
    }
    for (const key of keysToRemove) {
      sessionStorage.removeItem(key);
    }
  } catch {
    // sessionStorage may be unavailable.
  }
}

async function switchEnvironment(name) {
  clearRefresh();
  setActiveEnvironmentName(name);
  const envs = loadEnvironments();
  const env = envs.find((e) => e.name === name);
  applyEnvironment(env);
  APP.msalApp = null;
  APP.account = null;
  clearMsalSessionStorage();
  updateEnvironmentIndicator();
  await initMsal();
  await renderActiveTab();
}

function updateEnvironmentIndicator() {
  const el = document.getElementById('env-indicator');
  if (!el) return;
  const name = getActiveEnvironmentName();
  el.textContent = name || '';
  el.title = name ? `Active environment: ${name}` : 'No environment selected';
}

function showEnvironmentManager() {
  const envs = loadEnvironments();
  const activeName = getActiveEnvironmentName();

  const envListHtml = envs.length === 0
    ? '<p class="muted">No environments configured. Add one to get started.</p>'
    : `<div class="table-wrap"><table>
        <thead><tr><th>Name</th><th>Storage Account</th><th>Function App</th><th></th></tr></thead>
        <tbody>${envs.map((env) => `<tr>
          <td><strong>${escapeHtml(env.name)}</strong>${env.name === activeName ? ' <span class="badge success">active</span>' : ''}</td>
          <td><code>${escapeHtml(env.storageAccount || '')}</code></td>
          <td><code>${escapeHtml(env.functionAppName || '')}</code></td>
          <td style="white-space:nowrap">
            ${env.name !== activeName ? `<button type="button" class="secondary env-use-btn" data-env="${escapeHtml(env.name)}">Use</button> ` : ''}
            <button type="button" class="secondary env-edit-btn" data-env="${escapeHtml(env.name)}">Edit</button>
            <button type="button" class="secondary env-delete-btn" data-env="${escapeHtml(env.name)}" style="color:var(--danger)">Delete</button>
          </td>
        </tr>`).join('')}
        </tbody></table></div>`;

  const overlayHtml = `<div class="env-manager-overlay" id="env-manager-overlay" role="dialog" aria-modal="true" aria-label="Environment Manager">
    <div class="env-manager-panel panel">
      <h2>Environments</h2>
      ${envListHtml}
      <div style="margin-top:1rem;display:flex;gap:0.5rem;flex-wrap:wrap">
        <button type="button" class="primary" id="env-add-btn">Add Environment</button>
        ${envs.length > 0 ? '<button type="button" class="secondary" id="env-close-btn">Close</button>' : ''}
      </div>
    </div>
  </div>`;

  let overlay = document.getElementById('env-manager-overlay');
  if (overlay) overlay.remove();
  document.body.insertAdjacentHTML('beforeend', overlayHtml);

  document.getElementById('env-add-btn')?.addEventListener('click', () => showEnvironmentForm(null));
  document.getElementById('env-close-btn')?.addEventListener('click', () => {
    document.getElementById('env-manager-overlay')?.remove();
  });

  for (const btn of document.querySelectorAll('.env-use-btn')) {
    btn.addEventListener('click', () => {
      document.getElementById('env-manager-overlay')?.remove();
      switchEnvironment(btn.dataset.env).catch((error) => renderError('Switch failed', error));
    });
  }
  for (const btn of document.querySelectorAll('.env-edit-btn')) {
    btn.addEventListener('click', () => {
      const env = loadEnvironments().find((e) => e.name === btn.dataset.env);
      if (env) showEnvironmentForm(env);
    });
  }
  for (const btn of document.querySelectorAll('.env-delete-btn')) {
    btn.addEventListener('click', () => {
      const name = btn.dataset.env;
      const envsList = loadEnvironments().filter((e) => e.name !== name);
      if (!saveEnvironments(envsList)) {
        showViewMessage('error', 'Failed to save changes. Browser storage may be disabled or full.');
      }
      if (getActiveEnvironmentName() === name) {
        if (envsList.length > 0) {
          switchEnvironment(envsList[0].name).catch((error) => renderError('Switch failed', error));
        } else {
          clearRefresh();
          setActiveEnvironmentName('');
          CONFIG.msalClientId = '';
          CONFIG.msalAuthority = '';
          CONFIG.storageAccount = '';
          CONFIG.functionAppName = '';
          APP.msalApp = null;
          APP.account = null;
          clearMsalSessionStorage();
          updateEnvironmentIndicator();
          updateAuthUi();
          contentEl.innerHTML = '';
        }
      }
      showEnvironmentManager();
    });
  }
}

function showEnvironmentForm(existingEnv) {
  const isEdit = existingEnv != null;
  const title = isEdit ? 'Edit Environment' : 'Add Environment';

  const formHtml = `<div class="env-manager-overlay" id="env-form-overlay" role="dialog" aria-modal="true" aria-label="${title}">
    <div class="env-manager-panel panel">
      <h2>${title}</h2>
      <div class="stack">
        <label>Name <input type="text" id="env-field-name" value="${escapeHtml(existingEnv?.name || '')}" ${isEdit ? 'readonly' : ''} placeholder="e.g. production"></label>
        <label>Entra Client ID <input type="text" id="env-field-clientId" value="${escapeHtml(existingEnv?.clientId || '')}" placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"></label>
        <label>Entra Tenant ID <input type="text" id="env-field-tenantId" value="${escapeHtml(existingEnv?.tenantId || '')}" placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"></label>
        <label>Storage Account <input type="text" id="env-field-storageAccount" value="${escapeHtml(existingEnv?.storageAccount || '')}" placeholder="mystorageaccount"></label>
        <label>Function App Name <input type="text" id="env-field-functionAppName" value="${escapeHtml(existingEnv?.functionAppName || '')}" placeholder="sonde-decoder-xxxx"></label>
      </div>
      <div style="margin-top:1rem;display:flex;gap:0.5rem">
        <button type="button" class="primary" id="env-save-btn">Save</button>
        <button type="button" class="secondary" id="env-cancel-btn">Cancel</button>
      </div>
      <div id="env-form-error" class="alert error" style="display:none;margin-top:0.75rem"></div>
    </div>
  </div>`;

  let formOverlay = document.getElementById('env-form-overlay');
  if (formOverlay) formOverlay.remove();
  document.body.insertAdjacentHTML('beforeend', formHtml);

  document.getElementById('env-cancel-btn')?.addEventListener('click', () => {
    document.getElementById('env-form-overlay')?.remove();
  });

  document.getElementById('env-save-btn')?.addEventListener('click', () => {
    const name = document.getElementById('env-field-name')?.value.trim();
    const clientId = document.getElementById('env-field-clientId')?.value.trim();
    const tenantId = document.getElementById('env-field-tenantId')?.value.trim();
    const storageAccount = document.getElementById('env-field-storageAccount')?.value.trim();
    const functionAppName = document.getElementById('env-field-functionAppName')?.value.trim();
    const errorEl = document.getElementById('env-form-error');

    if (!name || !clientId || !tenantId || !storageAccount || !functionAppName) {
      if (errorEl) {
        errorEl.textContent = 'All fields are required.';
        errorEl.style.display = '';
      }
      return;
    }

    const guidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
    if (!guidPattern.test(clientId)) {
      if (errorEl) { errorEl.textContent = 'Client ID must be a valid GUID.'; errorEl.style.display = ''; }
      return;
    }
    if (!guidPattern.test(tenantId)) {
      if (errorEl) { errorEl.textContent = 'Tenant ID must be a valid GUID.'; errorEl.style.display = ''; }
      return;
    }
    if (!/^[a-z0-9]{3,24}$/.test(storageAccount)) {
      if (errorEl) { errorEl.textContent = 'Storage Account must be 3–24 lowercase alphanumeric characters.'; errorEl.style.display = ''; }
      return;
    }
    if (!/^[a-zA-Z0-9][a-zA-Z0-9-]{0,58}[a-zA-Z0-9]$/.test(functionAppName)) {
      if (errorEl) { errorEl.textContent = 'Function App Name must be 2–60 alphanumeric characters with optional hyphens.'; errorEl.style.display = ''; }
      return;
    }

    const envs = loadEnvironments();
    if (!isEdit && envs.some((e) => e.name === name)) {
      if (errorEl) {
        errorEl.textContent = `An environment named "${name}" already exists.`;
        errorEl.style.display = '';
      }
      return;
    }

    const envData = { name, clientId, tenantId, storageAccount, functionAppName };
    if (isEdit) {
      const idx = envs.findIndex((e) => e.name === name);
      if (idx >= 0) envs[idx] = envData;
    } else {
      envs.push(envData);
    }
    if (!saveEnvironments(envs)) {
      if (errorEl) { errorEl.textContent = 'Failed to save environment. Browser storage may be disabled or full.'; errorEl.style.display = ''; }
      return;
    }

    const isFirstEnv = !isEdit && envs.length === 1;
    const isActiveEnv = getActiveEnvironmentName() === name;

    document.getElementById('env-form-overlay')?.remove();

    if (isFirstEnv || isActiveEnv) {
      document.getElementById('env-manager-overlay')?.remove();
      switchEnvironment(name).catch((error) => renderError('Switch failed', error));
    } else {
      showEnvironmentManager();
    }
  });
}

document.addEventListener('DOMContentLoaded', () => {
  // MSAL loginPopup() opens a popup that loads this SPA.  The popup only needs
  // MSAL to process the auth response — skip full app init to avoid unnecessary
  // API calls and rendering.
  if (window.opener && window.opener !== window) {
    return;
  }
  init().catch((error) => renderError('Application failed to start', error));
});
