// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

(function (root, factory) {
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = factory();
    return;
  }
  root.SondeDashboardRuntime = factory();
}(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  const RESERVED_FUNCTION_NAMES = ['sqrt', 'log', 'log10', 'exp', 'abs', 'min', 'max'];
  const BLOCKED_OBJECT_KEYS = new Set(['__proto__', 'constructor', 'prototype']);
  const DASHBOARD_TIME_RANGE_MS = {
    '1h': 60 * 60 * 1000,
    '6h': 6 * 60 * 60 * 1000,
    '24h': 24 * 60 * 60 * 1000,
    '7d': 7 * 24 * 60 * 60 * 1000,
  };

  function createMetricId() {
    return `metric-${globalThis.crypto?.randomUUID?.() || Date.now()}`;
  }

  function createDefaultDashboardsArray() {
    return [];
  }

  function createDefaultDashboard(name) {
    return {
      name: name || 'Dashboard 1',
      variablesCollapsed: false,
      variables: [],
      charts: [],
      timeRange: {
        preset: '24h',
        start: null,
        end: null,
      },
    };
  }

  function createDefaultChart(name) {
    return {
      name: name || 'Chart 1',
      metricsCollapsed: false,
      metrics: [],
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
      id: typeof metric.id === 'string' ? metric.id : createMetricId(),
      displayName,
      expression,
      color: typeof metric.color === 'string' ? metric.color : '#007bff',
      ...(typeof metric._validationError === 'string' ? { _validationError: metric._validationError } : {}),
      ...(typeof metric._validationWarning === 'string' ? { _validationWarning: metric._validationWarning } : {}),
    };
  }

  function normalizeDashboardChart(chart, index) {
    if (typeof chart !== 'object' || chart === null) {
      return createDefaultChart(`Chart ${index + 1}`);
    }
    return {
      name: typeof chart.name === 'string' && chart.name.trim()
        ? chart.name.trim()
        : `Chart ${index + 1}`,
      metricsCollapsed: chart.metricsCollapsed === true,
      metrics: Array.isArray(chart.metrics)
        ? chart.metrics.map(normalizeDashboardMetric).filter(Boolean)
        : [],
    };
  }

  function normalizeDashboardCharts(dashboard) {
    if (Array.isArray(dashboard?.charts)) {
      return dashboard.charts.map((chart, index) => normalizeDashboardChart(chart, index));
    }
    if (Array.isArray(dashboard?.metrics) && dashboard.metrics.length > 0) {
      return [{
        name: 'Chart 1',
        metricsCollapsed: false,
        metrics: dashboard.metrics.map(normalizeDashboardMetric).filter(Boolean),
      }];
    }
    return [];
  }

  function normalizeDashboard(dashboard, index) {
    if (typeof dashboard !== 'object' || dashboard === null) {
      return createDefaultDashboard(`Dashboard ${index + 1}`);
    }
    return {
      name: typeof dashboard.name === 'string' && dashboard.name.trim()
        ? dashboard.name.trim()
        : `Dashboard ${index + 1}`,
      variablesCollapsed: dashboard.variablesCollapsed === true,
      variables: Array.isArray(dashboard.variables)
        ? dashboard.variables.map(normalizeDashboardVariable).filter(Boolean)
        : [],
      charts: normalizeDashboardCharts(dashboard),
      timeRange: normalizeDashboardTimeRange(dashboard.timeRange),
    };
  }

  function serializeDashboard(dashboard, index) {
    const normalized = normalizeDashboard(dashboard, index);
    return {
      name: normalized.name,
      variablesCollapsed: normalized.variablesCollapsed === true,
      variables: normalized.variables.map((variable) => ({ ...variable })),
      charts: normalized.charts.map((chart) => ({
        name: chart.name,
        metricsCollapsed: chart.metricsCollapsed === true,
        metrics: chart.metrics.map((metric) => ({
          id: metric.id,
          displayName: metric.displayName,
          expression: metric.expression,
          color: metric.color,
        })),
      })),
      timeRange: normalizeDashboardTimeRange(normalized.timeRange),
    };
  }

  function getDashboardMetricCount(dashboard) {
    return normalizeDashboardCharts(dashboard).reduce((count, chart) => count + chart.metrics.length, 0);
  }

  function getDashboardMetrics(dashboard) {
    return normalizeDashboardCharts(dashboard).flatMap((chart) => chart.metrics);
  }

  function createExpressionParser() {
    const Parser = globalThis.exprEval?.Parser;
    if (typeof Parser !== 'function') {
      throw new Error('Expression parser unavailable.');
    }
    return new Parser();
  }

  function validateVariableName(name, existingNames) {
    if (!name || !name.trim()) {
      return { valid: false, reason: 'missing', error: 'Variable name is required' };
    }
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
      return {
        valid: false,
        reason: 'invalid_identifier',
        error: 'Variable name must start with a letter or underscore and contain only letters, numbers, and underscores',
      };
    }
    if (BLOCKED_OBJECT_KEYS.has(name)) {
      return { valid: false, reason: 'reserved_key', error: `'${name}' is reserved and cannot be used as a variable name` };
    }
    if (existingNames.includes(name)) {
      return { valid: false, reason: 'duplicate', error: `Variable name '${name}' already exists` };
    }
    if (RESERVED_FUNCTION_NAMES.includes(name)) {
      return { valid: false, reason: 'reserved_function', error: `'${name}' is a reserved function name` };
    }
    return { valid: true };
  }

  function validateExpression(expression, availableVariables) {
    try {
      const expr = createExpressionParser().parse(expression);
      const usedVars = expr.variables();
      const undefinedVars = usedVars.filter((v) => !availableVariables.includes(v));
      if (undefinedVars.length > 0) {
        return {
          valid: true,
          warning: `Undefined variables: ${undefinedVars.join(', ')}`,
        };
      }
      return { valid: true };
    } catch (error) {
      return {
        valid: false,
        error: `Syntax error: ${error.message}`,
      };
    }
  }

  function isVariableUsedInExpression(variableName, expression) {
    try {
      return createExpressionParser().parse(expression).variables().includes(variableName);
    } catch {
      return expression.includes(variableName);
    }
  }

  function normalizeEnvironmentRecord(env, deps = {}) {
    const sanitizeSensorDataPreferencesFn = deps.sanitizeSensorDataPreferences;
    if (typeof sanitizeSensorDataPreferencesFn !== 'function') {
      throw new Error('sanitizeSensorDataPreferences dependency is required.');
    }
    const validateExpressionFn = deps.validateExpressionFn || validateExpression;
    const normalized = {
      name: typeof env?.name === 'string' ? env.name : '',
      clientId: typeof env?.clientId === 'string' ? env.clientId : '',
      tenantId: typeof env?.tenantId === 'string' ? env.tenantId : '',
      storageAccount: typeof env?.storageAccount === 'string' ? env.storageAccount : '',
      functionAppName: typeof env?.functionAppName === 'string' ? env.functionAppName : '',
      sensorData: sanitizeSensorDataPreferencesFn(env?.sensorData),
      dashboards: Array.isArray(env?.dashboards)
        ? env.dashboards.map((dashboard, index) => normalizeDashboard(dashboard, index))
        : createDefaultDashboardsArray(),
    };

    normalized.dashboards.forEach((dashboard) => {
      const variableNames = (dashboard.variables || []).map((v) => v.name);
      for (const chart of dashboard.charts || []) {
        for (const metric of chart.metrics || []) {
          delete metric._validationError;
          delete metric._validationWarning;
          const validation = validateExpressionFn(metric.expression, variableNames);
          if (validation.error) {
            metric._validationError = validation.error;
          } else if (validation.warning) {
            metric._validationWarning = validation.warning;
          }
        }
      }
    });

    return normalized;
  }

  function serializeEnvironmentRecord(env, deps = {}) {
    const normalized = normalizeEnvironmentRecord(env, deps);
    return {
      name: normalized.name,
      clientId: normalized.clientId,
      tenantId: normalized.tenantId,
      storageAccount: normalized.storageAccount,
      functionAppName: normalized.functionAppName,
      sensorData: normalized.sensorData,
      dashboards: normalized.dashboards.map((dashboard, index) => serializeDashboard(dashboard, index)),
    };
  }

  function buildEnvironmentExportData(env, deps = {}) {
    const normalizedEnv = serializeEnvironmentRecord(env, deps);
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

  function validateImportedDashboardVariable(variable, existingNames, dashboardName, deps = {}) {
    if (typeof variable !== 'object' || variable === null || Array.isArray(variable)) {
      throw new Error(`Dashboard "${dashboardName}" variables must be objects.`);
    }
    const name = typeof variable.name === 'string' ? variable.name.trim() : '';
    const nodeId = typeof variable.nodeId === 'string' ? variable.nodeId.trim() : '';
    const readingType = typeof variable.readingType === 'string' ? variable.readingType.trim() : '';
    if (!name || !nodeId || !readingType) {
      throw new Error(`Dashboard "${dashboardName}" variables require string name, nodeId, and readingType fields.`);
    }
    const validateVariableNameFn = deps.validateVariableNameFn || validateVariableName;
    const validation = validateVariableNameFn(name, existingNames);
    if (!validation.valid) {
      throw new Error(`Dashboard "${dashboardName}" variable "${name}": ${validation.error}.`);
    }
    return { name, nodeId, readingType };
  }

  function validateImportedDashboardMetric(metric, dashboardName) {
    if (typeof metric !== 'object' || metric === null || Array.isArray(metric)) {
      throw new Error(`Dashboard "${dashboardName}" metrics must be objects.`);
    }
    const expression = typeof metric.expression === 'string' ? metric.expression.trim() : '';
    if (!expression) {
      throw new Error(`Dashboard "${dashboardName}" metrics require a non-empty string expression.`);
    }
    return {
      id: typeof metric.id === 'string' ? metric.id : createMetricId(),
      displayName: typeof metric.displayName === 'string' ? metric.displayName : '',
      expression,
      color: typeof metric.color === 'string' ? metric.color : '#007bff',
    };
  }

  function validateImportedDashboardChart(chart, chartIndex, dashboardName) {
    if (typeof chart !== 'object' || chart === null || Array.isArray(chart)) {
      throw new Error(`Dashboard "${dashboardName}" charts must be objects.`);
    }
    const chartName = typeof chart.name === 'string' && chart.name.trim()
      ? chart.name.trim()
      : `Chart ${chartIndex + 1}`;
    const metrics = [];
    for (const metric of Array.isArray(chart.metrics) ? chart.metrics : []) {
      metrics.push(validateImportedDashboardMetric(metric, dashboardName));
    }
    return {
      name: chartName,
      metricsCollapsed: chart.metricsCollapsed === true,
      metrics,
    };
  }

  function validateImportedDashboard(dashboard, index, deps = {}) {
    if (typeof dashboard !== 'object' || dashboard === null || Array.isArray(dashboard)) {
      throw new Error(`Dashboard entry ${index + 1} must be an object.`);
    }
    const dashboardName = typeof dashboard.name === 'string' && dashboard.name.trim()
      ? dashboard.name.trim()
      : `Imported Dashboard ${index + 1}`;
    const variables = [];
    for (const variable of Array.isArray(dashboard.variables) ? dashboard.variables : []) {
      variables.push(validateImportedDashboardVariable(variable, variables.map((entry) => entry.name), dashboardName, deps));
    }
    let charts = [];
    if (Array.isArray(dashboard.charts)) {
      charts = dashboard.charts.map((chart, chartIndex) => validateImportedDashboardChart(chart, chartIndex, dashboardName));
    } else if (Array.isArray(dashboard.metrics) && dashboard.metrics.length > 0) {
      charts = [{
        name: 'Chart 1',
        metricsCollapsed: false,
        metrics: dashboard.metrics.map((metric) => validateImportedDashboardMetric(metric, dashboardName)),
      }];
    }
    return {
      name: dashboardName,
      variablesCollapsed: dashboard.variablesCollapsed === true,
      variables,
      charts,
      timeRange: (typeof dashboard.timeRange === 'object' && dashboard.timeRange !== null)
        ? normalizeDashboardTimeRange({
            preset: typeof dashboard.timeRange.preset === 'string' ? dashboard.timeRange.preset : '24h',
            start: dashboard.timeRange.start == null ? null : Number(dashboard.timeRange.start),
            end: dashboard.timeRange.end == null ? null : Number(dashboard.timeRange.end),
          })
        : { preset: '24h', start: null, end: null },
    };
  }

  function formatChartTooltipTimestamp(timestampMs) {
    const value = Number(timestampMs);
    if (!Number.isFinite(value)) {
      return '—';
    }
    return new Date(value).toLocaleString();
  }

  function formatTimeAxisTick(timestampMs, includeDate) {
    const value = Number(timestampMs);
    if (!Number.isFinite(value)) {
      return '';
    }
    const date = new Date(value);
    const hh = date.getHours().toString().padStart(2, '0');
    const mm = date.getMinutes().toString().padStart(2, '0');
    if (includeDate) {
      return `${date.getMonth() + 1}/${date.getDate()} ${hh}:${mm}`;
    }
    return `${hh}:${mm}`;
  }

  function escapeHtml(value) {
    return String(value ?? '')
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;')
      .replaceAll("'", '&#39;');
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

  function describeDashboardTimeRange(timeRange) {
    const normalized = normalizeDashboardTimeRange(timeRange);
    switch (normalized.preset) {
      case '1h':
        return 'Last Hour';
      case '6h':
        return 'Last 6 Hours';
      case '24h':
        return 'Last 24 Hours';
      case '7d':
        return 'Last 7 Days';
      case 'custom':
        if (Number.isFinite(normalized.start) && Number.isFinite(normalized.end)) {
          return `${new Date(normalized.start).toLocaleString()} - ${new Date(normalized.end).toLocaleString()}`;
        }
        return 'Custom Range';
      default:
        return 'Last 24 Hours';
    }
  }

  function renderDashboardTabs(dashboards, activeIndex) {
    const tabs = dashboards.map((dashboard, index) => `
    <div class="dashboard-tab-item">
      <button type="button" class="dashboard-tab ${index === activeIndex ? 'active' : ''}" data-dashboard-index="${index}">
        ${escapeHtml(dashboard.name)}
      </button>
      <button type="button" class="dashboard-tab-delete" data-delete-dashboard="${index}" title="Delete dashboard" aria-label="Delete dashboard ${escapeHtml(dashboard.name)}">&times;</button>
    </div>
  `).join('');

    return `
    <div class="dashboard-tabs-bar">
      ${tabs}
      <button type="button" class="dashboard-tab-add" id="add-dashboard-btn" title="Add dashboard" aria-label="Add dashboard">+</button>
    </div>
  `;
  }

  function renderVariablesList(variables, options = {}) {
    const readOnly = options.readOnly === true;
    if (variables.length === 0) {
      return '<p class="text-muted">No variables defined yet.</p>';
    }

    const rows = variables.map((variable, index) => `
    <tr>
      <td><code>${escapeHtml(variable.name)}</code></td>
      <td>${escapeHtml(variable.nodeId)} - ${escapeHtml(variable.readingType)}</td>
      ${readOnly ? '' : `
      <td>
        <button class="btn-sm" data-edit-variable="${index}">Edit</button>
        <button class="btn-sm btn-danger" data-delete-variable="${index}">Delete</button>
      </td>
      `}
    </tr>
  `).join('');

    return `
    <table class="variables-table">
      <thead>
        <tr>
          <th>Variable</th>
          <th>Data Source</th>
          ${readOnly ? '' : '<th>Actions</th>'}
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>
  `;
  }

  function renderMetricCard(metric, chartIndex, metricIndex, options = {}) {
    const readOnly = options.readOnly === true;
    const hasError = metric._validationError;
    const hasWarning = metric._validationWarning;
    return `
    <div class="metric-card ${hasError ? 'metric-error' : ''}" id="metric-${chartIndex}-${metricIndex}">
      <div class="metric-header">
        <h5>${escapeHtml(metric.displayName || `Metric ${metricIndex + 1}`)}</h5>
        ${readOnly ? '' : `
        <div class="metric-actions">
          <button class="btn-sm" data-edit-metric-chart="${chartIndex}" data-edit-metric-index="${metricIndex}">Edit</button>
          <button class="btn-sm btn-danger" data-delete-metric-chart="${chartIndex}" data-delete-metric-index="${metricIndex}">Delete</button>
        </div>
        `}
      </div>
      <div class="metric-expression">
        <code>${escapeHtml(metric.expression)}</code>
        ${hasError ? `<div class="error-text">${escapeHtml(metric._validationError)}</div>` : ''}
        ${!hasError && hasWarning ? `<div class="text-muted">${escapeHtml(metric._validationWarning)}</div>` : ''}
      </div>
    </div>
  `;
  }

  function renderChartCard(chart, chartIndex, variableCount, options = {}) {
    const readOnly = options.readOnly === true;
    const metricsOpenAttr = chart.metricsCollapsed === true ? '' : ' open';
    const metricCount = chart.metrics.length;
    return `
    <div class="chart-card" id="chart-${chartIndex}">
      <details class="chart-metrics-pane" data-chart-metrics-pane="${chartIndex}"${metricsOpenAttr}>
        <summary class="chart-header chart-pane-summary">
          <span class="chart-header-main">
            <span class="chart-pane-title">${escapeHtml(chart.name)}</span>
            <span class="chart-pane-meta">${metricCount} metric${metricCount === 1 ? '' : 's'}</span>
          </span>
          ${readOnly ? '' : `
          <span class="chart-actions">
            <button class="btn-sm" data-edit-chart="${chartIndex}">Rename</button>
            <button class="btn-sm btn-danger" data-delete-chart="${chartIndex}">Delete</button>
          </span>
          `}
        </summary>
        <div class="chart-metrics-pane-body">
          ${readOnly ? '' : `
          <div class="chart-metrics-actions">
            <button class="btn-sm btn-primary" data-add-metric="${chartIndex}">+ Add Metric</button>
          </div>
          `}
          <div class="chart-metrics">
            ${chart.metrics.length === 0
              ? (readOnly
                ? '<p class="text-muted">No metrics are defined for this chart.</p>'
                : `<p class="text-muted">No metrics yet. ${variableCount === 0 ? 'Add variables first, then ' : ''}click "+ Add Metric" for this chart.</p>`)
              : chart.metrics.map((metric, metricIndex) => renderMetricCard(metric, chartIndex, metricIndex, options)).join('')
            }
          </div>
        </div>
      </details>
      ${chart.metrics.length > 0 ? `
        <div class="metric-chart-container">
          <canvas id="metric-chart-${chartIndex}"></canvas>
        </div>
      ` : ''}
    </div>
  `;
  }

  function renderDashboardContent(dashboard, options = {}) {
    const readOnly = options.readOnly === true;
    const timeRange = normalizeDashboardTimeRange(dashboard.timeRange);
    const variablesOpenAttr = dashboard.variablesCollapsed === true ? '' : ' open';
    return `
    <div class="dashboard-header">
      <h2>${escapeHtml(dashboard.name)}</h2>
      <div class="dashboard-header-controls">
        ${readOnly ? `
        <span class="dashboard-time-range-label">${escapeHtml(describeDashboardTimeRange(timeRange))}</span>
        ` : `
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
        `}
      </div>
    </div>

    <details class="dashboard-pane dashboard-variables" id="dashboard-variables-pane"${variablesOpenAttr}>
      <summary class="dashboard-pane-summary">
        <span class="dashboard-pane-title">Variables</span>
        <span class="dashboard-pane-meta">${dashboard.variables.length} defined</span>
      </summary>
      <div class="dashboard-pane-body">
        ${readOnly ? '' : `
        <div class="dashboard-pane-actions">
          <button class="btn btn-sm btn-secondary" id="add-variable-btn">+ Add Variable</button>
        </div>
        `}
        ${renderVariablesList(dashboard.variables, options)}
      </div>
    </details>

    <div class="dashboard-charts">
      <h3>Charts${readOnly ? '' : ' <button class="btn btn-sm btn-primary" id="add-chart-btn">+ Add Chart</button>'}</h3>
      ${dashboard.charts.length === 0
        ? (readOnly
          ? '<p class="text-muted">No charts are defined for this dashboard.</p>'
          : '<p class="text-muted">No charts yet. Click "+ Add Chart" above to get started.</p>')
        : dashboard.charts.map((chart, chartIndex) => renderChartCard(chart, chartIndex, dashboard.variables.length, options)).join('')
      }
    </div>
  `;
  }

  function renderReadOnlyDashboardPage(dashboard) {
    return `
    <section class="dashboard-page dashboard-page--read-only">
      ${renderDashboardContent(dashboard, { readOnly: true })}
    </section>
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

  function dashboardRangeShowsDateLabels(timeRange, nowMs = Date.now()) {
    const { startMs, endMs } = getDashboardTimeRangeBounds(timeRange, nowMs);
    return Number.isFinite(startMs) && Number.isFinite(endMs) && (endMs - startMs) > DASHBOARD_TIME_RANGE_MS['24h'];
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

  async function renderMetricCharts(dashboard, deps = {}) {
    const normalizedDashboard = normalizeDashboard(dashboard, 0);
    const evaluateMetricTimeSeriesFn = deps.evaluateMetricTimeSeriesFn || evaluateMetricTimeSeries;
    const downsamplePointsFn = deps.downsamplePointsFn || downsamplePoints;
    const documentRef = deps.document || globalThis.document;
    const chartFactory = deps.chartFactory || ((canvas, config) => new globalThis.Chart(canvas, config));
    const includeDateInAxisLabels = dashboardRangeShowsDateLabels(normalizedDashboard.timeRange);
    for (let chartIndex = 0; chartIndex < normalizedDashboard.charts.length; chartIndex++) {
      const chart = normalizedDashboard.charts[chartIndex];
      const canvas = documentRef?.getElementById(`metric-chart-${chartIndex}`);
      if (!canvas) continue;

      if (typeof deps.destroyChartFn === 'function') {
        deps.destroyChartFn(chartIndex);
      }

      const datasets = [];
      let fallbackMessage = null;
      let fallbackClassName = 'text-muted';
      let fallbackSeverity = 0;

      function setFallback(message, className, severity) {
        if (severity >= fallbackSeverity) {
          fallbackMessage = message;
          fallbackClassName = className;
          fallbackSeverity = severity;
        }
      }

      for (const metric of chart.metrics) {
        if (metric._validationError) {
          setFallback(metric._validationError, 'error-text', 3);
          continue;
        }
        if (metric._validationWarning) {
          setFallback(metric._validationWarning, 'text-muted', 2);
          continue;
        }

        const result = await evaluateMetricTimeSeriesFn(metric, normalizedDashboard.variables, normalizedDashboard.timeRange, deps);
        if (result.error) {
          setFallback(result.error, 'error-text', 3);
          continue;
        }
        if (result.points.length === 0) {
          setFallback('No data in selected time range.', 'text-muted', 1);
          continue;
        }

        datasets.push({
          label: metric.displayName || metric.expression,
          data: downsamplePointsFn(result.points, 500).map((point) => ({ x: point.timestamp, y: point.value })),
          borderColor: metric.color || '#007bff',
          backgroundColor: metric.color || '#007bff',
          fill: false,
          tension: 0.1,
        });
      }

      if (datasets.length === 0) {
        canvas.parentElement.innerHTML = `<div class="${fallbackClassName}">${escapeHtml(fallbackMessage || 'No data in selected time range.')}</div>`;
        continue;
      }

      if (typeof deps.storeChartInstanceFn === 'function') {
        deps.storeChartInstanceFn(chartIndex, chartFactory(canvas, {
          type: 'line',
          data: { datasets },
          options: {
            responsive: true,
            maintainAspectRatio: false,
            scales: {
              x: {
                type: 'linear',
                ticks: {
                  callback(value) {
                    return formatTimeAxisTick(value, includeDateInAxisLabels);
                  },
                },
              },
              y: {
                beginAtZero: false,
              },
            },
            plugins: {
              tooltip: {
                callbacks: {
                  title(items) {
                    if (!items.length) return '';
                    return formatChartTooltipTimestamp(items[0].parsed.x);
                  },
                },
              },
            },
          },
        }));
        continue;
      }

      chartFactory(canvas, {
        type: 'line',
        data: { datasets },
        options: {
          responsive: true,
          maintainAspectRatio: false,
          scales: {
            x: {
              type: 'linear',
              ticks: {
                callback(value) {
                  return formatTimeAxisTick(value, includeDateInAxisLabels);
                },
              },
            },
            y: {
              beginAtZero: false,
            },
          },
          plugins: {
            tooltip: {
              callbacks: {
                title(items) {
                  if (!items.length) return '';
                  return formatChartTooltipTimestamp(items[0].parsed.x);
                },
              },
            },
          },
        },
      });
    }
  }

  async function evaluateMetricTimeSeries(metric, variables, timeRange, deps = {}) {
    const parserFactory = deps.parserFactory || createExpressionParser;
    const fetchVariableDataFn = deps.fetchVariableDataFn;
    if (typeof fetchVariableDataFn !== 'function') {
      return { points: [], error: 'Failed to fetch dashboard data: fetchVariableDataFn dependency is required.' };
    }
    let parser;
    let expr;
    try {
      parser = parserFactory();
      expr = parser.parse(metric.expression);
    } catch (error) {
      return {
        points: [],
        error: `Expression error: ${error.message}`,
      };
    }

    const usedVars = expr.variables();
    const undefinedVars = usedVars.filter((variableName) => !variables.some((variable) => variable.name === variableName));
    if (undefinedVars.length > 0) {
      return {
        points: [],
        error: `Undefined variables: ${undefinedVars.join(', ')}`,
      };
    }
    if (usedVars.length === 0) {
      return {
        points: [],
        error: 'Expression uses no variables',
      };
    }
    const usedVariables = variables.filter((variable) => usedVars.includes(variable.name));

    let variableData;
    try {
      variableData = await fetchVariableDataFn(usedVariables, timeRange, deps);
    } catch (error) {
      return {
        points: [],
        error: `Failed to fetch dashboard data: ${error.message}`,
      };
    }

    const timestamps = new Set();
    for (const points of Object.values(variableData)) {
      for (const point of points) {
        timestamps.add(point.timestamp);
      }
    }

    const sortedTimestamps = [...timestamps].sort((a, b) => a - b);
    const variableMaps = Object.create(null);
    for (const [name, points] of Object.entries(variableData)) {
      variableMaps[name] = new Map(points.map((point) => [point.timestamp, point.value]));
    }

    const result = [];
    for (const timestamp of sortedTimestamps) {
      const values = Object.create(null);
      let hasAllVariables = true;
      for (const variableName of usedVars) {
        const value = variableMaps[variableName]?.get(timestamp);
        if (typeof value !== 'number') {
          hasAllVariables = false;
          break;
        }
        values[variableName] = value;
      }
      if (!hasAllVariables) {
        continue;
      }

      try {
        const value = expr.evaluate(values);
        if (typeof value === 'number' && Number.isFinite(value)) {
          result.push({ timestamp, value });
        }
      } catch {
        // Skip timestamps that cannot be evaluated.
      }
    }

    return { points: result };
  }

  return {
    buildEnvironmentExportData,
    createDefaultDashboardsArray,
    downsamplePoints,
    evaluateMetricTimeSeries,
    getDashboardMetricCount,
    getDashboardMetrics,
    getDashboardTimeRangeBounds,
    isVariableUsedInExpression,
    normalizeDashboard,
    normalizeDashboardCharts,
    normalizeDashboardTimeRange,
    normalizeEnvironmentRecord,
    renderChartCard,
    renderDashboardContent,
    renderReadOnlyDashboardPage,
    renderDashboardTabs,
    renderMetricCharts,
    serializeDashboard,
    serializeEnvironmentRecord,
    validateExpression,
    validateImportedDashboard,
    validateVariableName,
  };
}));
