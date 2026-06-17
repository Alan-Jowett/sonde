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
const LEGACY_SERIES_OVERRIDES_KEY = 'sonde_series_overrides';
const SENSOR_VIEW_MODES = new Set(['graph', 'table']);
const SENSOR_TIME_RANGES = new Set(['1h', '24h', '7d']);
const BLOCKED_OBJECT_KEYS = new Set(['__proto__', 'constructor', 'prototype']);

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

function normalizeSeriesOverrides(rawOverrides) {
  if (typeof rawOverrides !== 'object' || rawOverrides === null || Array.isArray(rawOverrides)) {
    return {};
  }
  const normalized = {};
  for (const [seriesKey, entry] of Object.entries(rawOverrides)) {
    if (BLOCKED_OBJECT_KEYS.has(seriesKey)) {
      continue;
    }
    const normalizedEntry = normalizeSeriesOverrideEntry(entry);
    if (normalizedEntry) {
      normalized[seriesKey] = normalizedEntry;
    }
  }
  return normalized;
}

function sanitizeSensorDataPreferences(rawPreferences) {
  const defaults = createDefaultSensorDataPreferences();
  if (typeof rawPreferences !== 'object' || rawPreferences === null || Array.isArray(rawPreferences)) {
    return defaults;
  }
  return {
    viewMode: SENSOR_VIEW_MODES.has(rawPreferences.viewMode) ? rawPreferences.viewMode : defaults.viewMode,
    timeRange: SENSOR_TIME_RANGES.has(rawPreferences.timeRange) ? rawPreferences.timeRange : defaults.timeRange,
    selectedSeries: Array.isArray(rawPreferences.selectedSeries)
      ? rawPreferences.selectedSeries.filter((value) => typeof value === 'string')
      : [],
    selectedSeriesInitialized: typeof rawPreferences.selectedSeriesInitialized === 'boolean'
      ? rawPreferences.selectedSeriesInitialized
      : Array.isArray(rawPreferences.selectedSeries),
    seriesOverrides: normalizeSeriesOverrides(rawPreferences.seriesOverrides),
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

// Dashboard data model
const RESERVED_FUNCTION_NAMES = ['sqrt', 'log', 'log10', 'exp', 'abs', 'min', 'max'];
const DASHBOARD_TIME_RANGE_MS = {
  '1h': 60 * 60 * 1000,
  '6h': 6 * 60 * 60 * 1000,
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
};
const DASHBOARD_READING_DISCOVERY_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;

function createDefaultDashboardsArray() {
  return [];
}

function createDefaultDashboard(name) {
  return {
    name: name || 'Dashboard 1',
    variables: [],
    metrics: [],
    timeRange: {
      preset: '24h',
      start: null,
      end: null
    }
  };
}

function normalizeDashboardTimeRange(timeRange) {
  const preset = typeof timeRange?.preset === 'string' ? timeRange.preset : '24h';
  const start = Number.isFinite(timeRange?.start) ? timeRange.start : null;
  const end = Number.isFinite(timeRange?.end) ? timeRange.end : null;

  if (preset === 'custom' && start != null && end != null && start < end) {
    return { preset, start, end };
  }

  if (Object.prototype.hasOwnProperty.call(DASHBOARD_TIME_RANGE_MS, preset)) {
    return { preset, start: null, end: null };
  }

  return { preset: '24h', start: null, end: null };
}

function normalizeDashboardVariable(variable) {
  if (typeof variable !== 'object' || variable === null) {
    return null;
  }
  const name = typeof variable.name === 'string' ? variable.name.trim() : '';
  const nodeId = typeof variable.nodeId === 'string' ? variable.nodeId.trim() : '';
  const readingType = typeof variable.readingType === 'string' ? variable.readingType.trim() : '';
  if (!name || !nodeId || !readingType) {
    return null;
  }
  return { name, nodeId, readingType };
}

function normalizeDashboardMetric(metric) {
  if (typeof metric !== 'object' || metric === null) {
    return null;
  }
  const displayName = typeof metric.displayName === 'string' ? metric.displayName : '';
  const expression = typeof metric.expression === 'string' ? metric.expression : '';
  if (!expression) {
    return null;
  }
  return {
    id: typeof metric.id === 'string' ? metric.id : `metric-${crypto.randomUUID?.() || Date.now()}`,
    displayName,
    expression,
    color: typeof metric.color === 'string' ? metric.color : '#007bff',
  };
}

function normalizeDashboard(dashboard, index) {
  if (typeof dashboard !== 'object' || dashboard === null) {
    return createDefaultDashboard(`Dashboard ${index + 1}`);
  }
  return {
    name: typeof dashboard.name === 'string' && dashboard.name.trim()
      ? dashboard.name.trim()
      : `Dashboard ${index + 1}`,
    variables: Array.isArray(dashboard.variables)
      ? dashboard.variables.map(normalizeDashboardVariable).filter(Boolean)
      : [],
    metrics: Array.isArray(dashboard.metrics)
      ? dashboard.metrics.map(normalizeDashboardMetric).filter(Boolean)
      : [],
    timeRange: normalizeDashboardTimeRange(dashboard.timeRange),
  };
}

function validateVariableName(name, existingNames) {
  if (!/^[a-zA-Z_][a-zA-Z0-9_]*$/.test(name)) {
    return { valid: false, error: 'Variable name must be a valid JavaScript identifier' };
  }
  if (BLOCKED_OBJECT_KEYS.has(name)) {
    return { valid: false, error: `'${name}' is reserved and cannot be used as a variable name` };
  }
  if (existingNames.includes(name)) {
    return { valid: false, error: 'Variable name must be unique within dashboard' };
  }
  if (RESERVED_FUNCTION_NAMES.includes(name)) {
    return { valid: false, error: `'${name}' is a reserved function name` };
  }
  return { valid: true };
}

function validateExpression(expression, availableVariables) {
  try {
    const parser = new exprEval.Parser();
    const expr = parser.parse(expression);
    const usedVars = expr.variables();
    const undefinedVars = usedVars.filter(v => !availableVariables.includes(v));
    if (undefinedVars.length > 0) {
      return {
        valid: true,
        warning: `Undefined variables: ${undefinedVars.join(', ')}`
      };
    }
    return { valid: true };
  } catch (error) {
    return {
      valid: false,
      error: `Syntax error: ${error.message}`
    };
  }
}

function isVariableUsedInExpression(variableName, expression) {
  try {
    const parser = new exprEval.Parser();
    const expr = parser.parse(expression);
    return expr.variables().includes(variableName);
  } catch {
    return expression.includes(variableName);
  }
}

function normalizeEnvironmentRecord(env) {
  const normalized = {
    name: typeof env?.name === 'string' ? env.name : '',
    clientId: typeof env?.clientId === 'string' ? env.clientId : '',
    tenantId: typeof env?.tenantId === 'string' ? env.tenantId : '',
    storageAccount: typeof env?.storageAccount === 'string' ? env.storageAccount : '',
    functionAppName: typeof env?.functionAppName === 'string' ? env.functionAppName : '',
    sensorData: sanitizeSensorDataPreferences(env?.sensorData),
    dashboards: Array.isArray(env?.dashboards)
      ? env.dashboards.map((dashboard, index) => normalizeDashboard(dashboard, index))
      : createDefaultDashboardsArray(),
  };

  // Re-validate all metric expressions (defense-in-depth for localStorage tampering)
  normalized.dashboards.forEach(dashboard => {
    if (!dashboard.metrics) return;
    dashboard.metrics.forEach(metric => {
      const variableNames = (dashboard.variables || []).map(v => v.name);
      const validation = validateExpression(metric.expression, variableNames);
      if (validation.error) {
        metric._validationError = validation.error;
      } else if (validation.warning) {
        metric._validationWarning = validation.warning;
      }
    });
  });

  return normalized;
}

function buildEnvironmentExportData(env) {
  const normalizedEnv = normalizeEnvironmentRecord(env);
  return {
    version: 1,
    name: normalizedEnv.name,
    clientId: normalizedEnv.clientId,
    tenantId: normalizedEnv.tenantId,
    storageAccount: normalizedEnv.storageAccount,
    functionAppName: normalizedEnv.functionAppName,
    sensorData: {
      viewMode: normalizedEnv.sensorData.viewMode,
      timeRange: normalizedEnv.sensorData.timeRange,
      ...(normalizedEnv.sensorData.selectedSeriesInitialized
        ? { selectedSeries: normalizedEnv.sensorData.selectedSeries }
        : {}),
      seriesOverrides: normalizedEnv.sensorData.seriesOverrides,
    },
    dashboards: normalizedEnv.dashboards,
  };
}

function loadLegacySeriesOverrides() {
  try {
    const raw = localStorage.getItem(LEGACY_SERIES_OVERRIDES_KEY);
    if (!raw) return {};
    return normalizeSeriesOverrides(JSON.parse(raw));
  } catch {
    return {};
  }
}

function loadEnvironments() {
  try {
    const raw = localStorage.getItem(ENV_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.map(normalizeEnvironmentRecord) : [];
  } catch {
    return [];
  }
}

function saveEnvironments(envs) {
  try {
    localStorage.setItem(ENV_STORAGE_KEY, JSON.stringify(envs.map(normalizeEnvironmentRecord)));
    return true;
  } catch (error) {
    if (error.name === 'QuotaExceededError') {
      alert('Storage quota exceeded. Environment changes could not be saved. Try deleting old data or clearing browser data.');
      return false;
    }
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

function activateEnvironmentState(name, env) {
  clearRefresh();
  resetTransientSensorDataState();
  if (APP_DASHBOARD_STATE.unsavedEnvironment?.name && APP_DASHBOARD_STATE.unsavedEnvironment.name !== name) {
    clearUnsavedDashboardEnvironment();
  }
  setActiveEnvironmentName(name);
  applyEnvironment(env);
  applySensorDataPreferences(env?.sensorData);
  APP.msalApp = null;
  APP.account = null;
  clearMsalSessionStorage();
  updateEnvironmentIndicator();
}

function loadActiveEnvironment() {
  let envs = loadEnvironments();
  const activeName = getActiveEnvironmentName();
  const unsavedEnv = APP_DASHBOARD_STATE.unsavedEnvironment?.name === activeName
    ? normalizeEnvironmentRecord(APP_DASHBOARD_STATE.unsavedEnvironment)
    : null;
  let env = unsavedEnv || envs.find((e) => e.name === activeName) || envs[0] || null;
  if (env) {
    setActiveEnvironmentName(env.name);
    const legacyOverrides = loadLegacySeriesOverrides();
    if (Object.keys(legacyOverrides).length > 0 && Object.keys(env.sensorData.seriesOverrides).length === 0) {
      const envIndex = envs.findIndex((entry) => entry.name === env.name);
      if (envIndex >= 0) {
        const migratedEnv = normalizeEnvironmentRecord(envs[envIndex]);
        migratedEnv.sensorData.seriesOverrides = legacyOverrides;
        const migratedEnvs = [...envs];
        migratedEnvs[envIndex] = migratedEnv;
        if (saveEnvironments(migratedEnvs)) {
          envs = migratedEnvs;
          env = migratedEnv;
          try {
            localStorage.removeItem(LEGACY_SERIES_OVERRIDES_KEY);
          } catch {
            // Storage may be unavailable; keep using environment-scoped preferences.
          }
        } else {
          showViewMessage('error', 'Failed to migrate Sensor Data preferences. Browser storage may be disabled or full.');
        }
      }
    }
    applyEnvironment(env);
    applySensorDataPreferences(env.sensorData);
  }
  return env;
}

const STORAGE_SCOPES = ['https://storage.azure.com/.default'];

// 1b. Environment field validation helpers (shared by manual form and import)
const ENV_GUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const ENV_STORAGE_ACCOUNT_PATTERN = /^[a-z0-9]{3,24}$/;
const ENV_FUNCTION_APP_PATTERN = /^[a-zA-Z0-9][a-zA-Z0-9-]{0,58}[a-zA-Z0-9]$/;

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

// 1c. Environment import/export
function importEnvironmentFromFile() {
  const input = document.createElement('input');
  input.type = 'file';
  input.accept = '.json,application/json';
  input.addEventListener('change', () => {
    const file = input.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      try {
        handleImportedJson(reader.result);
      } catch (err) {
        showViewMessage('error', `Import failed: ${err.message}`);
      }
    };
    reader.onerror = () => {
      showViewMessage('error', 'Failed to read the selected file.');
    };
    reader.readAsText(file);
  });
  input.click();
}

function handleImportedJson(text) {
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
  if (validationError) throw new Error(validationError);
  const sensorData = validateImportedSensorDataPreferences(data.sensorData);
  
  // Validate and import dashboards
  let dashboards = [];
  if (Array.isArray(data.dashboards)) {
    dashboards = data.dashboards.map(d => {
      if (typeof d !== 'object' || d === null) return null;
      return {
        name: typeof d.name === 'string' ? d.name : 'Imported Dashboard',
        variables: Array.isArray(d.variables) ? d.variables.filter(v =>
          typeof v === 'object' && v !== null &&
          typeof v.name === 'string' &&
          typeof v.nodeId === 'string' &&
          typeof v.readingType === 'string'
        ) : [],
        metrics: Array.isArray(d.metrics) ? d.metrics.filter(m =>
          typeof m === 'object' && m !== null &&
          typeof m.displayName === 'string' &&
          typeof m.expression === 'string'
        ) : [],
        timeRange: (typeof d.timeRange === 'object' && d.timeRange !== null)
          ? normalizeDashboardTimeRange({
              preset: typeof d.timeRange.preset === 'string' ? d.timeRange.preset : '24h',
              start: d.timeRange.start == null ? null : Number(d.timeRange.start),
              end: d.timeRange.end == null ? null : Number(d.timeRange.end)
            })
          : { preset: '24h', start: null, end: null }
      };
    }).filter(Boolean);
  }

  let name = typeof data.name === 'string' ? data.name.trim() : '';
  if (!name) {
    name = window.prompt('Enter a name for this environment:');
    if (!name || !name.trim()) throw new Error('Import cancelled — no name provided.');
    name = name.trim();
  }

  const envs = loadEnvironments();
  const existing = envs.find((e) => e.name === name);
  if (existing) {
    const choice = window.confirm(
      `An environment named "${name}" already exists.\n\nClick OK to overwrite it, or Cancel to rename.`
    );
    if (!choice) {
      const newName = window.prompt('Enter a different name for this environment:', `${name} (2)`);
      if (!newName || !newName.trim()) throw new Error('Import cancelled — no name provided.');
      name = newName.trim();
      if (envs.some((e) => e.name === name)) {
        throw new Error(`An environment named "${name}" already exists.`);
      }
    }
  }

  const envData = { name, ...fields, sensorData, dashboards };
  const idx = envs.findIndex((e) => e.name === name);
  if (idx >= 0) {
    envs[idx] = envData;
  } else {
    envs.push(envData);
  }

  if (!saveEnvironments(envs)) {
    throw new Error('Failed to save environment. Browser storage may be disabled or full.');
  }
  clearUnsavedDashboardEnvironment(name);

  const isFirstEnv = envs.length === 1;
  const isActiveEnv = getActiveEnvironmentName() === name;

  document.getElementById('env-form-overlay')?.remove();
  document.getElementById('env-manager-overlay')?.remove();

  if (isFirstEnv || isActiveEnv) {
    switchEnvironment(name).catch((error) => renderError('Switch failed', error));
  } else {
    showEnvironmentManager();
  }
}

function exportEnvironment(env) {
  const data = buildEnvironmentExportData(env);
  const json = JSON.stringify(data, null, 2);
  const blob = new Blob([json], { type: 'application/json' });

  const safeName = (env.name || '')
    .replace(/[/\\:*?"<>|\x00-\x1F\x7F]/g, '-')
    .replace(/^-+|-+$/g, '')
    .trim();
  const filename = safeName ? `${safeName}.json` : 'sonde-environment.json';

  downloadBlob(filename, blob);
}

function downloadBlob(filename, blob) {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  URL.revokeObjectURL(url);
}
function functionScopes() {
  return [`api://${CONFIG.msalClientId}/user_impersonation`];
}
const TAB_IDS = ['dashboard', 'desired-state', 'programs', 'sensor-data', 'dashboards'];
const APP = {
  msalApp: null,
  account: null,
  activeTab: 'dashboard',
  refreshHandle: null,
  refreshToken: 0,
  viewMessage: null,
  sensorChart: null,
  rotationFormOpen: null, // gateway ID when rotation form is expanded (WEB-1002)
};
const DASHBOARD_EXPORT_STATE = {
  startMs: null,
  endMs: null,
  format: 'jsonl',
  busy: false,
  message: null,
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

// 8a. Key Management — BIP-39 fingerprint (WEB-1001)
let BIP39_WORDLIST = null;

async function loadBip39Wordlist() {
  if (BIP39_WORDLIST) return BIP39_WORDLIST;
  try {
    const response = await fetch('vendor/bip39-english.txt');
    if (!response.ok) throw new Error(`Failed to load BIP-39 wordlist: ${response.status}`);
    const text = await response.text();
    BIP39_WORDLIST = text.trim().split('\n').map((w) => w.trim()).filter((w) => w.length > 0);
    if (BIP39_WORDLIST.length !== 2048) {
      throw new Error(`BIP-39 wordlist has ${BIP39_WORDLIST.length} words, expected 2048`);
    }
    return BIP39_WORDLIST;
  } catch {
    BIP39_WORDLIST = null;
    return null;
  }
}

async function computeBip39Fingerprint(x25519PublicKeyBytes) {
  const wordlist = await loadBip39Wordlist();
  if (!wordlist || !x25519PublicKeyBytes || x25519PublicKeyBytes.length !== 32) {
    return null;
  }
  const hash = await crypto.subtle.digest('SHA-256', x25519PublicKeyBytes);
  const hashBytes = new Uint8Array(hash);
  // Extract 66 bits (6 × 11-bit indices) from the hash.
  const words = [];
  for (let i = 0; i < 6; i++) {
    const bitOffset = i * 11;
    const byteIdx = Math.floor(bitOffset / 8);
    const bitShift = bitOffset % 8;
    // Read 24 bits (3 bytes) to handle 11-bit windows that span 3 bytes.
    const val = ((hashBytes[byteIdx] << 16)
      | ((hashBytes[byteIdx + 1] || 0) << 8)
      | (hashBytes[byteIdx + 2] || 0)) >>> (24 - 11 - bitShift);
    const idx = val & 0x7ff;
    words.push(wordlist[idx]);
  }
  return words;
}

function hexToBytes(hex) {
  if (!hex || hex.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(hex)) return null;
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < hex.length; i += 2) {
    bytes[i / 2] = parseInt(hex.substring(i, i + 2), 16);
  }
  return bytes;
}

function base64ToBytes(b64) {
  try {
    const binary = atob(b64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return bytes;
  } catch {
    return null;
  }
}

function bytesToHex(bytes) {
  return [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');
}

function isNodePartitionKey(pk) {
  return pk && pk.startsWith('n:');
}

function isGatewayPartitionKey(pk) {
  return pk && pk.startsWith('g:');
}

function filterNodeRows(rows) {
  return rows.filter((r) => isNodePartitionKey(r.PartitionKey));
}

function filterGatewayRows(rows) {
  return rows.filter((r) => isGatewayPartitionKey(r.PartitionKey));
}

// Gateway convergence logic (WEB-1009).
// Returns true if the gateway's actual state diverges from desired state.
function computeGatewayDivergence(actual, desired) {
  if (!desired) return false;

  // Rotation payload pending: desired has rotation_payload AND gateway has not
  // consumed it (rotation_in_progress is not true AND epoch has not advanced
  // past submitted_epoch).
  if (desired.rotation_payload) {
    if (actual.rotation_in_progress === true) {
      // Gateway is actively processing — not diverged for this condition.
    } else {
      const submittedEpoch = Number(desired.submitted_epoch);
      if (!Number.isNaN(submittedEpoch)) {
        // New-style row with submitted_epoch — compare epochs.
        const actualEpoch = Number(actual.master_key_epoch) || 0;
        if (actualEpoch <= submittedEpoch) return true;
      }
      // Legacy row without submitted_epoch — cannot determine convergence,
      // skip to avoid permanent false Diverged after gateway consumes it.
    }
  }

  // Channel mismatch.
  if (desired.channel != null) {
    const desiredCh = Number(desired.channel);
    const actualCh = Number(actual.channel);
    if (!Number.isNaN(desiredCh) && desiredCh !== actualCh) return true;
  }

  // Salt and KDF convergence retired (GW-2020, GW-2021).

  return false;
}

async function renderGatewayStatusCard(gatewayRows, gatewayDesiredRows) {
  if (!gatewayRows || gatewayRows.length === 0) {
    return '<div class="panel stack"><h2>Gateway Status</h2><p class="muted">No gateway connected.</p></div>';
  }

  const desiredByPartition = new Map(
    (gatewayDesiredRows || []).map((row) => [row.PartitionKey, row])
  );

  let cardsHtml = '';
  for (const gw of gatewayRows) {
    const gwId = gw.PartitionKey?.replace('g:', '') || '—';

    // Compute BIP-39 fingerprint locally from x25519_public_key (WEB-1001).
    let fingerprintHtml = '<span class="muted">unavailable</span>';
    const pubKeyRaw = gw.x25519_public_key
      ? base64ToBytes(gw.x25519_public_key)
      : null;
    if (pubKeyRaw && pubKeyRaw.length === 32) {
      const words = await computeBip39Fingerprint(pubKeyRaw);
      if (words) {
        fingerprintHtml = `<code>${words.map(escapeHtml).join(' ')}</code>`;
      }
    }

    const epoch = gw.master_key_epoch ?? '—';
    const mkidBytes = gw.master_key_id ? base64ToBytes(gw.master_key_id) : null;
    const mkid = mkidBytes && mkidBytes.length > 0
      ? bytesToHex(mkidBytes)
      : '—';
    const rotInProgress = gw.rotation_in_progress === true ? 'Yes' : 'No';
    const gwVersion = gw.gateway_version || '—';
    const modemVersion = gw.modem_firmware_version || '—';
    const channel = gw.channel ?? '—';

    // Gateway convergence badge (WEB-1009).
    const desired = desiredByPartition.get(gw.PartitionKey);
    const diverged = computeGatewayDivergence(gw, desired);
    const badgeHtml = `<span class="badge ${diverged ? 'warning' : 'success'}">${diverged ? 'Diverged' : 'Aligned'}</span>`;

    const rotationDisabled = !hasRotationCrypto();
    const rotateBtn = rotationDisabled
      ? '<button class="secondary" disabled title="Browser lacks required crypto capabilities (Web Crypto + WebAssembly)">Rotate Key</button>'
      : `<button class="secondary rotate-key-btn" data-gateway-id="${escapeHtml(gwId)}">Rotate Key</button>`;

    const formExpanded = APP.rotationFormOpen === gwId;
    const formStyle = formExpanded ? '' : ' style="display:none"';

    cardsHtml += `
      <div class="panel stack" data-gateway-card="${escapeHtml(gwId)}">
        <h2>Gateway ${escapeHtml(gwId.slice(0, 8))}… ${badgeHtml}</h2>
        <div class="kv-grid">
          <div class="kv"><strong>Fingerprint</strong> ${fingerprintHtml}</div>
          <div class="kv"><strong>Epoch</strong> ${escapeHtml(epoch)}</div>
          <div class="kv"><strong>Master Key ID</strong> <code>${mkid === '—' ? '—' : escapeHtml(mkid.slice(0, 16)) + '…'}</code></div>
          <div class="kv"><strong>Rotation in progress</strong> ${escapeHtml(rotInProgress)}</div>
          <div class="kv"><strong>Gateway version</strong> ${escapeHtml(gwVersion)}</div>
          <div class="kv"><strong>Modem version</strong> ${escapeHtml(modemVersion)}</div>
          <div class="kv"><strong>Channel</strong> ${escapeHtml(channel)}</div>
        </div>
        ${rotateBtn}
        <div class="rotation-form-container" data-gateway-form="${escapeHtml(gwId)}"${formStyle}>
          <hr>
          <p><strong>Step 1:</strong> Verify the fingerprint above matches the modem display.</p>
          <form class="rotation-form form-grid" data-gateway-id="${escapeHtml(gwId)}">
            <label>Rotation Code (from modem)
              <input name="rotationCode" type="text" maxlength="6" pattern="[A-Za-z0-9]{6}"
                placeholder="ABC123" autocomplete="off" style="text-transform:uppercase" required>
            </label>
            <label>Passphrase (≥20 chars or 6+ words)
              <input name="passphrase" type="password"
                placeholder="Enter passphrase…" required>
            </label>
            <label>Deployment Label
              <input name="deploymentLabel" type="text"
                placeholder="e.g. Alan's production deployment" required>
            </label>
            <div class="rotation-status" data-gateway-status="${escapeHtml(gwId)}"></div>
            <div style="display:flex;gap:0.5rem">
              <button type="submit" class="primary rotation-submit-btn">Rotate</button>
              <button type="button" class="secondary rotation-cancel-btn" data-gateway-id="${escapeHtml(gwId)}">Cancel</button>
            </div>
          </form>
        </div>
      </div>
    `;
  }
  return cardsHtml;
}

function hasRotationCrypto() {
  try {
    return typeof crypto !== 'undefined'
      && typeof crypto.subtle !== 'undefined'
      && typeof crypto.subtle.importKey === 'function'
      && typeof crypto.subtle.deriveBits === 'function'
      && typeof crypto.getRandomValues === 'function'
      && typeof WebAssembly !== 'undefined'
      && typeof argon2 !== 'undefined'
      && (typeof noble_curves_x25519 !== 'undefined' || typeof nobleEd25519 !== 'undefined');
  } catch {
    return false;
  }
}

// 8b. Inline rotation form (WEB-1002, WEB-1009)
// Toggles the inline rotation form within a gateway card and pauses
// auto-refresh while the form is open to prevent DOM replacement.
function toggleRotationForm(gwId) {
  const container = document.querySelector(`.rotation-form-container[data-gateway-form="${gwId}"]`);
  if (!container) return;
  // Block any toggle while any form has an in-flight submission.
  if (document.querySelector('.rotation-form-container[data-submitting="1"]')) return;

  const isOpen = container.style.display !== 'none';
  if (isOpen) {
    closeRotationForm(gwId);
  } else {
    // Close the currently-open form (if any) before opening the new one.
    if (APP.rotationFormOpen) {
      const prev = document.querySelector(
        `.rotation-form-container[data-gateway-form="${APP.rotationFormOpen}"]`
      );
      if (prev) resetAndHideFormContainer(prev);
    }
    container.style.display = '';
    APP.rotationFormOpen = gwId;
    // Pause auto-refresh while form is open.
    clearRefresh();
    // Focus the first input.
    container.querySelector('input')?.focus();
  }
}

// Resets form inputs, clears status messages, and hides a form container.
function resetAndHideFormContainer(container) {
  const form = container.querySelector('.rotation-form');
  if (form) form.reset();
  const status = container.querySelector('.rotation-status');
  if (status) status.innerHTML = '';
  container.style.display = 'none';
}

// Idempotent close — safe to call from timeout even if user already closed.
function closeRotationForm(gwId) {
  if (APP.rotationFormOpen !== gwId) return;
  const container = document.querySelector(`.rotation-form-container[data-gateway-form="${gwId}"]`);
  if (container) resetAndHideFormContainer(container);
  APP.rotationFormOpen = null;
  // Resume auto-refresh only if still on the dashboard tab (WEB-1002 AC#5).
  if (APP.activeTab === 'dashboard') {
    setAutoRefresh(async () => {
      if (APP.activeTab === 'dashboard') await renderDashboard();
    });
  }
}

// Attaches event handlers for rotation forms and buttons after dashboard
// DOM is inserted. Called from renderDashboard().
function attachRotationFormHandlers(gatewayRows) {
  // Rotate Key button toggles inline form.
  for (const btn of document.querySelectorAll('.rotate-key-btn')) {
    btn.addEventListener('click', () => {
      toggleRotationForm(btn.dataset.gatewayId);
    });
  }

  // Cancel button collapses the form.
  for (const btn of document.querySelectorAll('.rotation-cancel-btn')) {
    btn.addEventListener('click', () => {
      toggleRotationForm(btn.dataset.gatewayId);
    });
  }

  // Submit handler for each rotation form.
  for (const form of document.querySelectorAll('.rotation-form')) {
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      const gwId = form.dataset.gatewayId;
      const gwRow = gatewayRows.find(
        (r) => (r.PartitionKey?.replace('g:', '') || '') === gwId
      );
      if (!gwRow) return;

      const code = (form.rotationCode?.value || '').toUpperCase().trim();
      const passphrase = form.passphrase?.value || '';
      const deploymentLabel = (form.deploymentLabel?.value || '').trim();
      const statusEl = document.querySelector(`.rotation-status[data-gateway-status="${gwId}"]`);
      const submitBtn = form.querySelector('.rotation-submit-btn');

      if (!/^[A-Z0-9]{6}$/.test(code)) {
        if (statusEl) statusEl.innerHTML = '<div class="alert error">Rotation code must be 6 alphanumeric characters.</div>';
        return;
      }
      if (passphrase.length < 20 && passphrase.split(/\s+/).filter((w) => w).length < 6) {
        if (statusEl) statusEl.innerHTML = '<div class="alert error">Passphrase must be ≥20 characters or 6+ space-separated words.</div>';
        return;
      }
      if (!deploymentLabel) {
        if (statusEl) statusEl.innerHTML = '<div class="alert error">Deployment label must not be empty.</div>';
        return;
      }

      const formContainer = document.querySelector(`.rotation-form-container[data-gateway-form="${gwId}"]`);
      const cancelBtn = formContainer?.querySelector('.rotation-cancel-btn');

      if (submitBtn) submitBtn.disabled = true;
      if (cancelBtn) cancelBtn.disabled = true;
      if (formContainer) formContainer.dataset.submitting = '1';
      if (statusEl) statusEl.innerHTML = '<p class="muted">Deriving key (Argon2id)… this may take a few seconds.</p>';

      const epoch = Number(gwRow.master_key_epoch);
      if (!Number.isSafeInteger(epoch) || epoch < 0) {
        if (statusEl) statusEl.innerHTML = '<div class="alert error">Invalid or missing master_key_epoch in gateway row.</div>';
        return;
      }

      try {
        const payload = await buildRotationPayload(gwRow, code, passphrase, deploymentLabel);
        if (statusEl) statusEl.innerHTML = '<p class="muted">Submitting rotation…</p>';
        await submitRotationPayload(gwId, payload, epoch);
        if (statusEl) statusEl.innerHTML = '<div class="alert success">Rotation submitted.</div>';
        // Collapse form after short delay to show success message.
        setTimeout(() => closeRotationForm(gwId), 1500);
      } catch (error) {
        const msg = error instanceof Error ? error.message : String(error);
        if (statusEl) statusEl.innerHTML = `<div class="alert error">${escapeHtml(msg)}</div>`;
      } finally {
        // Best-effort: clear passphrase and deployment label from DOM inputs.
        if (form.passphrase) form.passphrase.value = '';
        if (form.deploymentLabel) form.deploymentLabel.value = '';
        if (submitBtn) submitBtn.disabled = false;
        if (cancelBtn) cancelBtn.disabled = false;
        if (formContainer) delete formContainer.dataset.submitting;
      }
    });
  }
}

async function buildRotationPayload(gatewayRow, rotationCode, passphrase, deploymentLabel) {
  const gwId = gatewayRow.PartitionKey?.replace('g:', '') || '';
  const gwIdBytes = hexToBytes(gwId);
  if (!gwIdBytes || gwIdBytes.length !== 16) throw new Error('Invalid gateway_id (expected 16 bytes)');

  const pubKeyB64 = gatewayRow.x25519_public_key;
  if (!pubKeyB64) throw new Error('Gateway has no x25519_public_key');
  const gwPubKey = base64ToBytes(pubKeyB64);
  if (!gwPubKey || gwPubKey.length !== 32) throw new Error('Invalid x25519_public_key');

  const epoch = Number(gatewayRow.master_key_epoch);
  if (!Number.isSafeInteger(epoch) || epoch < 0) throw new Error('Invalid master_key_epoch');

  // KDF v1: hardcoded Argon2id params (GW-2020)
  const mCost = 65536, tCost = 3, pCost = 1;

  // Derive salt from deployment label (GW-2021)
  if (!deploymentLabel) throw new Error('Deployment label must not be empty');
  const saltInput = new TextEncoder().encode('sonde-kdf-v1:' + deploymentLabel);
  const saltHashBuf = await crypto.subtle.digest('SHA-256', saltInput);
  const salt = new Uint8Array(saltHashBuf).slice(0, 16);

  // Derive master key via Argon2id (WEB-1003)
  if (typeof argon2 === 'undefined') {
    throw new Error('Argon2id WASM library not loaded. Rotation requires argon2-browser.');
  }
  const argonResult = await argon2.hash({
    pass: passphrase,
    salt,
    time: tCost,
    mem: mCost,
    parallelism: pCost,
    hashLen: 32,
    type: argon2.ArgonType.Argon2id,
  });
  const newMasterKey = new Uint8Array(argonResult.hash);

  // master_key_id is derived by gateway as SHA-256(master_key) — not included in payload.

  // X25519 key exchange (WEB-1004)
  if (typeof nobleEd25519 === 'undefined' && typeof noble_curves_x25519 === 'undefined') {
    throw new Error('Noble curves library not loaded. Rotation requires @noble/curves.');
  }
  const x25519Lib = typeof noble_curves_x25519 !== 'undefined'
    ? noble_curves_x25519
    : nobleEd25519?.x25519;
  if (!x25519Lib) throw new Error('X25519 not available from noble-curves');

  const ephemeralPrivate = new Uint8Array(32);
  crypto.getRandomValues(ephemeralPrivate);
  const ephemeralPublic = x25519Lib.getPublicKey(ephemeralPrivate);
  const sharedSecret = x25519Lib.getSharedSecret(ephemeralPrivate, gwPubKey);

  // HKDF-SHA-256 (WEB-1004)
  const epochBe64 = new Uint8Array(8);
  new DataView(epochBe64.buffer).setBigUint64(0, BigInt(epoch));
  const hkdfInfo = new Uint8Array(gwIdBytes.length + 8);
  hkdfInfo.set(gwIdBytes, 0);
  hkdfInfo.set(epochBe64, gwIdBytes.length);

  const hkdfKey = await crypto.subtle.importKey('raw', sharedSecret, 'HKDF', false, ['deriveBits']);
  const derivedBits = await crypto.subtle.deriveBits(
    { name: 'HKDF', hash: 'SHA-256', salt: new TextEncoder().encode('sonde-rotation-v1'), info: hkdfInfo },
    hkdfKey,
    256,
  );
  const aesKey = new Uint8Array(derivedBits);

  // CBOR plaintext: {1: new_master_key, 2: rotation_code}
  // Keys 3-5 are RESERVED (previously new_master_key_id, salt, kdf_params).
  const plaintext = encodeCborRotationPlaintext(newMasterKey, rotationCode);

  // AES-256-GCM encryption
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const aad = hkdfInfo; // gateway_id_raw || epoch_be64

  const cryptoKey = await crypto.subtle.importKey('raw', aesKey, { name: 'AES-GCM' }, false, ['encrypt']);
  const ciphertextAndTag = await crypto.subtle.encrypt(
    { name: 'AES-GCM', iv: nonce, additionalData: aad },
    cryptoKey,
    plaintext,
  );

  // Build final payload: version(1) || ephemeral_public(32) || nonce(12) || ciphertext_and_tag
  const ct = new Uint8Array(ciphertextAndTag);
  const result = new Uint8Array(1 + 32 + 12 + ct.length);
  result[0] = 0x01; // version
  result.set(ephemeralPublic, 1);
  result.set(nonce, 33);
  result.set(ct, 45);

  // Best-effort key hygiene (WEB-1008)
  try {
    newMasterKey.fill(0);
    aesKey.fill(0);
    ephemeralPrivate.fill(0);
    sharedSecret.fill(0);
  } catch { /* best effort */ }

  return result;
}

function encodeCborRotationPlaintext(masterKey, rotationCode) {
  // Minimal deterministic CBOR encoder for the rotation plaintext map.
  // Map with 2 entries: {1: bstr, 2: tstr}
  // Keys 3-5 are RESERVED (previously new_master_key_id, salt, kdf_params).
  const parts = [];
  // CBOR map header (2 items)
  parts.push(0xa2);
  // Key 1: new_master_key (bstr 32)
  parts.push(0x01);
  parts.push(0x58, 0x20); // bstr(32)
  for (let i = 0; i < masterKey.length; i++) parts.push(masterKey[i]);
  // Key 2: rotation_code (tstr)
  parts.push(0x02);
  const codeBytes = new TextEncoder().encode(rotationCode);
  parts.push(0x60 + codeBytes.length);
  for (let i = 0; i < codeBytes.length; i++) parts.push(codeBytes[i]);
  return new Uint8Array(parts);
}

async function submitRotationPayload(gwId, payloadBytes, epoch) {
  const partitionKey = `g:${gwId}`;
  const nowMs = Date.now();
  const rowKey = desiredRowKey(nowMs);
  const chunkSize = 8192;
  const chunks = [];
  for (let i = 0; i < payloadBytes.length; i += chunkSize) {
    chunks.push(String.fromCharCode.apply(null, payloadBytes.subarray(i, i + chunkSize)));
  }
  const b64Payload = btoa(chunks.join(''));

  const entity = {
    PartitionKey: partitionKey,
    RowKey: rowKey,
    rotation_payload: b64Payload,
    'rotation_payload@odata.type': 'Edm.Binary',
    submitted_epoch: String(epoch),
    'submitted_epoch@odata.type': 'Edm.Int64',
    timestamp_ms: String(nowMs),
    'timestamp_ms@odata.type': 'Edm.Int64',
  };
  await insertEntity(CONFIG.desiredStateTable, entity);
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

function escapeODataStringLiteral(value) {
  return String(value).replaceAll("'", "''");
}

function entityUrl(tableName, partitionKey, rowKey) {
  const encodedPartition = encodeURIComponent(escapeODataStringLiteral(partitionKey));
  const encodedRow = encodeURIComponent(escapeODataStringLiteral(rowKey));
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

async function queryTable(tableName, filter, { top } = {}) {
  const token = await getToken();
  let allEntities = [];
  let nextPartitionKey = null;
  let nextRowKey = null;
  const maxPages = 10;

  for (let page = 0; page < maxPages; page++) {
    const url = new URL(tableQueryUrl(tableName));
    if (filter) url.searchParams.set('$filter', filter);
    if (top != null) url.searchParams.set('$top', String(top));
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
    if (top != null && allEntities.length >= top) break;
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

async function listPrograms() {
  return sortByDateDesc(await queryTable(CONFIG.programsTable, "PartitionKey eq 'program'"), 'created_at');
}

// 4. Dashboard Tab
async function renderDashboard() {
  if (!APP.account) {
    requireAuthenticatedView('Dashboard');
    return;
  }

  // If rotation form is open and still in the DOM, skip the entire render
  // to preserve form state (WEB-1002 AC#5). Must run before the loading
  // renderCard() call which would destroy the form.
  if (APP.rotationFormOpen) {
    const formStillPresent = document.querySelector(
      `.rotation-form-container[data-gateway-form="${APP.rotationFormOpen}"]`
    );
    if (formStillPresent) return;
    // Stale flag — user navigated away and back; clear and continue.
    APP.rotationFormOpen = null;
  }

  renderCard('Dashboard', '<p class="muted">Loading dashboard…</p>');

  try {
    const [actualRows, desiredRows] = await Promise.all([
      queryTable(CONFIG.actualStateTable, ''),
      queryTable(CONFIG.desiredStateTable, ''),
    ]);

    // Separate gateway and node rows (WEB-1001).
    const gatewayRows = latestByPartition(filterGatewayRows(actualRows));
    const gatewayDesiredRows = latestByPartition(filterGatewayRows(desiredRows));
    const gatewayCardHtml = await renderGatewayStatusCard(gatewayRows, gatewayDesiredRows);

    const nodeRows = filterNodeRows(actualRows);
    const latestActual = latestByPartition(nodeRows).sort((left, right) => String(left.node_id || '').localeCompare(String(right.node_id || '')));
    const nodePartitionKeys = [...new Set(nodeRows.map((row) => row.PartitionKey).filter(Boolean))];
    const desiredByPartition = new Map(latestByPartition(filterNodeRows(desiredRows)).map((row) => [row.PartitionKey, row]));
    initializeDashboardExportRange();

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
          <td>${escapeHtml(actual.wake_rssi_dbm != null ? actual.wake_rssi_dbm + ' dBm' : '—')}</td>
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

    const exportStartValue = formatDateTimeLocalInput(DASHBOARD_EXPORT_STATE.startMs);
    const exportEndValue = formatDateTimeLocalInput(DASHBOARD_EXPORT_STATE.endMs);
    const exportBusyAttr = DASHBOARD_EXPORT_STATE.busy ? ' disabled' : '';

    renderCard('Dashboard', `
      ${gatewayCardHtml}
      <div class="panel dashboard-export-panel">
        <div class="dashboard-export-row">
          <label class="dashboard-export-field">
            <span>Export start</span>
            <input type="datetime-local" id="dashboard-export-start" value="${escapeHtml(exportStartValue)}"${exportBusyAttr}>
          </label>
          <label class="dashboard-export-field">
            <span>Export end</span>
            <input type="datetime-local" id="dashboard-export-end" value="${escapeHtml(exportEndValue)}"${exportBusyAttr}>
          </label>
          <label class="dashboard-export-field">
            <span>Format</span>
            <select id="dashboard-export-format"${exportBusyAttr}>
              <option value="jsonl"${DASHBOARD_EXPORT_STATE.format === 'jsonl' ? ' selected' : ''}>.jsonl</option>
              <option value="csv"${DASHBOARD_EXPORT_STATE.format === 'csv' ? ' selected' : ''}>.csv</option>
            </select>
          </label>
          <button type="button" class="secondary" id="dashboard-export-button"${exportBusyAttr}>${DASHBOARD_EXPORT_STATE.busy ? 'Exporting…' : 'Export'}</button>
        </div>
        <div id="dashboard-export-status">${messageHtml(DASHBOARD_EXPORT_STATE.message)}</div>
      </div>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Node ID</th>
              <th>Battery (mV)</th>
              <th>RSSI</th>
              <th>Firmware</th>
              <th>ABI</th>
              <th>Schedule (s)</th>
              <th>Current Program</th>
              <th>Assigned Program</th>
              <th>Last Seen</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>${rowsHtml || '<tr><td colspan="10" class="muted">No node state found.</td></tr>'}</tbody>
        </table>
      </div>
    `);

    updateDashboardExportControls();

    const exportStartInput = document.getElementById('dashboard-export-start');
    if (exportStartInput) {
      exportStartInput.addEventListener('change', () => {
        DASHBOARD_EXPORT_STATE.startMs = parseDateTimeLocalInput(exportStartInput.value);
      });
    }

    const exportEndInput = document.getElementById('dashboard-export-end');
    if (exportEndInput) {
      exportEndInput.addEventListener('change', () => {
        DASHBOARD_EXPORT_STATE.endMs = parseDateTimeLocalInput(exportEndInput.value);
      });
    }

    const exportFormatSelect = document.getElementById('dashboard-export-format');
    if (exportFormatSelect) {
      exportFormatSelect.addEventListener('change', () => {
        DASHBOARD_EXPORT_STATE.format = exportFormatSelect.value === 'csv' ? 'csv' : 'jsonl';
      });
    }

    const exportButton = document.getElementById('dashboard-export-button');
    if (exportButton) {
      exportButton.addEventListener('click', async () => {
        DASHBOARD_EXPORT_STATE.startMs = parseDateTimeLocalInput(exportStartInput?.value || '');
        DASHBOARD_EXPORT_STATE.endMs = parseDateTimeLocalInput(exportEndInput?.value || '');
        DASHBOARD_EXPORT_STATE.format = exportFormatSelect?.value === 'csv' ? 'csv' : 'jsonl';
        try {
          await exportDeviceData(nodePartitionKeys);
        } catch (error) {
          setDashboardExportMessage('error', parseErrorPayload(error, 'Device export failed.'));
        }
      });
    }

    // Attach inline rotation form handlers (WEB-1002, WEB-1009).
    attachRotationFormHandlers(gatewayRows);
  } catch (error) {
    renderError('Dashboard', error);
  }

  // Only set auto-refresh if rotation form is not open (WEB-1002 AC#5).
  if (!APP.rotationFormOpen) {
    setAutoRefresh(async () => {
      if (APP.activeTab === 'dashboard') {
        await renderDashboard();
      }
    });
  }
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

    const latestActual = latestByPartition(filterNodeRows(actualRows))
      .filter((node) => node.node_id)
      .sort((left, right) =>
        String(left.node_id || '').localeCompare(String(right.node_id || '')));
    const desiredByPartition = new Map(
      latestByPartition(filterNodeRows(desiredRows)).map((row) => [row.PartitionKey, row]));

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

// Series display overrides are stored with the active environment's Sensor Data
// preferences rather than in a global key.

function loadSeriesOverrides() {
  const envs = loadEnvironments();
  const activeName = getActiveEnvironmentName();
  const env = envs.find((entry) => entry.name === activeName) || envs[0] || null;
  return env ? normalizeSeriesOverrides(env.sensorData.seriesOverrides) : {};
}

function saveSeriesOverrides(overrides) {
  return updateActiveEnvironmentSensorData((sensorData) => {
    sensorData.seriesOverrides = normalizeSeriesOverrides(overrides);
  });
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

function applySensorDataPreferences(preferences) {
  const normalized = sanitizeSensorDataPreferences(preferences);
  SENSOR_STATE.timeRange = normalized.timeRange;
  SENSOR_STATE.viewMode = normalized.viewMode;
  SENSOR_STATE.selectedSeries = new Set(normalized.selectedSeries);
  SENSOR_STATE.seriesInitialized = normalized.selectedSeriesInitialized === true;
}

function updateActiveEnvironmentSensorData(updater) {
  const activeName = getActiveEnvironmentName();
  if (!activeName) {
    return false;
  }
  const envs = loadEnvironments();
  const envIndex = envs.findIndex((entry) => entry.name === activeName);
  if (envIndex < 0) {
    return false;
  }
  const nextEnv = normalizeEnvironmentRecord(envs[envIndex]);
  const nextSensorData = sanitizeSensorDataPreferences(nextEnv.sensorData);
  updater(nextSensorData);
  nextEnv.sensorData = nextSensorData;
  const nextEnvs = [...envs];
  nextEnvs[envIndex] = nextEnv;
  return saveEnvironments(nextEnvs);
}

function persistActiveSensorDataPreferences() {
  const currentOverrides = loadSeriesOverrides();
  return updateActiveEnvironmentSensorData((sensorData) => {
    sensorData.viewMode = SENSOR_VIEW_MODES.has(SENSOR_STATE.viewMode) ? SENSOR_STATE.viewMode : 'graph';
    sensorData.timeRange = SENSOR_TIME_RANGES.has(SENSOR_STATE.timeRange) ? SENSOR_STATE.timeRange : '24h';
    sensorData.selectedSeries = [...SENSOR_STATE.selectedSeries].filter((value) => typeof value === 'string');
    sensorData.selectedSeriesInitialized = SENSOR_STATE.seriesInitialized === true || sensorData.selectedSeries.length > 0;
    sensorData.seriesOverrides = normalizeSeriesOverrides(currentOverrides);
  });
}

function persistActiveSensorDataPreferencesOrWarn() {
  if (persistActiveSensorDataPreferences()) {
    return true;
  }
  showViewMessage('error', 'Failed to save Sensor Data preferences. Browser storage may be disabled or full.');
  return false;
}

function clearPersistedSelectedSeriesPreference() {
  return updateActiveEnvironmentSensorData((sensorData) => {
    sensorData.selectedSeries = [];
    sensorData.selectedSeriesInitialized = false;
  });
}

function clearPersistedSelectedSeriesPreferenceOrWarn() {
  if (clearPersistedSelectedSeriesPreference()) {
    return true;
  }
  showViewMessage('error', 'Failed to save Sensor Data preferences. Browser storage may be disabled or full.');
  return false;
}

function resetTransientSensorDataState() {
  SENSOR_STATE.autoRefresh = false;
  SENSOR_STATE.exportStartMs = null;
  SENSOR_STATE.exportEndMs = null;
  SENSOR_STATE.exportFormat = 'jsonl';
  SENSOR_STATE.exportBusy = false;
  SENSOR_STATE.exportMessage = null;
}

function pruneUnavailableSelectedSeries(selectedSeries, currentPlottableKeys) {
  let changed = false;
  for (const key of [...selectedSeries]) {
    if (!currentPlottableKeys.has(key)) {
      selectedSeries.delete(key);
      changed = true;
    }
  }
  return changed;
}

const SENSOR_STATE = {
  timeRange: '24h',
  viewMode: 'graph',
  selectedSeries: new Set(),
  seriesInitialized: false,
  autoRefresh: false,
  exportStartMs: null,
  exportEndMs: null,
  exportFormat: 'jsonl',
  exportBusy: false,
  exportMessage: null,
};

const TIME_RANGE_MS = {
  '1h': 60 * 60 * 1000,
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
};
const SENSOR_EXPORT_MAX_PAGES_PER_PARTITION = 1000;

function reverseTimestampHex(ms) {
  const max = BigInt('0xffffffffffffffff');
  return (max - BigInt(ms)).toString(16).padStart(16, '0');
}

function historyTableFilter(partitionKey, startMs, endMs) {
  const rkStart = reverseTimestampHex(endMs);
  const rkEnd = reverseTimestampHex(startMs);
  return `PartitionKey eq '${escapeODataStringLiteral(partitionKey)}' and RowKey ge '${rkStart}' and RowKey le '${rkEnd}~'`;
}

function sensorDataFilter(partitionKey, startMs, endMs) {
  return historyTableFilter(partitionKey, startMs, endMs);
}

function actualStateFilter(partitionKey, startMs, endMs) {
  return historyTableFilter(partitionKey, startMs, endMs);
}

function initializeSensorExportRange() {
  if (Number.isFinite(SENSOR_STATE.exportStartMs) && Number.isFinite(SENSOR_STATE.exportEndMs)) {
    return;
  }
  const endMs = Date.now();
  SENSOR_STATE.exportEndMs = endMs;
  SENSOR_STATE.exportStartMs = endMs - TIME_RANGE_MS['24h'];
}

function formatDateTimeLocalInput(timestampMs) {
  if (!Number.isFinite(timestampMs)) {
    return '';
  }
  const date = new Date(timestampMs);
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  const hours = String(date.getHours()).padStart(2, '0');
  const minutes = String(date.getMinutes()).padStart(2, '0');
  return `${year}-${month}-${day}T${hours}:${minutes}`;
}

function parseDateTimeLocalInput(value) {
  if (!value) {
    return null;
  }
  const timestampMs = new Date(value).getTime();
  return Number.isFinite(timestampMs) ? timestampMs : null;
}

function setSensorExportMessage(kind, text) {
  SENSOR_STATE.exportMessage = { kind, text };
  const host = contentEl.querySelector('#sensor-export-status');
  if (host) {
    host.innerHTML = messageHtml(SENSOR_STATE.exportMessage);
  }
}

function updateSensorExportControls() {
  const startInput = document.getElementById('sensor-export-start');
  const endInput = document.getElementById('sensor-export-end');
  const formatSelect = document.getElementById('sensor-export-format');
  const exportButton = document.getElementById('sensor-export-button');
  const disabled = SENSOR_STATE.exportBusy;

  if (startInput) startInput.disabled = disabled;
  if (endInput) endInput.disabled = disabled;
  if (formatSelect) formatSelect.disabled = disabled;
  if (exportButton) {
    exportButton.disabled = disabled;
    exportButton.textContent = disabled ? 'Exporting…' : 'Export';
  }
}

async function querySensorDataRange(partitionKeys, startMs, endMs, options = {}) {
  return queryPartitionedTableRange(CONFIG.sensorDataTable, partitionKeys, startMs, endMs, {
    ...options,
    filterBuilder: sensorDataFilter,
    repeatedTokenLabel: 'Sensor export failed',
  });
}

async function queryPartitionedTableRange(tableName, partitionKeys, startMs, endMs, options = {}) {
  const token = await getToken();
  const {
    topPerPage = 1000,
    maxPagesPerPartition = 1,
    requireComplete = false,
    filterBuilder = historyTableFilter,
    repeatedTokenLabel = 'History export failed',
  } = options;

  const fetchPartition = async (pk) => {
    const filter = filterBuilder(pk, startMs, endMs);
    let nextPartitionKey = null;
    let nextRowKey = null;
    const entities = [];
    const seenContinuationTokens = new Set();

    for (let page = 0; page < maxPagesPerPartition; page++) {
      const url = new URL(tableQueryUrl(tableName));
      url.searchParams.set('$filter', filter);
      if (topPerPage != null) {
        url.searchParams.set('$top', String(topPerPage));
      }
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
        throw new Error(`Table query failed for ${tableName} (${response.status}): ${text}`);
      }

      const payload = await response.json();
      if (Array.isArray(payload.value)) {
        entities.push(...payload.value);
      }

      nextPartitionKey = response.headers.get('x-ms-continuation-NextPartitionKey');
      nextRowKey = response.headers.get('x-ms-continuation-NextRowKey');
      if (!nextPartitionKey) {
        break;
      }
      const continuationToken = `${nextPartitionKey}\n${nextRowKey || ''}`;
      if (seenContinuationTokens.has(continuationToken)) {
        throw new Error(
          `${repeatedTokenLabel}: Azure Tables returned a repeated continuation token for node partition ${pk}. Try again or narrow the export time range.`
        );
      }
      seenContinuationTokens.add(continuationToken);
    }

    if (requireComplete && nextPartitionKey) {
      throw new Error(
        `${repeatedTokenLabel}: Azure Tables returned more than ${maxPagesPerPartition} page(s) for node partition ${pk}. Narrow the export time range and try again.`
      );
    }

    return entities;
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

async function queryActualStateRange(partitionKeys, startMs, endMs, options = {}) {
  return queryPartitionedTableRange(CONFIG.actualStateTable, partitionKeys, startMs, endMs, {
    ...options,
    filterBuilder: actualStateFilter,
    repeatedTokenLabel: 'Device export failed',
  });
}

function parseSensorReadingsForExport(decodedReadings) {
  if (!decodedReadings || decodedReadings === '') {
    return null;
  }
  let parsed;
  try {
    parsed = JSON.parse(decodedReadings);
  } catch {
    throw new Error('Sensor export failed: `decoded_readings` is not valid JSON.');
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error('Sensor export failed: `decoded_readings` must be a JSON object.');
  }
  return parsed;
}

function csvEscape(value) {
  const text = value == null ? '' : String(value);
  return /[",\r\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

function sensorExportFilename(format) {
  return `sensor-data.${format}`;
}

function initializeDashboardExportRange() {
  if (Number.isFinite(DASHBOARD_EXPORT_STATE.startMs) && Number.isFinite(DASHBOARD_EXPORT_STATE.endMs)) {
    return;
  }
  const endMs = Date.now();
  DASHBOARD_EXPORT_STATE.endMs = endMs;
  DASHBOARD_EXPORT_STATE.startMs = endMs - TIME_RANGE_MS['24h'];
}

function setDashboardExportMessage(kind, text) {
  DASHBOARD_EXPORT_STATE.message = { kind, text };
  const host = contentEl.querySelector('#dashboard-export-status');
  if (host) {
    host.innerHTML = messageHtml(DASHBOARD_EXPORT_STATE.message);
  }
}

function updateDashboardExportControls() {
  const startInput = document.getElementById('dashboard-export-start');
  const endInput = document.getElementById('dashboard-export-end');
  const formatSelect = document.getElementById('dashboard-export-format');
  const exportButton = document.getElementById('dashboard-export-button');
  const disabled = DASHBOARD_EXPORT_STATE.busy;

  if (startInput) startInput.disabled = disabled;
  if (endInput) endInput.disabled = disabled;
  if (formatSelect) formatSelect.disabled = disabled;
  if (exportButton) {
    exportButton.disabled = disabled;
    exportButton.textContent = disabled ? 'Exporting…' : 'Export';
  }
}

function buildDeviceExportCsv(rows) {
  const header = [
    'timestamp_ms',
    'node_id',
    'battery_mv',
    'wake_rssi_dbm',
    'firmware_version',
    'firmware_abi_version',
    'observed_schedule_interval_s',
    'observed_current_program_hash',
    'observed_assigned_program_hash',
  ];
  const lines = [header.join(',')];
  const sorted = [...rows].sort((a, b) => (Number(a.timestamp_ms) || 0) - (Number(b.timestamp_ms) || 0));
  for (const row of sorted) {
    lines.push([
      csvEscape(row.timestamp_ms ?? ''),
      csvEscape(row.node_id ?? ''),
      csvEscape(row.battery_mv ?? ''),
      csvEscape(row.wake_rssi_dbm ?? ''),
      csvEscape(row.firmware_version ?? ''),
      csvEscape(row.firmware_abi_version ?? ''),
      csvEscape(row.observed_schedule_interval_s ?? ''),
      csvEscape(row.observed_current_program_hash ?? ''),
      csvEscape(row.observed_assigned_program_hash ?? ''),
    ].join(','));
  }
  return lines.join('\r\n');
}

function buildDeviceExportJsonl(rows) {
  const sorted = [...rows].sort((a, b) => (Number(a.timestamp_ms) || 0) - (Number(b.timestamp_ms) || 0));
  return sorted.map((row) => JSON.stringify({
    timestamp_ms: row.timestamp_ms ?? null,
    node_id: row.node_id ?? null,
    battery_mv: row.battery_mv ?? null,
    wake_rssi_dbm: row.wake_rssi_dbm ?? null,
    firmware_version: row.firmware_version ?? null,
    firmware_abi_version: row.firmware_abi_version ?? null,
    observed_schedule_interval_s: row.observed_schedule_interval_s ?? null,
    observed_current_program_hash: row.observed_current_program_hash ?? null,
    observed_assigned_program_hash: row.observed_assigned_program_hash ?? null,
  })).join('\n');
}

function deviceExportFilename(format) {
  return `device-data.${format}`;
}

async function exportDeviceData(partitionKeys) {
  const startMs = DASHBOARD_EXPORT_STATE.startMs;
  const endMs = DASHBOARD_EXPORT_STATE.endMs;
  const format = DASHBOARD_EXPORT_STATE.format;

  if (!Number.isFinite(startMs) || !Number.isFinite(endMs)) {
    throw new Error('Select both export start and end times.');
  }
  if (startMs > endMs) {
    throw new Error('Export start time must be earlier than or equal to the end time.');
  }

  DASHBOARD_EXPORT_STATE.busy = true;
  updateDashboardExportControls();

  try {
    const rows = await queryActualStateRange(partitionKeys, startMs, endMs, {
      topPerPage: null,
      maxPagesPerPartition: SENSOR_EXPORT_MAX_PAGES_PER_PARTITION,
      requireComplete: true,
    });
    const content = format === 'csv' ? buildDeviceExportCsv(rows) : buildDeviceExportJsonl(rows);
    const blob = new Blob([content], {
      type: format === 'csv' ? 'text/csv;charset=utf-8' : 'application/x-ndjson',
    });
    downloadBlob(deviceExportFilename(format), blob);
    setDashboardExportMessage('success', `Exported ${rows.length} device row(s) as .${format}.`);
  } finally {
    DASHBOARD_EXPORT_STATE.busy = false;
    updateDashboardExportControls();
  }
}

function buildSensorExportCsv(rows) {
  const header = ['timestamp_ms', 'node_id', 'program_hash', 'raw_payload', 'decoded_readings_json'];
  const lines = [header.join(',')];
  const sorted = [...rows].sort((a, b) => (Number(a.timestamp_ms) || 0) - (Number(b.timestamp_ms) || 0));
  for (const row of sorted) {
    lines.push([
      csvEscape(row.timestamp_ms || ''),
      csvEscape(row.node_id || ''),
      csvEscape(row.program_hash || ''),
      csvEscape(row.raw_payload || ''),
      csvEscape(row.decoded_readings || ''),
    ].join(','));
  }
  return lines.join('\r\n');
}

function buildSensorExportJsonl(rows) {
  const sorted = [...rows].sort((a, b) => (Number(a.timestamp_ms) || 0) - (Number(b.timestamp_ms) || 0));
  return sorted.map((row) => JSON.stringify({
    timestamp_ms: row.timestamp_ms || '',
    node_id: row.node_id || '',
    program_hash: row.program_hash || '',
    raw_payload: row.raw_payload || '',
    decoded_readings: parseSensorReadingsForExport(row.decoded_readings),
  })).join('\n');
}

async function exportSensorData(partitionKeys) {
  const startMs = SENSOR_STATE.exportStartMs;
  const endMs = SENSOR_STATE.exportEndMs;
  const format = SENSOR_STATE.exportFormat;

  if (!Number.isFinite(startMs) || !Number.isFinite(endMs)) {
    throw new Error('Select both export start and end times.');
  }
  if (startMs > endMs) {
    throw new Error('Export start time must be earlier than or equal to the end time.');
  }

  SENSOR_STATE.exportBusy = true;
  updateSensorExportControls();

  try {
    const rows = await querySensorDataRange(partitionKeys, startMs, endMs, {
      topPerPage: null,
      maxPagesPerPartition: SENSOR_EXPORT_MAX_PAGES_PER_PARTITION,
      requireComplete: true,
    });
    const content = format === 'csv' ? buildSensorExportCsv(rows) : buildSensorExportJsonl(rows);
    const blob = new Blob([content], {
      type: format === 'csv' ? 'text/csv;charset=utf-8' : 'application/x-ndjson',
    });
    downloadBlob(sensorExportFilename(format), blob);
    setSensorExportMessage('success', `Exported ${rows.length} sensor row(s) as .${format}.`);
  } finally {
    SENSOR_STATE.exportBusy = false;
    updateSensorExportControls();
  }
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
    initializeSensorExportRange();
    const actualRows = await queryTable(CONFIG.actualStateTable, '');
    const latestActual = latestByPartition(filterNodeRows(actualRows)).sort((a, b) =>
      String(a.node_id || '').localeCompare(String(b.node_id || ''))
    );

    const nodeIdMap = new Map(latestActual.map((r) => [r.PartitionKey, r.node_id]));
    const partitionKeys = latestActual.map((r) => r.PartitionKey);
    const hasKnownNodes = partitionKeys.length > 0;

    const rangeMs = TIME_RANGE_MS[SENSOR_STATE.timeRange] || TIME_RANGE_MS['24h'];
    const now = Date.now();
    const sensorRows = hasKnownNodes
      ? await querySensorDataRange(partitionKeys, now - rangeMs, now, {
          topPerPage: 1000,
          maxPagesPerPartition: 1,
        })
      : [];
    const allSeries = extractSeries(sensorRows, nodeIdMap);

    // Prune stale and non-plottable selections before auto-selection
    const currentPlottableKeys = new Set(
      allSeries.filter((s) => s.points.length > 0).map((s) => s.key)
    );
    const hadExplicitSelectionPreference = SENSOR_STATE.seriesInitialized;
    let selectionChanged = pruneUnavailableSelectedSeries(SENSOR_STATE.selectedSeries, currentPlottableKeys);
    if (selectionChanged && SENSOR_STATE.selectedSeries.size === 0) {
      SENSOR_STATE.seriesInitialized = false;
    }
    if (selectionChanged && hadExplicitSelectionPreference) {
      if (SENSOR_STATE.selectedSeries.size === 0) {
        clearPersistedSelectedSeriesPreferenceOrWarn();
      } else {
        persistActiveSensorDataPreferencesOrWarn();
      }
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
    const exportStartValue = formatDateTimeLocalInput(SENSOR_STATE.exportStartMs);
    const exportEndValue = formatDateTimeLocalInput(SENSOR_STATE.exportEndMs);
    const exportBusyAttr = SENSOR_STATE.exportBusy ? ' disabled' : '';

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
        <div class="sensor-export-panel">
          <div class="sensor-export-row">
            <label class="sensor-export-field">
              <span>Export start</span>
              <input type="datetime-local" id="sensor-export-start" value="${escapeHtml(exportStartValue)}"${exportBusyAttr}>
            </label>
            <label class="sensor-export-field">
              <span>Export end</span>
              <input type="datetime-local" id="sensor-export-end" value="${escapeHtml(exportEndValue)}"${exportBusyAttr}>
            </label>
            <label class="sensor-export-field">
              <span>Format</span>
              <select id="sensor-export-format"${exportBusyAttr}>
                <option value="jsonl"${SENSOR_STATE.exportFormat === 'jsonl' ? ' selected' : ''}>.jsonl</option>
                <option value="csv"${SENSOR_STATE.exportFormat === 'csv' ? ' selected' : ''}>.csv</option>
              </select>
            </label>
            <button type="button" class="secondary" id="sensor-export-button"${exportBusyAttr}>${SENSOR_STATE.exportBusy ? 'Exporting…' : 'Export'}</button>
          </div>
          <div id="sensor-export-status">${messageHtml(SENSOR_STATE.exportMessage)}</div>
        </div>
        ${allSeries.length > 0 ? `
          <details class="sensor-series-picker" open>
            <summary><strong>Series</strong> (${allSeries.length} available, max 20 plotted)</summary>
            <div class="sensor-series-grid">${seriesCheckboxes}</div>
          </details>
        ` : ''}
      </div>
      <div class="panel">
        ${!hasKnownNodes
          ? '<p class="muted">No nodes have reported state yet.</p>'
          : SENSOR_STATE.viewMode === 'graph'
            ? '<div class="sensor-chart-area chart-container"><p class="muted">Rendering chart…</p></div>'
            : renderSensorTable(sensorRows, nodeIdMap)}
      </div>
    `);

    if (hasKnownNodes && SENSOR_STATE.viewMode === 'graph') {
      renderSensorChart(allSeries);
    }
    updateSensorExportControls();

    // Attach event handlers
    for (const btn of contentEl.querySelectorAll('.sensor-range-btn')) {
      btn.addEventListener('click', async () => {
        SENSOR_STATE.timeRange = btn.dataset.range;
        persistActiveSensorDataPreferencesOrWarn();
        await renderSensorData();
      });
    }

    for (const btn of contentEl.querySelectorAll('.sensor-view-btn')) {
      btn.addEventListener('click', async () => {
        SENSOR_STATE.viewMode = btn.dataset.view;
        persistActiveSensorDataPreferencesOrWarn();
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
        persistActiveSensorDataPreferencesOrWarn();
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

    const exportStartInput = document.getElementById('sensor-export-start');
    if (exportStartInput) {
      exportStartInput.addEventListener('change', () => {
        SENSOR_STATE.exportStartMs = parseDateTimeLocalInput(exportStartInput.value);
      });
    }

    const exportEndInput = document.getElementById('sensor-export-end');
    if (exportEndInput) {
      exportEndInput.addEventListener('change', () => {
        SENSOR_STATE.exportEndMs = parseDateTimeLocalInput(exportEndInput.value);
      });
    }

    const exportFormatSelect = document.getElementById('sensor-export-format');
    if (exportFormatSelect) {
      exportFormatSelect.addEventListener('change', () => {
        SENSOR_STATE.exportFormat = exportFormatSelect.value === 'csv' ? 'csv' : 'jsonl';
      });
    }

    const exportButton = document.getElementById('sensor-export-button');
    if (exportButton) {
      exportButton.addEventListener('click', async () => {
        SENSOR_STATE.exportStartMs = parseDateTimeLocalInput(exportStartInput?.value || '');
        SENSOR_STATE.exportEndMs = parseDateTimeLocalInput(exportEndInput?.value || '');
        SENSOR_STATE.exportFormat = exportFormatSelect?.value === 'csv' ? 'csv' : 'jsonl';
        try {
          await exportSensorData(partitionKeys);
        } catch (error) {
          setSensorExportMessage('error', parseErrorPayload(error, 'Sensor export failed.'));
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

// 8. Custom Dashboards

const APP_DASHBOARD_STATE = {
  activeDashboardIndex: 0,
  metricCharts: {},
  unsavedEnvironment: null,
};

function setUnsavedDashboardEnvironment(env) {
  APP_DASHBOARD_STATE.unsavedEnvironment = env ? normalizeEnvironmentRecord(env) : null;
}

function clearUnsavedDashboardEnvironment(name = null) {
  if (!name || APP_DASHBOARD_STATE.unsavedEnvironment?.name === name) {
    APP_DASHBOARD_STATE.unsavedEnvironment = null;
  }
}

function persistDashboardEnvironment(env, environments = null) {
  const normalizedEnv = normalizeEnvironmentRecord(env);
  const envs = Array.isArray(environments) ? environments.map(normalizeEnvironmentRecord) : loadEnvironments();
  const envIndex = envs.findIndex((entry) => entry.name === normalizedEnv.name);
  if (envIndex >= 0) {
    envs[envIndex] = normalizedEnv;
  } else {
    envs.push(normalizedEnv);
  }
  if (saveEnvironments(envs)) {
    clearUnsavedDashboardEnvironment(normalizedEnv.name);
    return true;
  }
  setUnsavedDashboardEnvironment(normalizedEnv);
  return false;
}

function destroyDashboardChart(index) {
  const chart = APP_DASHBOARD_STATE.metricCharts[index];
  if (chart && typeof chart.destroy === 'function') {
    chart.destroy();
  }
  delete APP_DASHBOARD_STATE.metricCharts[index];
}

function destroyAllDashboardCharts() {
  for (const index of Object.keys(APP_DASHBOARD_STATE.metricCharts)) {
    destroyDashboardChart(index);
  }
}

function canAddDashboard(env) {
  if (env.dashboards.length >= 20) {
    return {
      allowed: false,
      warning: 'You have reached the recommended limit of 20 dashboards per environment. Creating more may impact performance. Continue anyway?'
    };
  }
  return { allowed: true };
}

function canAddMetric(dashboard) {
  if (dashboard.metrics.length >= 10) {
    return {
      allowed: false,
      warning: 'You have reached the recommended limit of 10 metrics per dashboard. Adding more may impact performance. Continue anyway?'
    };
  }
  return { allowed: true };
}

async function renderDashboards() {
  const content = document.getElementById('content');
  const env = loadActiveEnvironment();
  
  if (!env) {
    content.innerHTML = renderError('Configuration Error', new Error('No environment selected'));
    return;
  }
  
  if (!Array.isArray(env.dashboards)) {
    env.dashboards = createDefaultDashboardsArray();
<<<<<<< HEAD
<<<<<<< HEAD
    const environments = loadEnvironments();
=======
    environments = loadEnvironments();
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
=======
    const environments = loadEnvironments();
>>>>>>> acd7144 (Fix dashboard review feedback)
    persistDashboardEnvironment(env, environments);
  }
  
  if (env.dashboards.length === 0) {
    content.innerHTML = `
      <div class="dashboards-empty">
        <h2>No Dashboards Yet</h2>
        <p>Create a custom dashboard to visualize sensor data with algebraic expressions.</p>
        <button class="btn btn-primary" id="add-first-dashboard-btn">+ Create Dashboard</button>
      </div>
    `;
    document.getElementById('add-first-dashboard-btn')?.addEventListener('click', () => {
      const check = canAddDashboard(env);
      if (!check.allowed && !window.confirm(check.warning)) return;
      showAddDashboardModal();
    });
    return;
  }
  
  if (APP_DASHBOARD_STATE.activeDashboardIndex >= env.dashboards.length) {
    APP_DASHBOARD_STATE.activeDashboardIndex = 0;
  }
  
  const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
  
  content.innerHTML = `
    <div class="dashboards-container">
      ${renderDashboardTabs(env.dashboards, APP_DASHBOARD_STATE.activeDashboardIndex)}
      ${renderDashboardContent(dashboard)}
    </div>
  `;
  
  attachDashboardHandlers();
  if (dashboard.metrics.length > 0) {
    await renderMetricCharts(dashboard);
  }
}

function renderDashboardTabs(dashboards, activeIndex) {
  const tabs = dashboards.map((d, i) => `
    <div class="dashboard-tab-item">
      <button type="button" class="dashboard-tab ${i === activeIndex ? 'active' : ''}" data-dashboard-index="${i}">
        ${escapeHtml(d.name)}
      </button>
      <button type="button" class="dashboard-tab-delete" data-delete-dashboard="${i}" title="Delete dashboard" aria-label="Delete dashboard ${escapeHtml(d.name)}">&times;</button>
    </div>
  `).join('');
  
  return `
    <div class="dashboard-tabs-bar">
      ${tabs}
      <button class="dashboard-tab-add" id="add-dashboard-btn">+</button>
    </div>
  `;
}

function renderDashboardContent(dashboard) {
  const timeRange = normalizeDashboardTimeRange(dashboard.timeRange);
  return `
    <div class="dashboard-header">
      <h2>${escapeHtml(dashboard.name)}</h2>
      <div class="dashboard-header-controls">
        <select id="dashboard-time-range" class="time-range-select">
          <option value="1h" ${timeRange.preset === '1h' ? 'selected' : ''}>Last Hour</option>
          <option value="6h" ${timeRange.preset === '6h' ? 'selected' : ''}>Last 6 Hours</option>
          <option value="24h" ${timeRange.preset === '24h' ? 'selected' : ''}>Last 24 Hours</option>
          <option value="7d" ${timeRange.preset === '7d' ? 'selected' : ''}>Last 7 Days</option>
          <option value="custom" ${timeRange.preset === 'custom' ? 'selected' : ''}>Custom Range</option>
        </select>
        <input
          type="datetime-local"
          id="dashboard-time-start"
          class="time-range-input"
          value="${escapeHtml(formatDateTimeLocalInput(timeRange.start))}"
          ${timeRange.preset === 'custom' ? '' : 'disabled'}
        >
        <input
          type="datetime-local"
          id="dashboard-time-end"
          class="time-range-input"
          value="${escapeHtml(formatDateTimeLocalInput(timeRange.end))}"
          ${timeRange.preset === 'custom' ? '' : 'disabled'}
        >
        <button class="btn btn-secondary" id="edit-dashboard-name-btn">Rename</button>
      </div>
    </div>
    
    <div class="dashboard-variables">
      <h3>Variables <button class="btn btn-sm btn-secondary" id="add-variable-btn">+ Add Variable</button></h3>
      ${renderVariablesList(dashboard.variables)}
    </div>
    
    <div class="dashboard-metrics">
      <h3>Metrics <button class="btn btn-sm btn-primary" id="add-metric-btn">+ Add Metric</button></h3>
      ${dashboard.metrics.length === 0 
        ? '<p class="text-muted">No metrics yet. ' + (dashboard.variables.length === 0 ? 'Add variables first, then ' : '') + 'click "+ Add Metric" above.</p>'
        : dashboard.metrics.map((m, i) => renderMetricCard(m, i)).join('')
      }
    </div>
  `;
}

function renderVariablesList(variables) {
  if (variables.length === 0) {
    return '<p class="text-muted">No variables defined yet.</p>';
  }
  
  const rows = variables.map((v, i) => `
    <tr>
      <td><code>${escapeHtml(v.name)}</code></td>
      <td>${escapeHtml(v.nodeId)} - ${escapeHtml(v.readingType)}</td>
      <td>
        <button class="btn-sm" data-edit-variable="${i}">Edit</button>
        <button class="btn-sm btn-danger" data-delete-variable="${i}">Delete</button>
      </td>
    </tr>
  `).join('');
  
  return `
    <table class="variables-table">
      <thead>
        <tr>
          <th>Variable</th>
          <th>Data Source</th>
          <th>Actions</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>
  `;
}

function renderMetricCard(metric, index) {
  const hasError = metric._validationError;
  const hasWarning = metric._validationWarning;
  return `
    <div class="metric-card ${hasError ? 'metric-error' : ''}" id="metric-${index}">
      <div class="metric-header">
        <h4>${escapeHtml(metric.displayName || `Metric ${index + 1}`)}</h4>
        <div class="metric-actions">
          <button class="btn-sm" data-edit-metric="${index}">Edit</button>
          <button class="btn-sm btn-danger" data-delete-metric="${index}">Delete</button>
        </div>
      </div>
      <div class="metric-expression">
        <code>${escapeHtml(metric.expression)}</code>
        ${hasError ? `<div class="error-text">${escapeHtml(metric._validationError)}</div>` : ''}
        ${!hasError && hasWarning ? `<div class="text-muted">${escapeHtml(metric._validationWarning)}</div>` : ''}
      </div>
      <div class="metric-chart-container">
        <canvas id="metric-chart-${index}"></canvas>
      </div>
    </div>
  `;
}

function getDashboardTimeRangeBounds(timeRange, nowMs = Date.now()) {
  const normalized = normalizeDashboardTimeRange(timeRange);
  if (normalized.preset === 'custom') {
    return {
      startMs: normalized.start,
      endMs: normalized.end,
    };
  }
  const rangeMs = DASHBOARD_TIME_RANGE_MS[normalized.preset] || DASHBOARD_TIME_RANGE_MS['24h'];
  return {
    startMs: nowMs - rangeMs,
    endMs: nowMs,
  };
}

<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> 751eefb (Fix code review round 5 findings F-001 and F-002)
async function renderMetricCharts(dashboard, deps = {}) {
  const evaluateMetricTimeSeriesFn = deps.evaluateMetricTimeSeriesFn || evaluateMetricTimeSeries;
  const downsamplePointsFn = deps.downsamplePointsFn || downsamplePoints;
  const chartFactory = deps.chartFactory || ((canvas, config) => new Chart(canvas, config));
<<<<<<< HEAD
=======
async function renderMetricCharts(dashboard) {
>>>>>>> de06846 (Fix code review round 4 findings F-001 through F-003)
=======
>>>>>>> 751eefb (Fix code review round 5 findings F-001 and F-002)
  for (let i = 0; i < dashboard.metrics.length; i++) {
    const metric = dashboard.metrics[i];
    if (metric._validationError) continue;
    
    const canvas = document.getElementById(`metric-chart-${i}`);
    if (!canvas) continue;
<<<<<<< HEAD
<<<<<<< HEAD
    if (metric._validationWarning) {
      destroyDashboardChart(i);
      canvas.parentElement.innerHTML = `<div class="text-muted">${escapeHtml(metric._validationWarning)}</div>`;
=======
=======
    if (metric._validationWarning) {
      destroyDashboardChart(i);
      canvas.parentElement.innerHTML = `<div class="text-muted">${escapeHtml(metric._validationWarning)}</div>`;
      continue;
    }
>>>>>>> acd7144 (Fix dashboard review feedback)
    
    const result = await evaluateMetricTimeSeriesFn(metric, dashboard.variables, dashboard.timeRange, deps);
    
    if (result.error) {
      destroyDashboardChart(i);
      canvas.parentElement.innerHTML = `<div class="error-text">${escapeHtml(result.error)}</div>`;
>>>>>>> 751eefb (Fix code review round 5 findings F-001 and F-002)
      continue;
    }
    if (result.points.length === 0) {
      destroyDashboardChart(i);
      canvas.parentElement.innerHTML = '<div class="text-muted">No data in selected time range.</div>';
      continue;
    }
    
<<<<<<< HEAD
    const result = await evaluateMetricTimeSeriesFn(metric, dashboard.variables, dashboard.timeRange, deps);
    
    if (result.error) {
      destroyDashboardChart(i);
      canvas.parentElement.innerHTML = `<div class="error-text">${escapeHtml(result.error)}</div>`;
      continue;
    }
    if (result.points.length === 0) {
      destroyDashboardChart(i);
      canvas.parentElement.innerHTML = '<div class="text-muted">No data in selected time range.</div>';
      continue;
    }
=======
    destroyDashboardChart(i);
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
    const chartPoints = downsamplePointsFn(result.points, 500).map(p => ({ x: p.timestamp, y: p.value }));
    
<<<<<<< HEAD
    destroyDashboardChart(i);
    const chartPoints = downsamplePointsFn(result.points, 500).map(p => ({ x: p.timestamp, y: p.value }));
    
=======
>>>>>>> 751eefb (Fix code review round 5 findings F-001 and F-002)
    APP_DASHBOARD_STATE.metricCharts[i] = chartFactory(canvas, {
      type: 'line',
      data: {
        datasets: [{
          label: metric.displayName || metric.expression,
          data: chartPoints,
          borderColor: metric.color || '#007bff',
          backgroundColor: metric.color || '#007bff',
          fill: false,
          tension: 0.1
        }]
      },
      options: {
        responsive: true,
        maintainAspectRatio: false,
        scales: {
          x: {
            type: 'linear',
            ticks: {
              callback: function(value) {
                return new Date(value).toLocaleTimeString();
              }
            }
          },
          y: {
            beginAtZero: false
          }
        }
      }
    });
  }
}

async function evaluateMetricTimeSeries(metric, variables, timeRange, deps = {}) {
  const parserFactory = deps.parserFactory || (() => new exprEval.Parser());
  const fetchVariableDataFn = deps.fetchVariableDataFn || fetchVariableData;
  const parser = parserFactory();
  let expr;
  
  try {
    expr = parser.parse(metric.expression);
  } catch (error) {
    console.error('Expression parse error:', error);
    return { error: `Expression error: ${error.message}`, points: [] };
  }
  
  const usedVariableNames = expr.variables();
  const undefinedVariableNames = usedVariableNames.filter(name => !variables.some(v => v.name === name));
  if (undefinedVariableNames.length > 0) {
    return { error: `Undefined variables: ${undefinedVariableNames.join(', ')}`, points: [] };
  }
  const usedVariables = variables.filter(v => usedVariableNames.includes(v.name));
  
  if (usedVariables.length === 0) {
    return { error: 'Expression uses no variables', points: [] };
  }
  
  const fetchResult = await fetchVariableDataFn(usedVariables, timeRange || { preset: '24h' }, deps);
  
  // Propagate any fetch errors
  if (fetchResult.errors && fetchResult.errors.length > 0) {
    return { error: fetchResult.errors[0], points: [] };  // Show first error
  }
  
  const variableData = fetchResult.data;
  const indexedVariableData = Object.create(null);
  const timestamps = new Set();
  for (const variable of usedVariables) {
    const data = variableData[variable.name] || [];
    const byTimestamp = new Map();
    data.forEach(point => {
      byTimestamp.set(point.timestamp, point.value);
      timestamps.add(point.timestamp);
    });
    indexedVariableData[variable.name] = byTimestamp;
  }
  
  const sortedTimestamps = Array.from(timestamps).sort((a, b) => a - b);
  
  const points = [];
  for (const timestamp of sortedTimestamps) {
    const context = Object.create(null);
    let hasAllVars = true;
    
    for (const variable of usedVariables) {
      const data = indexedVariableData[variable.name];
      if (!data || !data.has(timestamp)) {
        hasAllVars = false;
        break;
      }
      context[variable.name] = data.get(timestamp);
    }
    
    if (!hasAllVars) continue;
    
    try {
      const value = expr.evaluate(context);
      if (Number.isFinite(value)) {
        points.push({ timestamp, value });
      } else {
        console.warn(`Non-finite value at timestamp ${timestamp}: ${value}`);
      }
    } catch (error) {
      console.warn(`Evaluation error at timestamp ${timestamp}:`, error);
    }
  }
  
  return { points };
}

async function fetchVariableData(variables, timeRange, deps = {}) {
<<<<<<< HEAD
<<<<<<< HEAD
  const result = Object.create(null);
=======
  const result = {};
>>>>>>> de06846 (Fix code review round 4 findings F-001 through F-003)
=======
  const result = Object.create(null);
>>>>>>> acd7144 (Fix dashboard review feedback)
  const errors = [];

  const nowFn = deps.nowFn || Date.now;
  const fetchActualStateNodesFn = deps.fetchActualStateNodesFn || fetchActualStateNodes;
  const querySensorDataRangeFn = deps.querySensorDataRangeFn || querySensorDataRange;
  const { startMs, endMs } = getDashboardTimeRangeBounds(timeRange, nowFn());

  // Fetch node mappings to resolve nodeId -> partitionKey
  let nodes;
  try {
    nodes = await fetchActualStateNodesFn(deps);
  } catch (error) {
    errors.push(`Failed to fetch node mappings: ${error.message}`);
    return { data: result, errors };
  }
  
  const nodeIdToPartitionKey = new Map();
  for (const node of nodes) {
    nodeIdToPartitionKey.set(node.nodeId, node.partitionKey);
  }
  
  // Group variables by partition key (node ID) to minimize queries
  const partitionMap = new Map();
  for (const variable of variables) {
    const partitionKey = nodeIdToPartitionKey.get(variable.nodeId);
    if (!partitionKey) {
      errors.push(`Node ID "${variable.nodeId}" not found for variable "${variable.name}"`);
      result[variable.name] = [];
      continue;
    }
    if (!partitionMap.has(partitionKey)) {
      partitionMap.set(partitionKey, []);
    }
    partitionMap.get(partitionKey).push(variable);
  }
  
  // Query sensor data for each partition
  for (const [partitionKey, vars] of partitionMap.entries()) {
    try {
      const rows = await querySensorDataRangeFn([partitionKey], startMs, endMs, {
        topPerPage: 1000,
        maxPagesPerPartition: 10
      });
      const availableReadingTypes = new Set();
      
      // Extract requested readings from each row
      for (const row of rows) {
        const timestampMs = Number(row.timestamp_ms);
        if (!Number.isFinite(timestampMs)) continue;
        
        const readings = parseSensorReadings(row.decoded_readings);
        if (!readings) continue;
        for (const [readingName, readingValue] of Object.entries(readings)) {
          if (toPlottableNumber(readingValue) != null) {
            availableReadingTypes.add(readingName);
          }
        }
        
        // Map each variable to its value at this timestamp
        for (const variable of vars) {
          const value = toPlottableNumber(readings[variable.readingType]);
          if (value != null) {
            if (!result[variable.name]) {
              result[variable.name] = [];
            }
            result[variable.name].push({
              timestamp: timestampMs,
              value
            });
          }
        }
      }
      if (rows.length > 0) {
        for (const variable of vars) {
          if (!availableReadingTypes.has(variable.readingType)) {
            errors.push(`Reading type "${variable.readingType}" not found for node "${variable.nodeId}"`);
            if (!result[variable.name]) {
              result[variable.name] = [];
            }
          }
        }
      }
    } catch (error) {
      const nodeNames = vars.map(v => v.nodeId).join(', ');
      errors.push(`Failed to fetch data for node(s) ${nodeNames}: ${error.message}`);
      for (const variable of vars) {
        result[variable.name] = [];
      }
    }
  }
  
  return { data: result, errors };
}

function attachDashboardHandlers() {
  document.querySelectorAll('.dashboard-tab').forEach(btn => {
    btn.addEventListener('click', (e) => {
      if (e.target.classList.contains('dashboard-tab-delete')) return;
      APP_DASHBOARD_STATE.activeDashboardIndex = parseInt(btn.dataset.dashboardIndex);
      renderActiveTab();
    });
  });
  
  document.querySelectorAll('.dashboard-tab-delete').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const index = parseInt(btn.dataset.deleteDashboard);
      deleteDashboard(index);
    });
  });
  
  document.getElementById('dashboard-time-range')?.addEventListener('change', (e) => {
    const env = loadActiveEnvironment();
    const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
    dashboard.timeRange.preset = e.target.value;
    if (dashboard.timeRange.preset === 'custom' && (!Number.isFinite(dashboard.timeRange.start) || !Number.isFinite(dashboard.timeRange.end) || dashboard.timeRange.start >= dashboard.timeRange.end)) {
      const defaultBounds = getDashboardTimeRangeBounds({ preset: '24h' });
      dashboard.timeRange.start = defaultBounds.startMs;
      dashboard.timeRange.end = defaultBounds.endMs;
    } else if (dashboard.timeRange.preset !== 'custom') {
      dashboard.timeRange.start = null;
      dashboard.timeRange.end = null;
<<<<<<< HEAD
=======
    }
<<<<<<< HEAD
    environments = loadEnvironments();
<<<<<<< HEAD
    const envIndex = environments.findIndex(e => e.name === env.name);
    if (envIndex >= 0) {
      environments[envIndex] = env;
      saveEnvironments(environments);
>>>>>>> de06846 (Fix code review round 4 findings F-001 through F-003)
    }
    const environments = loadEnvironments();
    persistDashboardEnvironment(env, environments);
    renderActiveTab();
  });
  document.getElementById('dashboard-time-start')?.addEventListener('change', (e) => {
    const env = loadActiveEnvironment();
    const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
    const startMs = parseDateTimeLocalInput(e.target.value);
    const endMs = Number.isFinite(dashboard.timeRange.end) ? dashboard.timeRange.end : parseDateTimeLocalInput(document.getElementById('dashboard-time-end')?.value);
    if (startMs == null || endMs == null || startMs >= endMs) {
      alert('Custom time range must have a start before the end.');
      renderActiveTab();
      return;
    }
    dashboard.timeRange = { preset: 'custom', start: startMs, end: endMs };
    const environments = loadEnvironments();
    persistDashboardEnvironment(env, environments);
    renderActiveTab();
  });
  document.getElementById('dashboard-time-end')?.addEventListener('change', (e) => {
    const env = loadActiveEnvironment();
    const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
    const endMs = parseDateTimeLocalInput(e.target.value);
    const startMs = Number.isFinite(dashboard.timeRange.start) ? dashboard.timeRange.start : parseDateTimeLocalInput(document.getElementById('dashboard-time-start')?.value);
    if (startMs == null || endMs == null || startMs >= endMs) {
      alert('Custom time range must have a start before the end.');
      renderActiveTab();
      return;
    }
    dashboard.timeRange = { preset: 'custom', start: startMs, end: endMs };
    const environments = loadEnvironments();
=======
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
=======
    const environments = loadEnvironments();
>>>>>>> acd7144 (Fix dashboard review feedback)
    persistDashboardEnvironment(env, environments);
    renderActiveTab();
  });
  document.getElementById('dashboard-time-start')?.addEventListener('change', (e) => {
    const env = loadActiveEnvironment();
    const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
    const startMs = parseDateTimeLocalInput(e.target.value);
    const endMs = Number.isFinite(dashboard.timeRange.end) ? dashboard.timeRange.end : parseDateTimeLocalInput(document.getElementById('dashboard-time-end')?.value);
    if (startMs == null || endMs == null || startMs >= endMs) {
      alert('Custom time range must have a start before the end.');
      renderActiveTab();
      return;
    }
    dashboard.timeRange = { preset: 'custom', start: startMs, end: endMs };
    const environments = loadEnvironments();
    persistDashboardEnvironment(env, environments);
    renderActiveTab();
  });
  document.getElementById('dashboard-time-end')?.addEventListener('change', (e) => {
    const env = loadActiveEnvironment();
    const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
    const endMs = parseDateTimeLocalInput(e.target.value);
    const startMs = Number.isFinite(dashboard.timeRange.start) ? dashboard.timeRange.start : parseDateTimeLocalInput(document.getElementById('dashboard-time-start')?.value);
    if (startMs == null || endMs == null || startMs >= endMs) {
      alert('Custom time range must have a start before the end.');
      renderActiveTab();
      return;
    }
    dashboard.timeRange = { preset: 'custom', start: startMs, end: endMs };
    const environments = loadEnvironments();
    persistDashboardEnvironment(env, environments);
    renderActiveTab();
  });
  
  document.getElementById('add-dashboard-btn')?.addEventListener('click', () => {
    const env = loadActiveEnvironment();
    const check = canAddDashboard(env);
    if (!check.allowed && !window.confirm(check.warning)) return;
    showAddDashboardModal();
  });
  
  document.getElementById('edit-dashboard-name-btn')?.addEventListener('click', () => {
    const env = loadActiveEnvironment();
    const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
    const newName = window.prompt('Enter new dashboard name:', dashboard.name);
    if (newName && newName.trim()) {
      dashboard.name = newName.trim();
<<<<<<< HEAD
<<<<<<< HEAD
      const environments = loadEnvironments();
=======
      environments = loadEnvironments();
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
=======
      const environments = loadEnvironments();
>>>>>>> acd7144 (Fix dashboard review feedback)
      persistDashboardEnvironment(env, environments);
      renderActiveTab();
    }
  });
  
  document.getElementById('add-variable-btn')?.addEventListener('click', () => showAddVariableModal());
  document.getElementById('add-metric-btn')?.addEventListener('click', () => {
    const env = loadActiveEnvironment();
    const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
    const check = canAddMetric(dashboard);
    if (!check.allowed && !window.confirm(check.warning)) return;
    showAddMetricModal();
  });
  
  document.querySelectorAll('[data-edit-variable]').forEach(btn => {
    btn.addEventListener('click', () => {
      const index = parseInt(btn.dataset.editVariable);
      showEditVariableModal(index);
    });
  });
  
  document.querySelectorAll('[data-delete-variable]').forEach(btn => {
    btn.addEventListener('click', () => {
      const index = parseInt(btn.dataset.deleteVariable);
      deleteVariable(index);
    });
  });
  
  document.querySelectorAll('[data-edit-metric]').forEach(btn => {
    btn.addEventListener('click', () => {
      const index = parseInt(btn.dataset.editMetric);
      showEditMetricModal(index);
    });
  });
  
  document.querySelectorAll('[data-delete-metric]').forEach(btn => {
    btn.addEventListener('click', () => {
      const index = parseInt(btn.dataset.deleteMetric);
      deleteMetric(index);
    });
  });
}

function showAddDashboardModal() {
  const env = loadActiveEnvironment();
  const name = window.prompt('Enter dashboard name:', `Dashboard ${env.dashboards.length + 1}`);
  if (name && name.trim()) {
    env.dashboards.push(createDefaultDashboard(name.trim()));
<<<<<<< HEAD
<<<<<<< HEAD
    const environments = loadEnvironments();
=======
    environments = loadEnvironments();
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
=======
    const environments = loadEnvironments();
>>>>>>> acd7144 (Fix dashboard review feedback)
    persistDashboardEnvironment(env, environments);
    APP_DASHBOARD_STATE.activeDashboardIndex = env.dashboards.length - 1;
    renderActiveTab();
  }
}

function deleteDashboard(index) {
  const env = loadActiveEnvironment();
  const dashboard = env.dashboards[index];
  if (!window.confirm(`Delete dashboard "${dashboard.name}"? This cannot be undone.`)) return;
  
  env.dashboards.splice(index, 1);
<<<<<<< HEAD
<<<<<<< HEAD
  const environments = loadEnvironments();
=======
  environments = loadEnvironments();
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
=======
  const environments = loadEnvironments();
>>>>>>> acd7144 (Fix dashboard review feedback)
  persistDashboardEnvironment(env, environments);
  
  if (APP_DASHBOARD_STATE.activeDashboardIndex >= env.dashboards.length) {
    APP_DASHBOARD_STATE.activeDashboardIndex = Math.max(0, env.dashboards.length - 1);
  }
  renderActiveTab();
}

async function fetchReadingTypesForNode(nodeId, deps = {}) {
  const fetchActualStateNodesFn = deps.fetchActualStateNodesFn || fetchActualStateNodes;
  const querySensorDataRangeFn = deps.querySensorDataRangeFn || querySensorDataRange;
  const nowFn = deps.nowFn || Date.now;
  const nodes = await fetchActualStateNodesFn(deps);
  const node = nodes.find((entry) => entry.nodeId === nodeId);
  if (!node || !node.partitionKey) {
    return [];
  }
  const endMs = nowFn();
  const startMs = endMs - DASHBOARD_READING_DISCOVERY_WINDOW_MS;
  const rows = await querySensorDataRangeFn([node.partitionKey], startMs, endMs, {
    topPerPage: 1000,
    maxPagesPerPartition: 5,
  });
  const readingTypes = new Set();
  for (const row of rows) {
    const readings = parseSensorReadings(row.decoded_readings);
    if (!readings) continue;
    for (const [readingName, value] of Object.entries(readings)) {
      if (toPlottableNumber(value) != null) {
        readingTypes.add(readingName);
      }
    }
  }
  return [...readingTypes].sort();
}

async function showVariableModal(index = null) {
  const isEdit = index != null;
  const env = loadActiveEnvironment();
  const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
  const variable = isEdit ? dashboard.variables[index] : null;
  const nodes = await fetchActualStateNodes();
  if (nodes.length === 0) {
    alert('No nodes available. Nodes must be online and reporting to create variables.');
    return;
  }

  const existing = document.getElementById('dashboard-variable-overlay');
  if (existing) existing.remove();

  const overlay = document.createElement('div');
  overlay.id = 'dashboard-variable-overlay';
  overlay.className = 'env-manager-overlay';
  overlay.setAttribute('role', 'dialog');
  overlay.setAttribute('aria-modal', 'true');
  overlay.setAttribute('aria-label', isEdit ? 'Edit Variable' : 'Add Variable');
  overlay.innerHTML = `
    <div class="env-manager-panel panel">
      <h2>${isEdit ? 'Edit Variable' : 'Add Variable'}</h2>
      <div class="stack">
        <label>
          Variable Name
          <input type="text" id="dashboard-variable-name" value="${escapeHtml(variable?.name || '')}" placeholder="e.g. TEMP, PRESS_KPA">
        </label>
        <label>
          Node ID
          <select id="dashboard-variable-node">
            ${nodes.map((node) => `<option value="${escapeHtml(node.nodeId)}" ${node.nodeId === (variable?.nodeId || nodes[0].nodeId) ? 'selected' : ''}>${escapeHtml(node.nodeId)}</option>`).join('')}
          </select>
        </label>
        <label>
          Reading Type
          <select id="dashboard-variable-reading" disabled>
            <option value="">Loading readings…</option>
          </select>
        </label>
      </div>
      <div style="margin-top:1rem;display:flex;gap:0.5rem">
        <button type="button" class="primary" id="dashboard-variable-save">Save</button>
        <button type="button" class="secondary" id="dashboard-variable-cancel">Cancel</button>
      </div>
      <div id="dashboard-variable-error" class="alert error" style="display:none;margin-top:0.75rem"></div>
    </div>
  `;
  document.body.appendChild(overlay);

  const nameInput = document.getElementById('dashboard-variable-name');
  const nodeSelect = document.getElementById('dashboard-variable-node');
  const readingSelect = document.getElementById('dashboard-variable-reading');
  const errorEl = document.getElementById('dashboard-variable-error');

  if (nameInput) nameInput.focus();

  function closeModal() {
    overlay.remove();
  }

  async function loadReadingsForSelectedNode(preferredReadingType) {
    if (!nodeSelect || !readingSelect) return;
    readingSelect.disabled = true;
    readingSelect.innerHTML = '<option value="">Loading readings…</option>';
    if (errorEl) {
      errorEl.textContent = '';
      errorEl.style.display = 'none';
    }
    try {
      const readingTypes = await fetchReadingTypesForNode(nodeSelect.value);
      if (readingTypes.length === 0) {
        readingSelect.innerHTML = '<option value="">No numeric readings discovered</option>';
        readingSelect.disabled = true;
        if (errorEl) {
          errorEl.textContent = `No numeric readings were discovered for node "${nodeSelect.value}" in recent sensor data.`;
          errorEl.style.display = '';
        }
        return;
      }
      const selectedReading = readingTypes.includes(preferredReadingType) ? preferredReadingType : readingTypes[0];
      readingSelect.innerHTML = readingTypes.map((readingType) => `
        <option value="${escapeHtml(readingType)}" ${readingType === selectedReading ? 'selected' : ''}>${escapeHtml(readingType)}</option>
      `).join('');
      readingSelect.disabled = false;
      if (preferredReadingType && !readingTypes.includes(preferredReadingType) && errorEl) {
        errorEl.textContent = `Reading type "${preferredReadingType}" is no longer available for node "${nodeSelect.value}". Choose a replacement before saving.`;
        errorEl.style.display = '';
      }
    } catch (error) {
      readingSelect.innerHTML = '<option value="">Failed to load readings</option>';
      readingSelect.disabled = true;
      if (errorEl) {
        errorEl.textContent = `Failed to load readings: ${error.message}`;
        errorEl.style.display = '';
      }
    }
  }

  document.getElementById('dashboard-variable-cancel')?.addEventListener('click', closeModal);
  overlay.addEventListener('click', (event) => {
    if (event.target === overlay) {
      closeModal();
    }
  });
  overlay.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      closeModal();
    }
  });
  nodeSelect?.addEventListener('change', () => {
    loadReadingsForSelectedNode(null);
  });
  document.getElementById('dashboard-variable-save')?.addEventListener('click', () => {
    const name = nameInput?.value.trim();
    const nodeId = nodeSelect?.value;
    const readingType = readingSelect?.value;
    if (!name || !nodeId || !readingType) {
      if (errorEl) {
        errorEl.textContent = 'Variable name, node ID, and reading type are required.';
        errorEl.style.display = '';
      }
      return;
    }
    const existingNames = dashboard.variables
      .map((entry, entryIndex) => (entryIndex === index ? null : entry.name))
      .filter(Boolean);
    const validation = validateVariableName(name, existingNames);
    if (!validation.valid) {
      if (errorEl) {
        errorEl.textContent = validation.error;
        errorEl.style.display = '';
      }
      return;
    }

    const normalizedVariable = { name, nodeId, readingType };
    if (isEdit) {
      dashboard.variables[index] = normalizedVariable;
    } else {
      dashboard.variables.push(normalizedVariable);
    }
<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> acd7144 (Fix dashboard review feedback)
    const environments = loadEnvironments();
    persistDashboardEnvironment(env, environments);
=======
    environments = loadEnvironments();
<<<<<<< HEAD
    const envIndex = environments.findIndex((entry) => entry.name === env.name);
    if (envIndex >= 0) {
      environments[envIndex] = env;
      saveEnvironments(environments);
    }
>>>>>>> de06846 (Fix code review round 4 findings F-001 through F-003)
=======
    persistDashboardEnvironment(env, environments);
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
    closeModal();
    renderActiveTab();
  });

  await loadReadingsForSelectedNode(variable?.readingType || null);
}

async function showAddVariableModal() {
  await showVariableModal(null);
}

async function showEditVariableModal(index) {
  await showVariableModal(index);
}

function deleteVariable(index) {
  const env = loadActiveEnvironment();
  const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
  const variable = dashboard.variables[index];
  
  const usedInMetrics = dashboard.metrics.filter(m =>
    isVariableUsedInExpression(variable.name, m.expression)
  );
  
  if (usedInMetrics.length > 0) {
    const metricNames = usedInMetrics.map(m => m.displayName).join(', ');
    if (!window.confirm(`Variable '${variable.name}' is used in metrics: ${metricNames}. These metrics will show errors. Continue?`)) {
      return;
    }
  }
  
  dashboard.variables.splice(index, 1);
<<<<<<< HEAD
<<<<<<< HEAD
  const environments = loadEnvironments();
=======
  environments = loadEnvironments();
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
=======
  const environments = loadEnvironments();
>>>>>>> acd7144 (Fix dashboard review feedback)
  persistDashboardEnvironment(env, environments);
  renderActiveTab();
}

function showAddMetricModal() {
  const env = loadActiveEnvironment();
  const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
  
  if (dashboard.variables.length === 0) {
    alert('Add variables first before creating metrics.');
    return;
  }
  
  const displayName = window.prompt('Enter metric display name:', 'New Metric');
  if (!displayName) return;
  
  const availableVars = dashboard.variables.map(v => v.name).join(', ');
  const helpText = `
Operators: + - * / ^ (power)
Precedence: () > ^ > * / > + - (left-to-right)
Functions: sqrt(x), log(x), log10(x), exp(x), abs(x), min(a,b), max(a,b)
Variables: ${availableVars}`;
  
  const expression = window.prompt(`Enter expression:\n${helpText}`, '');
  if (!expression) return;
  
  const validation = validateExpression(expression, dashboard.variables.map(v => v.name));
  if (validation.error) {
    alert(validation.error);
    return;
  }
  if (validation.warning) {
    alert(validation.warning);
  }
  
  const color = window.prompt('Enter color (hex code, optional):', '#007bff');
  
  const metric = {
    id: `m_${Date.now()}`,
    displayName,
    expression,
    color: color || '#007bff'
  };
  
  dashboard.metrics.push(metric);
<<<<<<< HEAD
<<<<<<< HEAD
  const environments = loadEnvironments();
=======
  environments = loadEnvironments();
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
=======
  const environments = loadEnvironments();
>>>>>>> acd7144 (Fix dashboard review feedback)
  persistDashboardEnvironment(env, environments);
  renderActiveTab();
}

function showEditMetricModal(index) {
  const env = loadActiveEnvironment();
  const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
  const metric = dashboard.metrics[index];
  
  const newDisplayName = window.prompt('Enter display name:', metric.displayName);
  if (newDisplayName) metric.displayName = newDisplayName;
  
  const availableVars = dashboard.variables.map(v => v.name).join(', ');
  const newExpression = window.prompt(`Enter expression (variables: ${availableVars}):`, metric.expression);
  if (newExpression) {
    const validation = validateExpression(newExpression, dashboard.variables.map(v => v.name));
    if (validation.error) {
      alert(validation.error);
      return;
    }
    if (validation.warning) {
      alert(validation.warning);
    }
    metric.expression = newExpression;
  }
  
  const newColor = window.prompt('Enter color (hex):', metric.color);
  if (newColor) metric.color = newColor;
  
<<<<<<< HEAD
<<<<<<< HEAD
  const environments = loadEnvironments();
=======
  environments = loadEnvironments();
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
=======
  const environments = loadEnvironments();
>>>>>>> acd7144 (Fix dashboard review feedback)
  persistDashboardEnvironment(env, environments);
  renderActiveTab();
}

function deleteMetric(index) {
  const env = loadActiveEnvironment();
  const dashboard = env.dashboards[APP_DASHBOARD_STATE.activeDashboardIndex];
  const metric = dashboard.metrics[index];
  
  if (!window.confirm(`Delete metric "${metric.displayName}"?`)) return;
  
  dashboard.metrics.splice(index, 1);
<<<<<<< HEAD
<<<<<<< HEAD
  const environments = loadEnvironments();
=======
  environments = loadEnvironments();
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
=======
  const environments = loadEnvironments();
>>>>>>> acd7144 (Fix dashboard review feedback)
  persistDashboardEnvironment(env, environments);
  renderActiveTab();
}

async function fetchActualStateNodes() {
  try {
    const rows = await queryTable(CONFIG.actualStateTable, '');
    const nodeRows = filterNodeRows(rows);
    const latest = latestByPartition(nodeRows);
    return latest.map(row => ({
      partitionKey: row.PartitionKey,  // Internal key for queries
      nodeId: row.node_id || row.PartitionKey  // User-facing ID
    }));
  } catch (error) {
    console.error('Failed to fetch nodes:', error);
    return [];
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
  destroyAllDashboardCharts();

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
    case 'dashboards':
      await renderDashboards();
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
  const envs = loadEnvironments();
  const env = envs.find((e) => e.name === name);
  activateEnvironmentState(name, env);
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
            <button type="button" class="secondary env-export-btn" data-env="${escapeHtml(env.name)}">Export</button>
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
        <button type="button" class="secondary" id="env-import-btn">Import</button>
        ${envs.length > 0 ? '<button type="button" class="secondary" id="env-close-btn">Close</button>' : ''}
      </div>
    </div>
  </div>`;

  let overlay = document.getElementById('env-manager-overlay');
  if (overlay) overlay.remove();
  document.body.insertAdjacentHTML('beforeend', overlayHtml);

  document.getElementById('env-add-btn')?.addEventListener('click', () => showEnvironmentForm(null));
  document.getElementById('env-import-btn')?.addEventListener('click', () => importEnvironmentFromFile());
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
  for (const btn of document.querySelectorAll('.env-export-btn')) {
    btn.addEventListener('click', () => {
      const env = loadEnvironments().find((e) => e.name === btn.dataset.env);
      if (env) exportEnvironment(env);
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
          applySensorDataPreferences(createDefaultSensorDataPreferences());
          resetTransientSensorDataState();
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

    const fieldError = validateEnvironmentFields({ clientId, tenantId, storageAccount, functionAppName });
    if (fieldError) {
      if (errorEl) { errorEl.textContent = fieldError; errorEl.style.display = ''; }
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

    const envData = {
      name,
      clientId,
      tenantId,
      storageAccount,
      functionAppName,
      sensorData: existingEnv?.sensorData || createDefaultSensorDataPreferences(),
      dashboards: existingEnv?.dashboards || createDefaultDashboardsArray(),
    };
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
    clearUnsavedDashboardEnvironment(name);

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

if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    APP,
    CONFIG,
    buildSensorExportCsv,
    buildSensorExportJsonl,
    buildDeviceExportCsv,
    buildDeviceExportJsonl,
    parseSensorReadingsForExport,
    actualStateFilter,
    queryActualStateRange,
    querySensorDataRange,
    buildEnvironmentExportData,
    activateEnvironmentState,
    createDefaultSensorDataPreferences,
    clearPersistedSelectedSeriesPreference,
    handleImportedJson,
    loadActiveEnvironment,
    loadEnvironments,
    normalizeEnvironmentRecord,
    persistActiveSensorDataPreferences,
    pruneUnavailableSelectedSeries,
    sanitizeSensorDataPreferences,
    sensorDataFilter,
    SENSOR_STATE,
    saveSeriesOverrides,
    validateImportedSensorDataPreferences,
    // Dashboard functions for testing
    fetchActualStateNodes,
    fetchReadingTypesForNode,
    fetchVariableData,
    evaluateMetricTimeSeries,
<<<<<<< HEAD
<<<<<<< HEAD
    renderMetricCharts,
    downsamplePoints,
    APP_DASHBOARD_STATE,
    persistDashboardEnvironment,
    destroyDashboardChart,
    destroyAllDashboardCharts,
<<<<<<< HEAD
=======
    renderMetricCharts,
    downsamplePoints,
>>>>>>> 751eefb (Fix code review round 5 findings F-001 and F-002)
=======
>>>>>>> 81336eb (Fix code review round 6 findings F-001 and F-002)
    getDashboardTimeRangeBounds,
    normalizeDashboardTimeRange,
    validateVariableName,
    validateExpression,
<<<<<<< HEAD
=======
    getDashboardTimeRangeBounds,
    normalizeDashboardTimeRange,
>>>>>>> de06846 (Fix code review round 4 findings F-001 through F-003)
=======
>>>>>>> acd7144 (Fix dashboard review feedback)
  };
}

document.addEventListener('DOMContentLoaded', () => {
  // MSAL loginPopup() opens a popup that loads this SPA.  The popup only needs
  // MSAL to process the auth response — skip full app init to avoid unnecessary
  // API calls and rendering.
  if ((window.opener && window.opener !== window) || window.__SONDE_TEST__ === true) {
    return;
  }
  init().catch((error) => renderError('Application failed to start', error));
});
