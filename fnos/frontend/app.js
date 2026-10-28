'use strict';
/*
 * FusionPlay fnOS management UI.
 *
 * Everything shown here comes from the Rust host's JSON API (see
 * fnos/ARCHITECTURE.md §3.1). There is no placeholder data, no fabricated
 * progress and no third-party dependency. The page works whether it is served at the site root
 * (development) or under the fnOS gateway prefix (`/app/fusionplay-fnos/`).
 */
(function () {
  // ---------------------------------------------------------------------------
  // Constants
  // ---------------------------------------------------------------------------
  const POLL_MS = 2000;
  const LOGS_REFRESH_MS = 4000;
  const DIAG_REFRESH_MS = 10000;
  const PAGE_IDS = ['overview', 'protocols', 'output', 'calibration', 'settings'];
  const PROTOCOLS = [
    { key: 'airplay', label: 'AirPlay 2' },
    { key: 'dlna', label: 'DLNA (DMR)' },
    { key: 'miplay', label: '小米妙播' },
  ];
  const SOURCE_LABELS = { airplay: 'AirPlay 2', dlna: 'DLNA', miplay: '小米妙播' };
  const CURVE_LABELS = { linear: '线性', soft: '柔和', balanced: '均衡', custom: '自定义' };
  const STATE_LABELS = {
    running: '运行中', listening: '监听中', active: '运行中',
    playing: '播放中', streaming: '串流中', paused: '已暂停',
    loading: '加载中', buffering: '缓冲中', idle: '空闲',
    starting: '启动中', stopping: '停止中', stopped: '已停止',
    connecting: '连接中', reconnecting: '重连中', connected: '已连接',
    disconnected: '已断开', ready: '就绪', waiting: '等待中',
    offline: '离线', disabled: '已禁用', error: '错误', failed: '错误',
  };
  const DIRTY_BADGES = {
    protocols: 'proto-dirty', output: 'out-dirty', calibration: 'cal-dirty',
  };

  // Gateway prefix: `/app/fusionplay-fnos` when behind the fnOS gateway,
  // empty string when served at the root for local debugging.
  const GATEWAY_BASE = (function () {
    let path = location.pathname || '/';
    path = path.replace(/\/index\.html$/i, '').replace(/\/+$/, '');
    return path === '/' || path === '' ? '' : path;
  })();

  // ---------------------------------------------------------------------------
  // Small helpers
  // ---------------------------------------------------------------------------
  const byId = (id) => document.getElementById(id);

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.textContent = text;
    return node;
  }
  function clear(node) {
    while (node && node.firstChild) node.removeChild(node.firstChild);
  }
  function setText(id, text) {
    const node = byId(id);
    if (node) node.textContent = text;
  }
  function show(node, visible) {
    if (node) node.classList.toggle('hidden', !visible);
  }
  function clamp(value, low, high) {
    return Math.min(high, Math.max(low, value));
  }
  function round(value, digits) {
    const factor = Math.pow(10, digits == null ? 1 : digits);
    return Math.round(Number(value) * factor) / factor;
  }
  function finite(value, fallback) {
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
  }
  function pad2(value) {
    return String(value).padStart(2, '0');
  }
  function fmtUptime(seconds) {
    const total = Math.max(0, Math.floor(finite(seconds, 0)));
    const days = Math.floor(total / 86400);
    const hours = Math.floor((total % 86400) / 3600);
    const minutes = Math.floor((total % 3600) / 60);
    const secs = total % 60;
    const parts = [];
    if (days) parts.push(days + ' 天');
    if (hours) parts.push(hours + ' 小时');
    if (minutes) parts.push(minutes + ' 分');
    if (!parts.length || secs) parts.push(secs + ' 秒');
    return parts.join(' ');
  }
  function fmtPlayTime(ms) {
    const total = Math.max(0, Math.round(finite(ms, 0) / 1000));
    const hours = Math.floor(total / 3600);
    const minutes = Math.floor((total % 3600) / 60);
    const secs = total % 60;
    return hours ? hours + ':' + pad2(minutes) + ':' + pad2(secs) : minutes + ':' + pad2(secs);
  }
  function fmtDb(value) {
    return value == null || !Number.isFinite(Number(value)) ? '—' : round(value, 1) + ' dB';
  }
  function fmtMs(value) {
    return value == null || !Number.isFinite(Number(value)) ? '—' : Math.round(value) + ' ms';
  }
  function fmtPpm(value) {
    return value == null || !Number.isFinite(Number(value)) ? '—' : round(value, 1) + ' ppm';
  }
  function fmtRate(rate) {
    const value = finite(rate, 0);
    if (!value) return '—';
    const khz = value / 1000;
    return (Number.isInteger(khz) ? khz : khz.toFixed(1)) + ' kHz';
  }
  function fmtCount(value) {
    return value == null || !Number.isFinite(Number(value)) ? '—' : Number(value).toLocaleString('zh-CN');
  }
  function fmtPercent(value) {
    return value == null || !Number.isFinite(Number(value)) ? '—' : Math.round(value) + '%';
  }
  function friendlyError(error) {
    const message = error && error.message ? error.message : String(error);
    if (/Failed to fetch|NetworkError|Load failed|network error/i.test(message)) {
      return '无法连接后端服务（网络错误）';
    }
    return message;
  }
  function clone(value) {
    return value == null ? value : JSON.parse(JSON.stringify(value));
  }
  function timestampLabel(date) {
    const d = date || new Date();
    return d.getFullYear() + pad2(d.getMonth() + 1) + pad2(d.getDate()) + '-' +
      pad2(d.getHours()) + pad2(d.getMinutes()) + pad2(d.getSeconds());
  }

  // ---------------------------------------------------------------------------
  // API layer (gateway-prefix aware)
  // ---------------------------------------------------------------------------
  function apiUrl(path) {
    const clean = String(path || '').replace(/^\/+/, '');
    if (!clean) return GATEWAY_BASE || '/';
    return GATEWAY_BASE + '/' + clean;
  }
  function jsonInit(method, body) {
    return {
      method: method,
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body == null ? {} : body),
    };
  }
  async function request(path, options, timeoutMs) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs || 12000);
    try {
      const response = await fetch(apiUrl(path), Object.assign({ cache: 'no-store', signal: controller.signal }, options || {}));
      const text = await response.text();
      let body = null;
      if (text) {
        try {
          body = JSON.parse(text);
        } catch (_error) {
          body = { raw: text };
        }
      }
      if (!response.ok) {
        const message = (body && (body.error || body.raw)) || ('HTTP ' + response.status);
        throw new Error(message);
      }
      return body;
    } catch (error) {
      if (error && error.name === 'AbortError') throw new Error('请求超时，请检查后端服务是否运行');
      throw error;
    } finally {
      clearTimeout(timer);
    }
  }

  // ---------------------------------------------------------------------------
  // Global state (server data + drafts so polling never clobbers user edits)
  // ---------------------------------------------------------------------------
  const state = {
    page: null,
    status: null,
    config: null,
    configSignature: '',
    devices: [],
    diagnostics: null,
    logs: [],
    logsFetchedAt: 0,
    diagFetchedAt: 0,
    devicesFetchedAt: 0,
    dirty: { protocols: false, output: false, calibration: false },
    drafts: { identity: null, output: null, calibration: null },
    removedOutput: new Set(),
    outputRows: new Map(),
    outputOrderKey: '',
    calDeviceKey: '',
    calDeviceRows: new Map(),
    preview: null,
    previewTimer: null,
    previewSeq: 0,
    selectedPoint: -1,
    draggingPoint: -1,
    coverUrl: '',
    failures: 0,
    lastError: '',
    tickBusy: false,
    logsPinned: true,
  };

  // ---------------------------------------------------------------------------
  // Header / connectivity / toast
  // ---------------------------------------------------------------------------
  let toastTimer = null;
  function toast(message, kind) {
    const node = byId('toast');
    if (!node) return;
    node.textContent = message;
    node.classList.toggle('error', kind === 'error');
    node.classList.add('show');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { node.classList.remove('show'); }, 3800);
  }

  function setConnectivity(ok, message) {
    state.failures = ok ? 0 : state.failures + 1;
    state.lastError = ok ? '' : (message || '请求失败');
    const dot = byId('health-dot');
    const text = byId('health-text');
    const banner = byId('conn-banner');
    const bannerText = byId('conn-banner-text');
    if (dot) dot.className = 'dot ' + (ok ? 'ok' : state.failures >= 3 ? 'bad' : 'warn');
    if (text) text.textContent = ok ? '运行中' : state.failures >= 3 ? '离线' : '重试中…';
    if (banner) banner.classList.toggle('hidden', ok);
    if (bannerText && !ok) {
      bannerText.textContent = '无法连接后端服务：' + state.lastError + '（每 2 秒自动重试）';
    }
  }

  function renderHeader() {
    const status = state.status;
    const pill = byId('version-pill');
    if (pill && status && status.version) pill.textContent = 'v' + status.version;
  }

  // ---------------------------------------------------------------------------
  // Config / draft synchronisation
  // ---------------------------------------------------------------------------
  function buildIdentityDraft() {
    const config = state.config;
    const draft = {
      name: 'FusionPlay',
      interface: '',
      protocols: { airplay: true, dlna: true, miplay: true },
    };
    if (config && config.receiver) {
      draft.name = config.receiver.name || '';
      draft.interface = config.receiver.interface || '';
    }
    if (config && config.protocols) {
      for (const protocol of PROTOCOLS) {
        draft.protocols[protocol.key] = !!(config.protocols[protocol.key] && config.protocols[protocol.key].enabled);
      }
    }
    return draft;
  }

  function normalizeCurvePoints(rawPoints) {
    const points = [];
    for (const pair of rawPoints || []) {
      if (!Array.isArray(pair) || pair.length < 2) continue;
      const x = Number(pair[0]);
      const y = Number(pair[1]);
      if (!Number.isFinite(x) || !Number.isFinite(y)) continue;
      if (x <= 0 || x >= 1) continue; // endpoints are implicit and fixed
      points.push([round(clamp(x, 0, 1), 4), round(clamp(y, 0, 1), 4)]);
    }
    points.sort(function (a, b) { return a[0] - b[0]; });
    let lastY = 0;
    for (const point of points) {
      if (point[1] < lastY) point[1] = lastY;
      lastY = point[1];
    }
    return points;
  }

  function buildCalibrationDraft() {
    const config = state.config;
    const audio = (config && config.audio) || {};
    const curve = audio.curve || { kind: 'balanced', points: [] };
    const draft = {
      global: finite(audio.global_attenuation_percent, 100),
      muted: !!audio.muted,
      kind: Object.prototype.hasOwnProperty.call(CURVE_LABELS, curve.kind) ? curve.kind : 'balanced',
      points: normalizeCurvePoints(curve.points),
      devices: [],
    };
    for (const device of audio.devices || []) {
      draft.devices.push({
        id: device.id,
        name: device.name || device.id,
        enabled: !!device.enabled,
        gain_db: finite(device.gain_db, 0),
        delay_ms: finite(device.delay_ms, 0),
      });
    }
    return draft;
  }

  function buildOutputDraft() {
    const map = new Map();
    const config = state.config;
    for (const device of (config && config.audio && config.audio.devices) || []) {
      map.set(device.id, {
        id: device.id,
        name: device.name || device.id,
        enabled: !!device.enabled,
        gain_db: finite(device.gain_db, 0),
        delay_ms: finite(device.delay_ms, 0),
        sample_rate: finite(device.sample_rate, 0),
        wasConfigured: true,
        touched: false,
      });
    }
    for (const info of state.devices) {
      const existing = map.get(info.id);
      if (existing) {
        if (info.name && !existing.touched) existing.name = info.name;
      } else {
        map.set(info.id, {
          id: info.id,
          name: info.name || info.id,
          enabled: false,
          gain_db: 0,
          delay_ms: 0,
          sample_rate: 0,
          wasConfigured: false,
          touched: false,
        });
      }
    }
    return map;
  }

  function applyConfig(config) {
    if (!config) return;
    const signature = JSON.stringify(config);
    const changed = signature !== state.configSignature;
    state.config = config;
    state.configSignature = signature;
    if (changed) {
      if (!state.dirty.protocols) state.drafts.identity = buildIdentityDraft();
      if (!state.dirty.output) state.drafts.output = buildOutputDraft();
      if (!state.dirty.calibration) {
        state.drafts.calibration = buildCalibrationDraft();
        if (state.page === 'calibration') schedulePreview(0);
      }
    }
    if (!state.drafts.identity) state.drafts.identity = buildIdentityDraft();
    if (!state.drafts.output) state.drafts.output = buildOutputDraft();
    if (!state.drafts.calibration) state.drafts.calibration = buildCalibrationDraft();
  }

  function rebuildAllDrafts() {
    state.drafts.identity = buildIdentityDraft();
    state.drafts.output = buildOutputDraft();
    state.drafts.calibration = buildCalibrationDraft();
    state.removedOutput.clear();
    state.outputOrderKey = '';
    state.calDeviceKey = '';
    state.selectedPoint = -1;
    state.preview = null;
  }

  function markDirty(page) {
    state.dirty[page] = true;
    const badgeId = DIRTY_BADGES[page];
    if (badgeId) show(byId(badgeId), true);
  }
  function clearDirty(page) {
    state.dirty[page] = false;
    const badgeId = DIRTY_BADGES[page];
    if (badgeId) show(byId(badgeId), false);
  }
  function clearAllDirty() {
    for (const page of Object.keys(state.dirty)) clearDirty(page);
  }

  // ---------------------------------------------------------------------------
  // Overview page
  // ---------------------------------------------------------------------------
  function protocolStateLabel(rawState, enabled) {
    if (!rawState) return enabled ? '未上报' : '未启用';
    return STATE_LABELS[String(rawState).toLowerCase()] || String(rawState);
  }
  function protocolBadgeClass(rawState, enabled) {
    const value = String(rawState || '').toLowerCase();
    if (['running', 'listening', 'active', 'playing', 'streaming', 'ready', 'connected'].indexOf(value) >= 0) return 'ok';
    if (['error', 'failed'].indexOf(value) >= 0) return 'bad';
    if (['starting', 'stopping', 'loading', 'buffering', 'paused', 'connecting', 'reconnecting', 'disconnected', 'waiting'].indexOf(value) >= 0) return 'warn';
    if (['idle', 'disabled', 'stopped', 'offline'].indexOf(value) >= 0) return 'muted';
    return enabled ? 'warn' : 'muted';
  }
  function protocolMetaItem(label, value, valueClass) {
    const item = el('span');
    item.appendChild(document.createTextNode(label + ' '));
    item.appendChild(el('b', valueClass || null, value));
    return item;
  }

  function renderOverviewProtocols() {
    const container = byId('ov-protocols');
    if (!container) return;
    clear(container);
    const runtime = (state.status && state.status.protocols) || {};
    const configured = (state.config && state.config.protocols) || null;
    for (const protocol of PROTOCOLS) {
      const info = runtime[protocol.key] || {};
      const enabled = configured ? !!(configured[protocol.key] && configured[protocol.key].enabled) : !!info.enabled;
      const tile = el('article', 'proto-tile');
      const head = el('div', 'proto-tile-head');
      head.appendChild(el('span', 'proto-name', protocol.label));
      head.appendChild(el('span', 'badge ' + protocolBadgeClass(info.state, enabled), protocolStateLabel(info.state, enabled)));
      tile.appendChild(head);
      const meta = el('div', 'proto-meta');
      meta.appendChild(protocolMetaItem('配置', enabled ? '开启' : '关闭', enabled ? 'ok-text' : 'muted'));
      meta.appendChild(protocolMetaItem('客户端', String(info.clients == null ? 0 : info.clients)));
      meta.appendChild(protocolMetaItem('端口', info.port ? String(info.port) : '—'));
      tile.appendChild(meta);
      const detail = info.detail || (enabled ? '运行状态由协议宿主上报' : '已在配置中关闭');
      tile.appendChild(el('p', 'proto-detail', detail));
      container.appendChild(tile);
    }
  }

  function renderOverviewMedia() {
    const status = state.status;
    const media = (status && status.media) || {};
    const audio = (status && status.audio) || {};
    const configAudio = (state.config && state.config.audio) || {};
    const source = status && status.current_source;
    setText('ov-source', source ? (SOURCE_LABELS[source] || source) : '空闲（等待投放）');
    const badge = byId('ov-source-badge');
    if (badge) {
      badge.textContent = source ? (SOURCE_LABELS[source] || source) : '空闲';
      badge.className = 'badge ' + (source ? 'ok' : 'muted');
    }
    setText('ov-title', media.title || '—');
    setText('ov-artist', media.artist || '—');
    setText('ov-album', media.album || '—');
    setText('ov-position', media.position_ms == null ? '—' : fmtPlayTime(media.position_ms) + (media.duration_ms ? ' / ' + fmtPlayTime(media.duration_ms) : ''));
    setText('ov-format', media.detail_codec || 'PCM f32 · 立体声（引擎主混音）');
    setText('ov-engine-rate', configAudio.sample_rate ? fmtRate(configAudio.sample_rate) : '—');

    const liveDevices = (status && status.devices) || [];
    const deviceRates = [];
    for (const device of liveDevices) {
      if (!device.enabled) continue;
      const label = fmtRate(device.sample_rate) + (device.channels ? ' / ' + device.channels + ' ch' : '');
      if (deviceRates.indexOf(label) === -1) deviceRates.push(label);
    }
    setText('ov-device-rate', deviceRates.length ? deviceRates.join('；') : '—（后端未上报）');

    const mode = audio.mode ? String(audio.mode) : '';
    const hint = byId('ov-media-hint');
    if (hint) {
      hint.textContent = '音频后端：' + (mode || '未知') +
        (mode === 'null' ? '（null 后端无硬件输出，设备状态不上报）' : '') +
        (media.title ? '' : ' · 当前没有活动媒体。');
    }

    const cover = byId('ov-cover');
    if (cover) {
      const url = media.cover_url;
      if (url && /^https?:\/\//i.test(url)) {
        if (state.coverUrl !== url) {
          state.coverUrl = url;
          cover.src = url;
        }
        show(cover, true);
      } else {
        state.coverUrl = '';
        cover.removeAttribute('src');
        show(cover, false);
      }
    }
  }

  function renderOverviewVolume() {
    const senderVolume = (state.status && state.status.sender_volume) || {};
    const airplay = senderVolume.airplay_db != null
      ? fmtDb(senderVolume.airplay_db) + (senderVolume.airplay_percent != null ? '（' + senderVolume.airplay_percent + '%）' : '')
      : senderVolume.airplay_percent != null ? senderVolume.airplay_percent + '%' : '—';
    setText('ov-vol-airplay', airplay);
    setText('ov-vol-dlna', senderVolume.dlna_percent != null ? senderVolume.dlna_percent + '%' : '—');
    setText('ov-vol-miplay', senderVolume.miplay_percent != null ? senderVolume.miplay_percent + '%' : '—');
  }

  function renderOverviewDevices() {
    const tbody = byId('ov-devices');
    if (!tbody) return;
    const status = state.status || {};
    const live = status.devices || [];
    const configured = (state.config && state.config.audio && state.config.audio.devices) || [];
    const seen = new Set();
    const rows = [];
    for (const device of live) {
      seen.add(device.id);
      rows.push({
        id: device.id,
        name: device.name || device.id,
        online: !!device.online,
        state: device.state || (device.online ? 'running' : 'offline'),
        buffer: device.buffer_ms,
        drift: device.drift_ppm,
        underruns: device.underruns,
        pending: false,
      });
    }
    for (const device of configured) {
      if (!device.enabled || seen.has(device.id)) continue;
      rows.push({
        id: device.id,
        name: device.name || device.id,
        online: false,
        state: 'pending',
        buffer: null,
        drift: null,
        underruns: null,
        pending: true,
      });
    }
    const badge = byId('ov-device-badge');
    const onlineCount = rows.filter(function (row) { return row.online; }).length;
    if (badge) {
      if (!rows.length) {
        badge.textContent = '未启用';
        badge.className = 'badge muted';
      } else {
        badge.textContent = onlineCount + ' / ' + rows.length + ' 在线';
        badge.className = 'badge ' + (onlineCount === rows.length ? 'ok' : onlineCount ? 'warn' : 'bad');
      }
    }
    clear(tbody);
    if (!rows.length) {
      const tr = el('tr');
      const td = el('td', 'muted', '未启用输出设备（可在「音频输出」页选择）');
      td.colSpan = 6;
      tr.appendChild(td);
      tbody.appendChild(tr);
    } else {
      for (const row of rows) {
        const tr = el('tr');
        tr.appendChild(el('td', 'name-cell', row.name));
        tr.appendChild(el('td', row.online ? 'ok-text' : 'muted', row.online ? '在线' : '离线'));
        tr.appendChild(el('td', 'muted', row.pending ? '未上报' : protocolStateLabel(row.state, true)));
        tr.appendChild(el('td', 'muted', row.pending ? '—' : fmtMs(row.buffer)));
        tr.appendChild(el('td', 'muted', row.pending ? '—' : fmtPpm(row.drift)));
        tr.appendChild(el('td', 'muted', row.pending ? '—' : fmtCount(row.underruns)));
        tbody.appendChild(tr);
      }
    }
    const mode = (status.audio && status.audio.mode) || '';
    const hint = byId('ov-device-hint');
    if (hint) {
      if (mode === 'null') {
        hint.textContent = '当前音频后端为 null（无硬件输出），引擎不上报缓冲 / 漂移 / underrun；已配置设备显示为「未上报」。';
      } else if (!rows.length) {
        hint.textContent = '未启用输出设备；配置后的设备会出现在这里。';
      } else {
        hint.textContent = '状态由后端音频引擎实时上报（缓冲、时钟漂移、underrun 计数）。';
      }
    }
  }

  function mergedErrors() {
    const status = state.status;
    if (!status) return [];
    const errors = (status.errors || []).slice(-5);
    if (status.last_error && errors.indexOf(status.last_error) === -1) errors.push(status.last_error);
    return errors;
  }

  function renderOverviewErrors() {
    const node = byId('ov-errors');
    if (!node) return;
    const errors = mergedErrors();
    node.textContent = errors.length ? errors.join('\n') : '暂无错误';
    node.classList.toggle('ok', errors.length === 0);
  }

  function renderOverviewEvents() {
    const pre = byId('ov-events');
    if (!pre) return;
    const lines = (state.logs || []).slice(-8);
    pre.textContent = lines.length ? lines.join('\n') : '暂无事件（后端日志为空）';
  }

  function renderOverview() {
    renderOverviewProtocols();
    renderOverviewMedia();
    renderOverviewVolume();
    renderOverviewDevices();
    renderOverviewErrors();
    renderOverviewEvents();
  }

  // ---------------------------------------------------------------------------
  // 接收协议 page
  // ---------------------------------------------------------------------------
  function buildProtocolCards() {
    const container = byId('proto-cards');
    if (!container) return;
    clear(container);
    for (const protocol of PROTOCOLS) {
      const card = el('article', 'card proto-card');
      card.dataset.proto = protocol.key;

      const head = el('div', 'proto-head');
      head.appendChild(el('h3', null, protocol.label));
      const badge = el('span', 'badge', '未上报');
      badge.dataset.role = 'badge';
      head.appendChild(badge);
      card.appendChild(head);

      const toggle = el('label', 'check-row');
      const checkbox = document.createElement('input');
      checkbox.type = 'checkbox';
      checkbox.className = 'proto-enabled';
      checkbox.dataset.proto = protocol.key;
      toggle.appendChild(checkbox);
      toggle.appendChild(el('span', null, '开启接收'));
      card.appendChild(toggle);

      const nameField = el('label', 'field');
      nameField.appendChild(el('span', null, '广播名称（写入 config.receiver.name）'));
      const nameInput = document.createElement('input');
      nameInput.type = 'text';
      nameInput.maxLength = 64;
      nameInput.autocomplete = 'off';
      nameInput.className = 'js-receiver-name';
      nameInput.dataset.proto = protocol.key;
      nameField.appendChild(nameInput);
      card.appendChild(nameField);

      const interfaceField = el('label', 'field');
      interfaceField.appendChild(el('span', null, '网络接口（留空 = 自动）'));
      const interfaceSelect = document.createElement('select');
      interfaceSelect.className = 'js-receiver-interface';
      interfaceSelect.dataset.proto = protocol.key;
      interfaceField.appendChild(interfaceSelect);
      card.appendChild(interfaceField);

      const runtime = el('div', 'runtime');
      const kv = el('div', 'kv small');
      const runtimeRows = [
        ['port', '端口'],
        ['state', '发现状态'],
        ['clients', '客户端'],
        ['detail', '详情'],
      ];
      for (const entry of runtimeRows) {
        const item = el('div');
        item.appendChild(el('span', null, entry[1]));
        const value = el('b', null, '—');
        value.dataset.role = entry[0];
        item.appendChild(value);
        kv.appendChild(item);
      }
      runtime.appendChild(kv);
      card.appendChild(runtime);
      container.appendChild(card);

      checkbox.addEventListener('change', function () {
        const draft = state.drafts.identity;
        if (!draft) return;
        draft.protocols[protocol.key] = checkbox.checked;
        markDirty('protocols');
        renderProtocols();
      });
      nameInput.addEventListener('input', function () {
        const draft = state.drafts.identity;
        if (!draft) return;
        draft.name = nameInput.value;
        syncIdentityInputs(nameInput);
        markDirty('protocols');
      });
      interfaceSelect.addEventListener('change', function () {
        const draft = state.drafts.identity;
        if (!draft) return;
        draft.interface = interfaceSelect.value;
        syncIdentityInputs(interfaceSelect);
        markDirty('protocols');
      });
    }
  }

  function syncIdentityInputs(source) {
    const draft = state.drafts.identity;
    if (!draft) return;
    for (const input of document.querySelectorAll('.js-receiver-name')) {
      if (input !== source && document.activeElement !== input) input.value = draft.name;
    }
    for (const select of document.querySelectorAll('.js-receiver-interface')) {
      if (select !== source) select.value = draft.interface;
    }
  }

  function fillInterfaceOptions(select, interfaces, current) {
    if (!select) return;
    const signature = (interfaces || []).join('|') + '#' + (current || '');
    if (select.dataset.signature !== signature) {
      select.dataset.signature = signature;
      clear(select);
      const auto = document.createElement('option');
      auto.value = '';
      auto.textContent = '自动（默认网卡）';
      select.appendChild(auto);
      for (const name of interfaces || []) {
        const option = document.createElement('option');
        option.value = name;
        option.textContent = name;
        select.appendChild(option);
      }
      if (current && (interfaces || []).indexOf(current) === -1) {
        const option = document.createElement('option');
        option.value = current;
        option.textContent = current + '（当前配置）';
        select.appendChild(option);
      }
    }
    select.value = current || '';
  }

  function renderProtocols() {
    const draft = state.drafts.identity;
    const container = byId('proto-cards');
    if (!draft || !container) return;
    const runtime = (state.status && state.status.protocols) || {};
    const interfaces = (state.diagnostics && state.diagnostics.interfaces) || [];
    for (const card of container.children) {
      const key = card.dataset.proto;
      if (!key) continue;
      const info = runtime[key] || {};
      const enabled = !!draft.protocols[key];
      const checkbox = card.querySelector('.proto-enabled');
      if (checkbox) checkbox.checked = enabled;
      const nameInput = card.querySelector('.js-receiver-name');
      if (nameInput && document.activeElement !== nameInput) nameInput.value = draft.name;
      const interfaceSelect = card.querySelector('.js-receiver-interface');
      fillInterfaceOptions(interfaceSelect, interfaces, draft.interface);
      const badge = card.querySelector('[data-role="badge"]');
      if (badge) {
        badge.textContent = protocolStateLabel(info.state, enabled);
        badge.className = 'badge ' + protocolBadgeClass(info.state, enabled);
      }
      const port = card.querySelector('[data-role="port"]');
      if (port) port.textContent = info.port ? String(info.port) : '—';
      const stateCell = card.querySelector('[data-role="state"]');
      if (stateCell) {
        stateCell.textContent = protocolStateLabel(info.state, enabled);
        stateCell.className = info.state ? '' : 'muted';
      }
      const clients = card.querySelector('[data-role="clients"]');
      if (clients) clients.textContent = String(info.clients == null ? 0 : info.clients);
      const detail = card.querySelector('[data-role="detail"]');
      if (detail) {
        detail.textContent = info.detail || (enabled ? '协议宿主未上报详情' : '配置中已关闭');
        detail.className = 'muted';
        detail.title = info.detail || '';
      }
    }
  }

  async function saveProtocols() {
    const draft = state.drafts.identity;
    const config = state.config;
    if (!draft || !config) return;
    const name = String(draft.name || '').trim();
    if (!name) {
      setErrors('proto-error', '广播名称不能为空（后端会拒绝保存）。');
      toast('保存失败：广播名称不能为空', 'error');
      return;
    }
    setErrors('proto-error', '');
    const payload = clone(config);
    payload.receiver.name = name;
    payload.receiver.interface = String(draft.interface || '');
    for (const protocol of PROTOCOLS) {
      if (!payload.protocols[protocol.key]) payload.protocols[protocol.key] = {};
      payload.protocols[protocol.key].enabled = !!draft.protocols[protocol.key];
    }
    try {
      const result = await request('api/config', jsonInit('PUT', payload));
      const saved = (result && result.config) || payload;
      clearDirty('protocols');
      applyConfig(saved);
      renderProtocols();
      toast('协议设置已保存，后端已按新配置重启协议宿主');
      refreshCore();
    } catch (error) {
      const message = friendlyError(error);
      // Current backend saves the config first and then restarts the protocol
      // hosts; a restart failure therefore may still have persisted the config.
      let persisted = false;
      try {
        const latest = await request('api/config');
        persisted = configLooksApplied(payload, latest);
        if (persisted) {
          state.config = latest;
          state.configSignature = JSON.stringify(latest);
        }
      } catch (_error) { /* keep original error */ }
      if (persisted) {
        const detail = '配置已写入 config.json，但协议宿主重启失败：' + message + '。请查看「设置 / 诊断」页日志，必要时重启应用。';
        setErrors('proto-error', detail);
        toast('配置已保存，协议宿主重启失败', 'error');
        refreshCore();
      } else {
        setErrors('proto-error', '保存失败：' + message);
        toast('保存失败：' + message, 'error');
      }
    }
  }

  // The backend persists config *before* restarting protocol hosts; a restart
  // failure therefore still leaves the new settings on disk. Compare the
  // relevant fields to tell "not saved" apart from "saved but restart failed".
  function normalizeConfigForCompare(config) {
    if (!config) return null;
    const receiver = config.receiver || {};
    const protocols = config.protocols || {};
    const audio = config.audio || {};
    const curve = audio.curve || {};
    const logging = config.logging || {};
    return {
      name: String(receiver.name || ''),
      interface: String(receiver.interface || ''),
      flags: PROTOCOLS.map(function (protocol) {
        return !!(protocols[protocol.key] && protocols[protocol.key].enabled);
      }),
      attenuation: round(finite(audio.global_attenuation_percent, 100), 3),
      muted: !!audio.muted,
      curveKind: String(curve.kind || ''),
      curvePoints: (curve.points || []).map(function (point) {
        return [round(finite(point[0], 0), 3), round(finite(point[1], 0), 3)];
      }),
      devices: (audio.devices || []).map(function (device) {
        return {
          id: String(device.id || ''),
          name: String(device.name || ''),
          enabled: !!device.enabled,
          gain_db: round(finite(device.gain_db, 0), 3),
          delay_ms: round(finite(device.delay_ms, 0), 3),
          sample_rate: Math.round(finite(device.sample_rate, 0)),
        };
      }),
      level: String(logging.level || ''),
    };
  }
  function configLooksApplied(expected, latest) {
    try {
      return JSON.stringify(normalizeConfigForCompare(expected)) === JSON.stringify(normalizeConfigForCompare(latest));
    } catch (_error) {
      return false;
    }
  }

  function resetProtocols() {
    if (!state.config) return;
    state.drafts.identity = buildIdentityDraft();
    clearDirty('protocols');
    setErrors('proto-error', '');
    renderProtocols();
    toast('已撤销未保存的协议修改');
  }

  // ---------------------------------------------------------------------------
  // 音频输出 page
  // ---------------------------------------------------------------------------
  function ensureOutputDraft() {
    if (!state.config) return null;
    if (!state.drafts.output) {
      state.drafts.output = buildOutputDraft();
      return state.drafts.output;
    }
    const draft = state.drafts.output;
    for (const info of state.devices) {
      let entry = draft.get(info.id);
      if (!entry) {
        entry = {
          id: info.id,
          name: info.name || info.id,
          enabled: false,
          gain_db: 0,
          delay_ms: 0,
          sample_rate: 0,
          wasConfigured: false,
          touched: false,
        };
        draft.set(info.id, entry);
      } else if (info.name && !entry.touched) {
        entry.name = info.name;
      }
    }
    return draft;
  }

  function outputDraftEntry(id) {
    return state.drafts.output ? state.drafts.output.get(id) : null;
  }

  function fillRateSelect(select, info, value) {
    const rates = (info && info.sample_rates) || [];
    const signature = rates.join(',') + '#' + value;
    if (select.dataset.signature !== signature) {
      select.dataset.signature = signature;
      clear(select);
      const auto = document.createElement('option');
      auto.value = '0';
      auto.textContent = '设备默认';
      select.appendChild(auto);
      for (const rate of rates) {
        const option = document.createElement('option');
        option.value = String(rate);
        option.textContent = rate + ' Hz';
        select.appendChild(option);
      }
      if (value && rates.indexOf(value) === -1) {
        const option = document.createElement('option');
        option.value = String(value);
        option.textContent = value + ' Hz（配置）';
        select.appendChild(option);
      }
    }
    select.value = String(value || 0);
    select.disabled = false;
    select.title = rates.length ? '设备支持的采样率' : '后端未上报可用采样率';
  }

  function createOutputRow(id) {
    const tr = el('tr');
    const els = {};

    const tdEnable = el('td');
    const enable = document.createElement('input');
    enable.type = 'checkbox';
    enable.setAttribute('aria-label', '启用该输出设备');
    enable.addEventListener('change', function () {
      const entry = outputDraftEntry(id);
      if (!entry) return;
      entry.enabled = enable.checked;
      entry.touched = true;
      markDirty('output');
    });
    tdEnable.appendChild(enable);
    els.enable = enable;
    tr.appendChild(tdEnable);

    const tdName = el('td', 'name-cell');
    els.name = el('span');
    els.defaultTag = el('span', 'tag hidden', '默认');
    tdName.appendChild(els.name);
    tdName.appendChild(document.createTextNode(' '));
    tdName.appendChild(els.defaultTag);
    tr.appendChild(tdName);

    els.id = el('td', 'mono muted');
    tr.appendChild(els.id);

    els.online = el('td');
    tr.appendChild(els.online);

    els.rates = el('td', 'muted');
    tr.appendChild(els.rates);

    els.channels = el('td', 'muted');
    tr.appendChild(els.channels);

    const tdFormat = el('td');
    els.rateSelect = document.createElement('select');
    els.rateSelect.setAttribute('aria-label', '设备首选采样率');
    els.rateSelect.addEventListener('change', function () {
      const entry = outputDraftEntry(id);
      if (!entry) return;
      entry.sample_rate = parseInt(els.rateSelect.value, 10) || 0;
      entry.touched = true;
      markDirty('output');
    });
    tdFormat.appendChild(els.rateSelect);
    els.formatNote = el('span', 'format-note');
    tdFormat.appendChild(els.formatNote);
    tr.appendChild(tdFormat);

    const tdGain = el('td');
    const gainWrap = el('div', 'gain-cell');
    els.gain = document.createElement('input');
    els.gain.type = 'range';
    els.gain.min = '-30';
    els.gain.max = '0';
    els.gain.step = '0.5';
    els.gain.setAttribute('aria-label', '设备响度 dB');
    els.gainLabel = el('b', null, '0 dB');
    els.gain.addEventListener('input', function () {
      const entry = outputDraftEntry(id);
      if (!entry) return;
      entry.gain_db = clamp(finite(els.gain.value, 0), -30, 0);
      entry.touched = true;
      els.gainLabel.textContent = fmtDb(entry.gain_db);
      markDirty('output');
    });
    gainWrap.appendChild(els.gain);
    gainWrap.appendChild(els.gainLabel);
    tdGain.appendChild(gainWrap);
    tr.appendChild(tdGain);

    const tdDelay = el('td');
    els.delay = document.createElement('input');
    els.delay.type = 'number';
    els.delay.min = '0';
    els.delay.max = '2000';
    els.delay.step = '1';
    els.delay.style.width = '74px';
    els.delay.setAttribute('aria-label', '设备延迟补偿 ms');
    els.delay.addEventListener('change', function () {
      const entry = outputDraftEntry(id);
      if (!entry) return;
      entry.delay_ms = clamp(finite(els.delay.value, 0), 0, 2000);
      entry.touched = true;
      els.delay.value = String(Math.round(entry.delay_ms));
      markDirty('output');
    });
    tdDelay.appendChild(els.delay);
    tr.appendChild(tdDelay);

    els.buffer = el('td', 'muted');
    tr.appendChild(els.buffer);
    els.drift = el('td', 'muted');
    tr.appendChild(els.drift);
    els.underruns = el('td', 'muted');
    tr.appendChild(els.underruns);

    const tdOps = el('td');
    const ops = el('div', 'row');
    ops.style.marginTop = '0';
    els.test = el('button', 'btn small', '测试音');
    els.test.type = 'button';
    els.test.addEventListener('click', function () { playTestTone(id, els.test); });
    els.remove = el('button', 'btn small danger', '移除');
    els.remove.type = 'button';
    els.remove.addEventListener('click', function () { removeOutputDevice(id); });
    ops.appendChild(els.test);
    ops.appendChild(els.remove);
    tdOps.appendChild(ops);
    tr.appendChild(tdOps);

    return { tr: tr, els: els };
  }

  function renderOutput() {
    const draft = ensureOutputDraft();
    const tbody = byId('out-devices');
    if (!draft || !tbody) return;
    const probed = new Map();
    for (const info of state.devices) probed.set(info.id, info);
    const live = new Map();
    for (const device of (state.status && state.status.devices) || []) live.set(device.id, device);

    const order = [];
    for (const info of state.devices) {
      if (!state.removedOutput.has(info.id)) order.push(info.id);
    }
    const configured = (state.config && state.config.audio && state.config.audio.devices) || [];
    for (const device of configured) {
      if (probed.has(device.id) || state.removedOutput.has(device.id)) continue;
      if (draft.has(device.id)) order.push(device.id);
    }

    const orderKey = order.join('\u0000');
    if (orderKey !== state.outputOrderKey) {
      state.outputOrderKey = orderKey;
      state.outputRows.clear();
      clear(tbody);
      if (!order.length) {
        const tr = el('tr');
        const td = el('td', 'muted', '未发现 ALSA 输出设备（可点击「刷新设备」重新枚举）');
        td.colSpan = 13;
        tr.appendChild(td);
        tbody.appendChild(tr);
      } else {
        for (const id of order) {
          const row = createOutputRow(id);
          state.outputRows.set(id, row);
          tbody.appendChild(row.tr);
        }
      }
    }

    for (const id of order) {
      const row = state.outputRows.get(id);
      const entry = draft.get(id);
      if (!row || !entry) continue;
      const info = probed.get(id);
      const status = live.get(id);
      const rates = (info && info.sample_rates) || [];

      row.els.name.textContent = (info && info.name) || entry.name || id;
      show(row.els.defaultTag, !!(info && info.is_default));
      row.els.id.textContent = id;
      row.els.online.textContent = info ? (info.online ? '在线' : '离线') : '未检测到';
      row.els.online.className = info ? (info.online ? 'ok-text' : 'muted') : 'warn-text';
      row.els.rates.textContent = rates.length ? rates.map(fmtRate).join(' / ') : '—';
      row.els.channels.textContent = info && info.channels ? info.channels + ' ch' : '—';
      fillRateSelect(row.els.rateSelect, info, entry.sample_rate);
      const liveFormat = status && status.format ? String(status.format) : '';
      if (row.els.formatNote) {
        row.els.formatNote.textContent = liveFormat ? '实际 ' + liveFormat : '';
        show(row.els.formatNote, !!liveFormat);
      }
      row.els.buffer.textContent = status && status.buffer_ms != null ? fmtMs(status.buffer_ms) : '—';
      row.els.drift.textContent = status && status.drift_ppm != null ? fmtPpm(status.drift_ppm) : '—';
      row.els.underruns.textContent = status && status.underruns != null ? fmtCount(status.underruns) : '—';
      row.els.enable.checked = !!entry.enabled;
      row.els.gain.value = String(clamp(entry.gain_db, -30, 0));
      row.els.gainLabel.textContent = fmtDb(entry.gain_db);
      if (document.activeElement !== row.els.delay) row.els.delay.value = String(Math.round(clamp(entry.delay_ms, 0, 2000)));
      row.els.test.disabled = !info;
      row.els.test.title = info ? '播放 1 秒测试音' : '设备当前不可用，无法测试';
      show(row.els.remove, !info);
      row.tr.classList.toggle('offline-row', !info);
    }

    const gone = configured.filter(function (device) {
      return !probed.has(device.id) && !state.removedOutput.has(device.id);
    });
    const banner = byId('out-gone');
    if (banner) {
      if (gone.length) {
        banner.textContent = '以下已配置的输出设备当前未检测到（可能已拔出或声卡编号变化）：' +
          gone.map(function (device) { return device.name || device.id; }).join('、') +
          '。配置会保留，不会导致页面崩溃；可在此移除或重新插拔后刷新。';
        show(banner, true);
      } else if (!state.devices.length) {
        banner.textContent = '未枚举到任何 ALSA 输出设备。';
        show(banner, true);
      } else {
        show(banner, false);
      }
    }
  }

  async function removeOutputDevice(id) {
    if (!window.confirm('从输出设备配置中移除该设备？如果设备只是暂时离线，建议保留配置。')) return;
    state.removedOutput.add(id);
    if (state.drafts.output) state.drafts.output.delete(id);
    const row = state.outputRows.get(id);
    if (row) {
      if (row.tr.parentNode) row.tr.parentNode.removeChild(row.tr);
      state.outputRows.delete(id);
    }
    state.outputOrderKey = '';
    markDirty('output');
    renderOutput();
    toast('已从列表移除，点击「保存并应用」后写回配置');
  }

  async function playTestTone(id, button) {
    button.disabled = true;
    try {
      await request('api/devices/test', jsonInit('POST', { id: id }));
      const mode = state.status && state.status.audio && state.status.audio.mode;
      toast(mode === 'null' ? '后端已受理测试音请求（当前为 null 后端，无实际声音）' : '已请求播放 1 秒测试音');
    } catch (error) {
      toast('测试音失败：' + friendlyError(error), 'error');
    } finally {
      setTimeout(function () { button.disabled = false; }, 900);
    }
  }

  async function refreshDevices(silent) {
    try {
      const result = await request('api/devices');
      state.devices = (result && result.devices) || [];
      state.devicesFetchedAt = Date.now();
      if (!state.dirty.output) state.drafts.output = buildOutputDraft();
      if (state.page === 'output') renderOutput();
      return true;
    } catch (error) {
      if (!silent) toast('设备枚举失败：' + friendlyError(error), 'error');
      return false;
    }
  }

  async function saveOutput() {
    const draft = state.drafts.output;
    const config = state.config;
    if (!draft || !config) return;
    setErrors('out-error', '');
    const payload = clone(config);
    const devices = [];
    for (const entry of draft.values()) {
      if (state.removedOutput.has(entry.id)) continue;
      if (!entry.enabled && !entry.wasConfigured && !entry.touched) continue;
      devices.push({
        id: entry.id,
        name: entry.name || entry.id,
        enabled: !!entry.enabled,
        gain_db: round(clamp(entry.gain_db, -30, 0), 1),
        delay_ms: round(clamp(entry.delay_ms, 0, 2000), 1),
        sample_rate: Math.round(finite(entry.sample_rate, 0)),
      });
    }
    payload.audio.devices = devices;
    let saved = null;
    try {
      saved = await request('api/config', jsonInit('PUT', payload));
    } catch (error) {
      const message = friendlyError(error);
      let persisted = false;
      try {
        const latest = await request('api/config');
        persisted = configLooksApplied(payload, latest);
        if (persisted) {
          state.config = latest;
          state.configSignature = JSON.stringify(latest);
        }
      } catch (_error) { /* keep original error */ }
      if (persisted) {
        await finishOutputSaveAfterPersistedFailure(message);
      } else {
        setErrors('out-error', '保存失败：' + message);
        toast('保存失败：' + message, 'error');
      }
      return;
    }
    if (saved && saved.config) {
      state.config = saved.config;
      state.configSignature = JSON.stringify(saved.config);
    }
    try {
      const refreshed = await request('api/devices/refresh', jsonInit('POST', {}));
      state.devices = (refreshed && refreshed.devices) || state.devices;
      state.devicesFetchedAt = Date.now();
    } catch (error) {
      const message = friendlyError(error);
      clearDirty('output');
      state.removedOutput.clear();
      state.outputOrderKey = '';
      state.drafts.output = buildOutputDraft();
      renderOutput();
      setErrors('out-error', '设备配置已保存，但重新枚举设备失败：' + message + '。可点击「刷新设备」重试。');
      toast('设备配置已保存，枚举失败：' + message, 'error');
      refreshCore();
      return;
    }
    clearDirty('output');
    state.removedOutput.clear();
    state.outputOrderKey = '';
    state.drafts.output = buildOutputDraft();
    renderOutput();
    toast('输出设备设置已保存并应用');
    refreshCore();
  }

  async function finishOutputSaveAfterPersistedFailure(message) {
    try {
      const refreshed = await request('api/devices/refresh', jsonInit('POST', {}));
      state.devices = (refreshed && refreshed.devices) || state.devices;
      state.devicesFetchedAt = Date.now();
    } catch (_error) { /* device list will be refreshed by the poller */ }
    clearDirty('output');
    state.removedOutput.clear();
    state.outputOrderKey = '';
    state.drafts.output = buildOutputDraft();
    renderOutput();
    setErrors('out-error', '设备配置已保存并应用，但协议宿主重启失败：' + message + '。请在「设置 / 诊断」查看日志，必要时重启应用。');
    toast('设备配置已保存，协议宿主重启失败', 'error');
    refreshCore();
  }

  function resetOutput() {
    if (!state.config) return;
    state.drafts.output = buildOutputDraft();
    state.removedOutput.clear();
    state.outputOrderKey = '';
    clearDirty('output');
    setErrors('out-error', '');
    renderOutput();
    toast('已撤销未保存的输出设备修改');
  }

  // ---------------------------------------------------------------------------
  // 音频校准 page — global gain + graphical custom curve editor
  // ---------------------------------------------------------------------------
  function calibrationDraft() {
    return state.drafts.calibration;
  }
  function curveKey(draft) {
    if (!draft) return '';
    if (draft.kind !== 'custom') return draft.kind;
    return 'custom|' + draft.points.map(function (point) {
      return round(point[0], 4) + ',' + round(point[1], 4);
    }).join(';');
  }
  function curvePointsForApi(draft) {
    return draft.points.map(function (point) { return [round(point[0], 4), round(point[1], 4)]; });
  }

  let previewTimer = null;
  function schedulePreview(delay) {
    clearTimeout(previewTimer);
    previewTimer = setTimeout(function () { fetchPreview(); }, delay == null ? 160 : delay);
  }

  async function fetchPreview() {
    const draft = calibrationDraft();
    if (!draft) return;
    const sequence = ++state.previewSeq;
    const key = curveKey(draft);
    const curve = {
      kind: draft.kind,
      points: draft.kind === 'custom' ? curvePointsForApi(draft) : [],
    };
    try {
      const result = await request('api/calibration/preview', jsonInit('POST', { curve: curve }));
      if (sequence !== state.previewSeq) return;
      state.preview = { key: key, kind: draft.kind, samples: (result && result.samples) || [], ok: true };
    } catch (error) {
      if (sequence !== state.previewSeq) return;
      state.preview = { key: key, kind: draft.kind, samples: [], ok: false, error: friendlyError(error) };
    }
    updatePreviewHint();
    drawCurve();
  }

  function updatePreviewHint() {
    const draft = calibrationDraft();
    const node = byId('curve-preview-hint');
    if (!node || !draft) return;
    let prefix = '正在获取曲线预览…（POST /api/calibration/preview）';
    if (state.preview && state.preview.ok && state.preview.key === curveKey(draft)) {
      prefix = '实时预览：已通过 POST /api/calibration/preview 获取 ' + state.preview.samples.length + ' 个采样点。';
    } else if (state.preview && !state.preview.ok && state.preview.key === curveKey(draft)) {
      prefix = '预览接口不可用（' + state.preview.error + '），画布显示本地估算。';
    }
    node.textContent = prefix + ' x 与 y 均须单调不减（音量提高时输出不能下降）；首尾控制点固定为 (0,0) 与 (1,1)。';
  }

  function localCustomSamples(points) {
    const all = [[0, 0]].concat(points.slice().sort(function (a, b) { return a[0] - b[0]; }), [[1, 1]]);
    const samples = [];
    for (let index = 0; index <= 100; index += 1) {
      const x = index / 100;
      let y = x;
      for (let i = 0; i < all.length - 1; i += 1) {
        const x0 = all[i][0];
        const y0 = all[i][1];
        const x1 = all[i + 1][0];
        const y1 = all[i + 1][1];
        if (x >= x0 && x <= x1) {
          y = x1 - x0 <= 1e-9 ? y1 : y0 + (y1 - y0) * ((x - x0) / (x1 - x0));
          break;
        }
      }
      samples.push({ logical: x, output: clamp(y, 0, 1) });
    }
    return samples;
  }

  function drawSamples(draft) {
    if (!draft) return null;
    if (draft.kind === 'custom') {
      if (state.preview && state.preview.ok && state.preview.key === curveKey(draft) && state.preview.samples.length) {
        return state.preview.samples;
      }
      return localCustomSamples(draft.points);
    }
    if (state.preview && state.preview.kind === draft.kind && state.preview.samples.length) {
      return state.preview.samples;
    }
    return null;
  }

  function drawCurve() {
    const canvas = byId('curve-canvas');
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const width = Math.max(260, Math.round(rect.width || 640));
    const height = Math.max(180, Math.round(rect.height || 260));
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    if (canvas.width !== Math.round(width * dpr) || canvas.height !== Math.round(height * dpr)) {
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
    }
    const context = canvas.getContext('2d');
    if (!context) return;
    context.setTransform(dpr, 0, 0, dpr, 0, 0);
    context.clearRect(0, 0, width, height);

    const pad = { left: 46, right: 18, top: 16, bottom: 30 };
    const plotW = Math.max(1, width - pad.left - pad.right);
    const plotH = Math.max(1, height - pad.top - pad.bottom);

    context.lineWidth = 1;
    context.strokeStyle = 'rgba(139, 148, 158, 0.16)';
    context.fillStyle = '#8b949e';
    context.font = '10px -apple-system, "PingFang SC", "Microsoft YaHei", sans-serif';
    context.textAlign = 'center';
    context.textBaseline = 'top';
    for (let i = 0; i <= 4; i += 1) {
      const x = pad.left + plotW * (i / 4);
      context.beginPath();
      context.moveTo(x, pad.top);
      context.lineTo(x, pad.top + plotH);
      context.stroke();
      context.fillText((i * 25) + '%', x, pad.top + plotH + 8);
    }
    context.textAlign = 'right';
    context.textBaseline = 'middle';
    for (let i = 0; i <= 4; i += 1) {
      const y = pad.top + plotH * (1 - i / 4);
      context.beginPath();
      context.moveTo(pad.left, y);
      context.lineTo(pad.left + plotW, y);
      context.stroke();
      context.fillText((i * 25) + '%', pad.left - 8, y);
    }
    context.strokeStyle = 'rgba(139, 148, 158, 0.4)';
    context.strokeRect(pad.left, pad.top, plotW, plotH);

    context.textAlign = 'center';
    context.textBaseline = 'top';
    context.fillText('发送端逻辑音量 →', pad.left + plotW / 2, height - 14);

    const draft = calibrationDraft();
    if (!draft) return;
    const samples = drawSamples(draft);
    if (samples && samples.length) {
      context.strokeStyle = '#58a6ff';
      context.lineWidth = 2;
      context.beginPath();
      samples.forEach(function (point, index) {
        const x = pad.left + clamp(point.logical, 0, 1) * plotW;
        const y = pad.top + (1 - clamp(point.output, 0, 1)) * plotH;
        if (index === 0) context.moveTo(x, y);
        else context.lineTo(x, y);
      });
      context.stroke();
    }

    if (draft.kind === 'custom') {
      const all = [[0, 0]].concat(draft.points, [[1, 1]]);
      all.forEach(function (point, index) {
        const internal = index > 0 && index < all.length - 1;
        const x = pad.left + clamp(point[0], 0, 1) * plotW;
        const y = pad.top + (1 - clamp(point[1], 0, 1)) * plotH;
        context.beginPath();
        context.arc(x, y, internal ? 5.5 : 4.5, 0, Math.PI * 2);
        context.fillStyle = internal ? '#58a6ff' : '#8b949e';
        context.fill();
        if (internal && state.selectedPoint === index - 1) {
          context.strokeStyle = '#ffffff';
          context.lineWidth = 2;
          context.stroke();
        }
      });
    }
  }

  function renderCurvePointsText(draft) {
    const list = [[0, 0]].concat(draft.points, [[1, 1]]).map(function (point) {
      return Math.round(point[0] * 100) + '%, ' + Math.round(point[1] * 100) + '%';
    });
    setText('curve-points', '控制点：' + list.join(' → ') + '（共 ' + list.length + ' 个，首尾固定）');
  }

  function renderCurveButtons(draft) {
    const custom = !!draft && draft.kind === 'custom';
    const add = byId('curve-add');
    const remove = byId('curve-remove');
    const reset = byId('curve-reset');
    const canvas = byId('curve-canvas');
    if (add) add.disabled = !custom;
    if (remove) remove.disabled = !custom || state.selectedPoint < 0;
    if (reset) reset.disabled = !custom;
    if (canvas) canvas.classList.toggle('readonly', !custom);
    setText('curve-hint', custom ? '拖动控制点编辑 · 双击空白处新增 · 双击控制点删除' : '选择「自定义」后可拖动控制点');
  }

  function renderCalibrationDevices(draft) {
    const tbody = byId('cal-devices');
    const empty = byId('cal-device-empty');
    if (!tbody) return;
    const devices = draft.devices || [];
    show(empty, devices.length === 0);
    const key = devices.map(function (device) { return device.id; }).join('\u0000');
    if (key !== state.calDeviceKey) {
      state.calDeviceKey = key;
      state.calDeviceRows.clear();
      clear(tbody);
      for (const device of devices) {
        const row = createCalibrationDeviceRow(device.id);
        state.calDeviceRows.set(device.id, row);
        tbody.appendChild(row.tr);
      }
    }
    for (const device of devices) {
      const row = state.calDeviceRows.get(device.id);
      if (!row) continue;
      row.name.textContent = device.name || device.id;
      row.id.textContent = device.id;
      row.enabled.textContent = device.enabled ? '已启用' : '未启用';
      row.enabled.className = device.enabled ? 'ok-text' : 'muted';
      row.gain.value = String(clamp(device.gain_db, -30, 0));
      row.gainLabel.textContent = fmtDb(device.gain_db);
      if (document.activeElement !== row.delay) row.delay.value = String(Math.round(clamp(device.delay_ms, 0, 2000)));
    }
  }

  function calibrationDeviceEntry(id) {
    const draft = calibrationDraft();
    if (!draft) return null;
    for (const device of draft.devices) if (device.id === id) return device;
    return null;
  }

  function createCalibrationDeviceRow(id) {
    const tr = el('tr');
    const els = {};
    els.name = el('td', 'name-cell');
    els.id = el('td', 'mono muted');
    els.enabled = el('td');
    tr.appendChild(els.name);
    tr.appendChild(els.id);
    tr.appendChild(els.enabled);

    const tdGain = el('td');
    const gainWrap = el('div', 'gain-cell');
    els.gain = document.createElement('input');
    els.gain.type = 'range';
    els.gain.min = '-30';
    els.gain.max = '0';
    els.gain.step = '0.5';
    els.gain.setAttribute('aria-label', '设备响度 dB');
    els.gainLabel = el('b', null, '0 dB');
    els.gain.addEventListener('input', function () {
      const entry = calibrationDeviceEntry(id);
      if (!entry) return;
      entry.gain_db = clamp(finite(els.gain.value, 0), -30, 0);
      els.gainLabel.textContent = fmtDb(entry.gain_db);
      markDirty('calibration');
    });
    gainWrap.appendChild(els.gain);
    gainWrap.appendChild(els.gainLabel);
    tdGain.appendChild(gainWrap);
    tr.appendChild(tdGain);

    const tdDelay = el('td');
    els.delay = document.createElement('input');
    els.delay.type = 'number';
    els.delay.min = '0';
    els.delay.max = '2000';
    els.delay.step = '1';
    els.delay.style.width = '74px';
    els.delay.setAttribute('aria-label', '设备延迟补偿 ms');
    els.delay.addEventListener('change', function () {
      const entry = calibrationDeviceEntry(id);
      if (!entry) return;
      entry.delay_ms = clamp(finite(els.delay.value, 0), 0, 2000);
      els.delay.value = String(Math.round(entry.delay_ms));
      markDirty('calibration');
    });
    tdDelay.appendChild(els.delay);
    tr.appendChild(tdDelay);
    return { tr: tr, els: els, name: els.name, id: els.id, enabled: els.enabled, gain: els.gain, gainLabel: els.gainLabel, delay: els.delay };
  }

  function renderCalibration() {
    const draft = calibrationDraft();
    if (!draft) return;
    const globalInput = byId('cal-global');
    const globalValue = Math.round(clamp(draft.global, 0, 100));
    if (globalInput && document.activeElement !== globalInput) globalInput.value = String(globalValue);
    setText('cal-global-label', globalValue + '%');
    const muted = byId('cal-muted');
    if (muted) muted.checked = !!draft.muted;
    const kindSelect = byId('cal-curve-kind');
    if (kindSelect && document.activeElement !== kindSelect) kindSelect.value = draft.kind;
    const label = CURVE_LABELS[draft.kind] || draft.kind;
    setText('cal-mode-badge', label);
    setText('cal-status-badge', label + (draft.muted ? ' · 已静音' : ' · ' + globalValue + '%'));
    renderCurveButtons(draft);
    renderCurvePointsText(draft);
    updatePreviewHint();
    drawCurve();
    renderCalibrationDevices(draft);
  }

  function canvasGeometry() {
    const canvas = byId('curve-canvas');
    const rect = canvas.getBoundingClientRect();
    const pad = { left: 46, right: 18, top: 16, bottom: 30 };
    const plotW = Math.max(1, rect.width - pad.left - pad.right);
    const plotH = Math.max(1, rect.height - pad.top - pad.bottom);
    return { canvas: canvas, rect: rect, pad: pad, plotW: plotW, plotH: plotH };
  }
  function pointerToCurve(event) {
    const geometry = canvasGeometry();
    const px = event.clientX - geometry.rect.left;
    const py = event.clientY - geometry.rect.top;
    return {
      x: clamp((px - geometry.pad.left) / geometry.plotW, 0, 1),
      y: clamp(1 - (py - geometry.pad.top) / geometry.plotH, 0, 1),
      px: px,
      py: py,
      geometry: geometry,
    };
  }
  function hitCurvePoint(event) {
    const draft = calibrationDraft();
    if (!draft) return -1;
    const position = pointerToCurve(event);
    const geometry = position.geometry;
    let best = -1;
    let bestDistance = 18;
    draft.points.forEach(function (point, index) {
      const x = geometry.pad.left + point[0] * geometry.plotW;
      const y = geometry.pad.top + (1 - point[1]) * geometry.plotH;
      const distance = Math.hypot(position.px - x, position.py - y);
      if (distance < bestDistance) {
        bestDistance = distance;
        best = index;
      }
    });
    return best;
  }

  function curveNeighbours(draft, index) {
    return {
      previous: index === 0 ? [0, 0] : draft.points[index - 1],
      next: index === draft.points.length - 1 ? [1, 1] : draft.points[index + 1],
    };
  }
  function setCurvePoint(draft, index, x, y) {
    const neighbours = curveNeighbours(draft, index);
    const gap = 0.002;
    const nx = clamp(x, neighbours.previous[0] + gap, neighbours.next[0] - gap);
    const ny = clamp(y, neighbours.previous[1], neighbours.next[1]);
    draft.points[index] = [round(nx, 4), round(ny, 4)];
  }
  function insertCurvePointAt(x) {
    const draft = calibrationDraft();
    if (!draft) return false;
    const all = [[0, 0]].concat(draft.points, [[1, 1]]);
    let insertIndex = -1;
    for (let i = 0; i < all.length - 1; i += 1) {
      if (x > all[i][0] && x < all[i + 1][0]) {
        insertIndex = i;
        break;
      }
    }
    if (insertIndex < 0) {
      toast('该位置无法新增控制点（过于靠近端点或已有控制点）', 'error');
      return false;
    }
    const x0 = all[insertIndex][0];
    const y0 = all[insertIndex][1];
    const x1 = all[insertIndex + 1][0];
    const y1 = all[insertIndex + 1][1];
    const span = x1 - x0;
    if (span < 0.04) {
      toast('控制点过密，请先移动或删除相邻控制点', 'error');
      return false;
    }
    const margin = Math.min(0.02, span / 2);
    const nx = clamp(x, x0 + margin, x1 - margin);
    const t = (nx - x0) / span;
    const ny = clamp(y0 + (y1 - y0) * t, y0, y1);
    draft.points.splice(insertIndex, 0, [round(nx, 4), round(ny, 4)]);
    state.selectedPoint = insertIndex;
    return true;
  }

  function onCurvePointerDown(event) {
    const draft = calibrationDraft();
    if (!draft) return;
    if (draft.kind !== 'custom') {
      toast('请先选择「自定义」曲线后再编辑控制点');
      return;
    }
    const index = hitCurvePoint(event);
    state.selectedPoint = index;
    if (index >= 0) {
      state.draggingPoint = index;
      try { byId('curve-canvas').setPointerCapture(event.pointerId); } catch (_error) { /* ignore */ }
    }
    renderCurveButtons(draft);
    drawCurve();
    event.preventDefault();
  }

  function onCurvePointerMove(event) {
    if (state.draggingPoint < 0) return;
    const draft = calibrationDraft();
    if (!draft) return;
    const index = state.draggingPoint;
    if (index >= draft.points.length) { state.draggingPoint = -1; return; }
    const position = pointerToCurve(event);
    setCurvePoint(draft, index, position.x, position.y);
    markDirty('calibration');
    renderCurvePointsText(draft);
    updatePreviewHint();
    drawCurve();
    schedulePreview();
    event.preventDefault();
  }

  function onCurvePointerUp(event) {
    if (state.draggingPoint < 0) return;
    state.draggingPoint = -1;
    try { byId('curve-canvas').releasePointerCapture(event.pointerId); } catch (_error) { /* ignore */ }
    renderCurveButtons(calibrationDraft());
    schedulePreview(0);
  }

  function onCurveDoubleClick(event) {
    const draft = calibrationDraft();
    if (!draft) return;
    if (draft.kind !== 'custom') {
      toast('请先选择「自定义」曲线后再编辑控制点');
      return;
    }
    const index = hitCurvePoint(event);
    if (index >= 0) {
      draft.points.splice(index, 1);
      state.selectedPoint = -1;
    } else {
      const position = pointerToCurve(event);
      if (!insertCurvePointAt(position.x)) return;
    }
    markDirty('calibration');
    renderCalibration();
    schedulePreview(0);
    event.preventDefault();
  }

  function onCurveKeyDown(event) {
    const draft = calibrationDraft();
    if (!draft || draft.kind !== 'custom') return;
    if (event.key === 'Delete' || event.key === 'Backspace') {
      if (state.selectedPoint >= 0 && state.selectedPoint < draft.points.length) {
        event.preventDefault();
        draft.points.splice(state.selectedPoint, 1);
        state.selectedPoint = -1;
        markDirty('calibration');
        renderCalibration();
        schedulePreview(0);
      }
      return;
    }
    const nudges = { ArrowLeft: [-0.01, 0], ArrowRight: [0.01, 0], ArrowUp: [0, 0.01], ArrowDown: [0, -0.01] };
    const nudge = nudges[event.key];
    if (!nudge) return;
    if (state.selectedPoint < 0 || state.selectedPoint >= draft.points.length) return;
    event.preventDefault();
    const current = draft.points[state.selectedPoint];
    setCurvePoint(draft, state.selectedPoint, current[0] + nudge[0], current[1] + nudge[1]);
    markDirty('calibration');
    renderCalibration();
    schedulePreview();
  }

  function addCurvePoint() {
    const draft = calibrationDraft();
    if (!draft) return;
    if (draft.kind !== 'custom') {
      toast('请先选择「自定义」曲线');
      return;
    }
    const all = [[0, 0]].concat(draft.points, [[1, 1]]);
    let bestIndex = 0;
    let bestGap = -1;
    for (let i = 0; i < all.length - 1; i += 1) {
      const gap = all[i + 1][0] - all[i][0];
      if (gap > bestGap) {
        bestGap = gap;
        bestIndex = i;
      }
    }
    if (bestGap < 0.04) {
      toast('控制点已足够密集，无法再添加');
      return;
    }
    const middle = (all[bestIndex][0] + all[bestIndex + 1][0]) / 2;
    if (!insertCurvePointAt(middle)) return;
    markDirty('calibration');
    renderCalibration();
    schedulePreview(0);
  }

  function removeCurvePoint() {
    const draft = calibrationDraft();
    if (!draft) return;
    if (state.selectedPoint < 0 || state.selectedPoint >= draft.points.length) {
      toast('请先在画布上选中一个控制点');
      return;
    }
    draft.points.splice(state.selectedPoint, 1);
    state.selectedPoint = -1;
    markDirty('calibration');
    renderCalibration();
    schedulePreview(0);
  }

  function resetCurvePoints() {
    const draft = calibrationDraft();
    if (!draft) return;
    draft.points = [];
    state.selectedPoint = -1;
    markDirty('calibration');
    renderCalibration();
    schedulePreview(0);
  }

  async function saveCalibration() {
    const draft = calibrationDraft();
    const config = state.config;
    if (!draft || !config) return;
    setErrors('cal-error', '');
    const payload = {
      global_attenuation_percent: round(clamp(draft.global, 0, 100), 1),
      muted: !!draft.muted,
      curve: { kind: draft.kind, points: curvePointsForApi(draft) },
      devices: draft.devices.map(function (device) {
        return {
          id: device.id,
          enabled: !!device.enabled,
          gain_db: round(clamp(device.gain_db, -30, 0), 1),
          delay_ms: round(clamp(device.delay_ms, 0, 2000), 1),
        };
      }),
    };
    try {
      const result = await request('api/calibration', jsonInit('PUT', payload));
      if (result && result.audio) {
        state.config = Object.assign({}, state.config, { audio: result.audio });
        state.configSignature = JSON.stringify(state.config);
      }
      state.drafts.calibration = buildCalibrationDraft();
      clearDirty('calibration');
      state.selectedPoint = -1;
      renderCalibration();
      schedulePreview(0);
      toast('校准参数已保存并应用（发送端主音量未改变）');
      refreshCore();
    } catch (error) {
      const message = friendlyError(error);
      setErrors('cal-error', '保存失败：' + message);
      toast('保存失败：' + message, 'error');
    }
  }

  async function resetCalibration() {
    if (!window.confirm('恢复默认校准参数？\n全局输出衰减回到 100%，曲线回到「均衡」，取消静音；每设备响度 / 延迟保留。')) return;
    const draft = calibrationDraft();
    if (!draft) return;
    draft.global = 100;
    draft.muted = false;
    draft.kind = 'balanced';
    draft.points = [];
    state.selectedPoint = -1;
    markDirty('calibration');
    renderCalibration();
    await saveCalibration();
    toast('已恢复默认全局校准（每设备 dB / 延迟保留）');
  }

  // ---------------------------------------------------------------------------
  // 设置 / 诊断 page
  // ---------------------------------------------------------------------------
  function protocolSummary() {
    const config = state.config;
    const runtime = (state.status && state.status.protocols) || {};
    return PROTOCOLS.map(function (protocol) {
      const enabled = config ? !!(config.protocols[protocol.key] && config.protocols[protocol.key].enabled) : false;
      const info = runtime[protocol.key] || {};
      return protocol.label + (enabled ? ' 开' : ' 关') + '（' + protocolStateLabel(info.state, enabled) + '）';
    }).join(' · ');
  }

  function addKV(container, label, value) {
    const item = el('div');
    item.appendChild(el('span', null, label));
    item.appendChild(el('b', null, value));
    container.appendChild(item);
  }

  function renderConfigSummary() {
    const node = byId('config-summary');
    if (!node) return;
    clear(node);
    const config = state.config;
    if (!config) {
      node.appendChild(el('span', 'muted', '配置加载中…'));
      return;
    }
    const enabledDevices = (config.audio.devices || []).filter(function (device) { return device.enabled; }).length;
    addKV(node, '广播名称', config.receiver.name || '—');
    addKV(node, '网络接口', config.receiver.interface || '自动');
    addKV(node, '输出设备', enabledDevices + ' 启用 / ' + (config.audio.devices || []).length + ' 已配置');
    addKV(node, '全局衰减', fmtPercent(config.audio.global_attenuation_percent));
    addKV(node, '音量曲线', CURVE_LABELS[config.audio.curve.kind] || config.audio.curve.kind);
    addKV(node, '静音输出', config.audio.muted ? '是' : '否');
    addKV(node, '日志级别', config.logging.level);
  }

  function renderDiagnostics() {
    const diagnostics = state.diagnostics;
    if (!diagnostics) return;
    setText('diag-backend', diagnostics.backend || '—');
    setText('diag-logpath', diagnostics.log_path || '—');
    const chips = byId('diag-interfaces');
    if (chips) {
      clear(chips);
      const interfaces = diagnostics.interfaces || [];
      if (!interfaces.length) chips.appendChild(el('span', 'muted', '未枚举到网络接口'));
      for (const name of interfaces) chips.appendChild(el('span', 'chip', name));
    }
    setText('diag-cards', String(diagnostics.alsa_cards || '').trim() || '（/proc/asound/cards 不可读）');
    setText('diag-pcm', String(diagnostics.alsa_pcm || '').trim() || '（/proc/asound/pcm 不可读）');
  }

  function renderLogs() {
    const pre = byId('logs');
    if (!pre) return;
    const lines = state.logs || [];
    pre.textContent = lines.length ? lines.join('\n') : '（暂无日志）';
    if (state.logsPinned) pre.scrollTop = pre.scrollHeight;
    setText('logs-hint', '显示后端最近 ' + lines.length + ' 行日志（最多 200 行，来自 GET /api/logs）；日志文件达上限后端会自动轮转。');
  }

  function renderSettings() {
    const status = state.status || {};
    const audio = status.audio || {};
    const diagnostics = state.diagnostics || {};
    setText('set-version', status.version ? 'v' + status.version : '—');
    setText('set-uptime', status.uptime != null ? fmtUptime(status.uptime) : '—');
    setText('set-backend', audio.mode ? String(audio.mode) : (diagnostics.backend || '—'));
    setText('set-protocols', protocolSummary());
    setText('set-logpath', diagnostics.log_path || '—');
    setText('set-build', status.version ? 'fusionplay-fnos ' + status.version : '—');
    const health = byId('set-health');
    if (health) {
      health.textContent = state.failures === 0 && state.status ? '在线' : '离线';
      health.className = 'badge ' + (state.failures === 0 && state.status ? 'ok' : 'bad');
    }
    setText('set-source', status.current_source ? (SOURCE_LABELS[status.current_source] || status.current_source) : '空闲');
    setText('set-frames', fmtCount(audio.master_frames));
    setText('set-dropped', fmtCount(audio.dropped_frames));
    const devices = status.devices || [];
    const enabledDevices = devices.filter(function (device) { return device.enabled; }).length;
    setText('set-devices', devices.length ? enabledDevices + ' / ' + devices.length + ' 启用' : '—');
    let underruns = 0;
    for (const device of devices) underruns += finite(device.underruns, 0);
    setText('set-underruns', devices.length ? fmtCount(underruns) : '—');
    const level = (status.logging && status.logging.level) ||
      (state.config && state.config.logging && state.config.logging.level) || '—';
    setText('set-level', level);
    const levelSelect = byId('log-level');
    if (levelSelect && document.activeElement !== levelSelect) levelSelect.value = level;

    const errorsNode = byId('set-errors');
    if (errorsNode) {
      const errors = mergedErrors();
      errorsNode.textContent = errors.length ? errors.join('\n') : '暂无错误';
      errorsNode.classList.toggle('ok', errors.length === 0);
    }
    renderConfigSummary();
    renderDiagnostics();
  }

  async function refreshDiagnostics(force) {
    const now = Date.now();
    if (!force && state.diagnostics && now - state.diagFetchedAt < 4000) {
      if (state.page === 'protocols') renderProtocols();
      if (state.page === 'settings') renderSettings();
      return true;
    }
    try {
      const diagnostics = await request('api/diagnostics');
      state.diagnostics = diagnostics;
      state.diagFetchedAt = Date.now();
      if (state.page === 'protocols') renderProtocols();
      if (state.page === 'settings') renderSettings();
      return true;
    } catch (error) {
      toast('诊断信息加载失败：' + friendlyError(error), 'error');
      return false;
    }
  }

  async function refreshLogs(silent) {
    try {
      const result = await request('api/logs?lines=200');
      state.logs = (result && result.lines) || [];
      state.logsFetchedAt = Date.now();
      if (state.page === 'overview') renderOverviewEvents();
      if (state.page === 'settings') renderLogs();
      return true;
    } catch (error) {
      if (!silent) toast('日志加载失败：' + friendlyError(error), 'error');
      return false;
    }
  }

  async function exportConfig() {
    try {
      const result = await request('api/export');
      const config = result && result.config ? result.config : result;
      if (!config) throw new Error('导出内容为空');
      const blob = new Blob([JSON.stringify(config, null, 2) + '\n'], { type: 'application/json;charset=utf-8' });
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = 'fusionplay-fnos-config-' + timestampLabel() + '.json';
      document.body.appendChild(link);
      link.click();
      document.body.removeChild(link);
      setTimeout(function () { URL.revokeObjectURL(url); }, 1500);
      toast('配置已导出为 JSON 文件');
    } catch (error) {
      toast('导出失败：' + friendlyError(error), 'error');
    }
  }

  async function importConfigFile(file, input) {
    setErrors('import-error', '');
    if (!file) return;
    if (file.size > 1024 * 1024) {
      setErrors('import-error', '配置文件超过 1 MiB（后端最大请求体 1 MiB），已取消导入。');
      if (input) input.value = '';
      return;
    }
    let payload = null;
    try {
      const text = await file.text();
      const parsed = JSON.parse(text);
      payload = parsed && typeof parsed === 'object' && parsed.config && typeof parsed.config === 'object'
        ? { config: parsed.config }
        : { config: parsed };
      const result = await request('api/import', jsonInit('POST', payload));
      if (result && result.config) {
        applyConfig(result.config);
      }
      clearAllDirty();
      rebuildAllDrafts();
      renderActivePage();
      toast('配置已导入并应用（协议宿主已按新配置重启）');
      refreshCore();
    } catch (error) {
      const message = error instanceof SyntaxError ? 'JSON 解析失败：' + error.message : friendlyError(error);
      let persisted = false;
      if (payload && !(error instanceof SyntaxError)) {
        try {
          const latest = await request('api/config');
          persisted = configLooksApplied(payload.config, latest);
          if (persisted) {
            state.config = latest;
            state.configSignature = JSON.stringify(latest);
          }
        } catch (_error) { /* keep original error */ }
      }
      if (persisted) {
        clearAllDirty();
        rebuildAllDrafts();
        renderActivePage();
        setErrors('import-error', '配置已导入并落盘，但协议宿主重启失败：' + message + '。请在「设置 / 诊断」查看日志，必要时重启应用。');
        toast('配置已导入，协议宿主重启失败', 'error');
        refreshCore();
      } else {
        setErrors('import-error', '导入失败：' + message);
        toast('导入失败：' + message, 'error');
      }
    } finally {
      if (input) input.value = '';
    }
  }

  async function applyLogLevel() {
    const select = byId('log-level');
    if (!select) return;
    const level = select.value;
    try {
      const result = await request('api/logs/level', jsonInit('POST', { level: level }));
      const applied = (result && result.level) || level;
      if (state.status) {
        state.status.logging = { level: applied };
      }
      if (state.config) state.config.logging.level = applied;
      state.configSignature = state.config ? JSON.stringify(state.config) : state.configSignature;
      toast('日志级别已切换为 ' + applied);
      renderSettings();
    } catch (error) {
      toast('切换日志级别失败：' + friendlyError(error), 'error');
    }
  }

  // ---------------------------------------------------------------------------
  // Errors helper
  // ---------------------------------------------------------------------------
  function setErrors(id, message) {
    const node = byId(id);
    if (!node) return;
    if (!message) {
      node.textContent = '';
      node.classList.add('hidden');
      return;
    }
    node.textContent = message;
    node.classList.remove('hidden');
  }

  // ---------------------------------------------------------------------------
  // Polling / routing
  // ---------------------------------------------------------------------------
  function renderActivePage() {
    switch (state.page) {
      case 'overview': renderOverview(); break;
      case 'protocols': renderProtocols(); break;
      case 'output': renderOutput(); break;
      case 'calibration': renderCalibration(); break;
      case 'settings': renderSettings(); break;
      default: break;
    }
  }

  async function refreshCore() {
    try {
      const results = await Promise.all([request('api/status'), request('api/config')]);
      state.status = results[0];
      applyConfig(results[1]);
      setConnectivity(true);
      renderHeader();
      renderActivePage();
      return true;
    } catch (error) {
      setConnectivity(false, friendlyError(error));
      return false;
    }
  }

  function currentPageFromHash() {
    const raw = String(location.hash || '').replace(/^#\/?/, '').split(/[/?]/)[0];
    return PAGE_IDS.indexOf(raw) >= 0 ? raw : 'overview';
  }

  function applyRoute() {
    const page = currentPageFromHash();
    const previous = state.page;
    state.page = page;
    for (const id of PAGE_IDS) {
      const section = byId('page-' + id);
      if (section) section.classList.toggle('hidden', id !== page);
      const button = document.querySelector('.tab[data-page="' + id + '"]');
      if (button) {
        button.classList.toggle('active', id === page);
        if (id === page) button.setAttribute('aria-current', 'page');
        else button.removeAttribute('aria-current');
      }
    }
    renderActivePage();
    if (previous !== page) onPageEnter(page);
  }

  function onPageEnter(page) {
    if (page === 'overview' || page === 'settings') refreshLogs(true);
    if (page === 'protocols' || page === 'settings') refreshDiagnostics(false);
    if (page === 'output') refreshDevices(true);
    if (page === 'calibration') schedulePreview(0);
  }

  let pollTimer = null;
  function startPolling() {
    stopPolling();
    if (!document.hidden) pollTimer = setInterval(tick, POLL_MS);
  }
  function stopPolling() {
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = null;
  }

  async function tick() {
    if (state.tickBusy || document.hidden) return;
    state.tickBusy = true;
    try {
      await refreshCore();
      const now = Date.now();
      if (state.page === 'output') await refreshDevices(true);
      if (state.page === 'overview' || state.page === 'settings') {
        if (now - state.logsFetchedAt > LOGS_REFRESH_MS) {
          const auto = byId('logs-auto');
          if (state.page !== 'settings' || !auto || auto.checked) await refreshLogs(true);
        }
      }
      if (state.page === 'settings' && now - state.diagFetchedAt > DIAG_REFRESH_MS) {
        await refreshDiagnostics(true);
      }
    } finally {
      state.tickBusy = false;
    }
  }

  async function manualRefresh() {
    toast('正在刷新…');
    await refreshCore();
    if (state.page === 'output') await refreshDevices(true);
    if (state.page === 'overview' || state.page === 'settings') await refreshLogs(true);
    if (state.page === 'protocols' || state.page === 'settings') await refreshDiagnostics(true);
    renderActivePage();
  }

  // ---------------------------------------------------------------------------
  // Event wiring
  // ---------------------------------------------------------------------------
  function bindStaticEvents() {
    const refreshAll = byId('refresh-all');
    if (refreshAll) refreshAll.addEventListener('click', manualRefresh);
    const connRetry = byId('conn-retry');
    if (connRetry) connRetry.addEventListener('click', manualRefresh);

    for (const button of document.querySelectorAll('.tab[data-page]')) {
      button.addEventListener('click', function () {
        const page = button.dataset.page;
        if (currentPageFromHash() !== page) location.hash = '#/' + page;
        else applyRoute();
      });
    }

    // 接收协议
    const saveProtocolsButton = byId('save-protocols');
    if (saveProtocolsButton) saveProtocolsButton.addEventListener('click', saveProtocols);
    const resetProtocolsButton = byId('reset-protocols');
    if (resetProtocolsButton) resetProtocolsButton.addEventListener('click', resetProtocols);

    // 音频输出
    const saveOutputButton = byId('save-output');
    if (saveOutputButton) saveOutputButton.addEventListener('click', saveOutput);
    const resetOutputButton = byId('reset-output');
    if (resetOutputButton) resetOutputButton.addEventListener('click', resetOutput);
    const refreshDevicesButton = byId('refresh-devices');
    if (refreshDevicesButton) {
      refreshDevicesButton.addEventListener('click', async function () {
        refreshDevicesButton.disabled = true;
        try {
          const result = await request('api/devices/refresh', jsonInit('POST', {}));
          state.devices = (result && result.devices) || state.devices;
          state.devicesFetchedAt = Date.now();
          state.drafts.output = buildOutputDraft();
          state.outputOrderKey = '';
          clearDirty('output');
          state.removedOutput.clear();
          renderOutput();
          toast('设备列表已重新枚举');
        } catch (error) {
          toast('刷新设备失败：' + friendlyError(error), 'error');
        } finally {
          refreshDevicesButton.disabled = false;
        }
      });
    }

    // 音频校准
    const globalInput = byId('cal-global');
    if (globalInput) {
      globalInput.addEventListener('input', function () {
        const draft = calibrationDraft();
        if (!draft) return;
        draft.global = clamp(finite(globalInput.value, 100), 0, 100);
        setText('cal-global-label', Math.round(draft.global) + '%');
        markDirty('calibration');
        renderCalibrationBadgesOnly(draft);
      });
    }
    const muted = byId('cal-muted');
    if (muted) {
      muted.addEventListener('change', function () {
        const draft = calibrationDraft();
        if (!draft) return;
        draft.muted = muted.checked;
        markDirty('calibration');
        renderCalibrationBadgesOnly(draft);
      });
    }
    const kindSelect = byId('cal-curve-kind');
    if (kindSelect) {
      kindSelect.addEventListener('change', function () {
        const draft = calibrationDraft();
        if (!draft) return;
        draft.kind = kindSelect.value;
        state.selectedPoint = -1;
        markDirty('calibration');
        renderCalibration();
        schedulePreview(0);
      });
    }
    const saveCalButton = byId('save-cal');
    if (saveCalButton) saveCalButton.addEventListener('click', saveCalibration);
    const resetCalButton = byId('reset-cal');
    if (resetCalButton) resetCalButton.addEventListener('click', resetCalibration);
    const curveAdd = byId('curve-add');
    if (curveAdd) curveAdd.addEventListener('click', addCurvePoint);
    const curveRemove = byId('curve-remove');
    if (curveRemove) curveRemove.addEventListener('click', removeCurvePoint);
    const curveReset = byId('curve-reset');
    if (curveReset) curveReset.addEventListener('click', resetCurvePoints);

    const canvas = byId('curve-canvas');
    if (canvas) {
      canvas.addEventListener('pointerdown', onCurvePointerDown);
      canvas.addEventListener('pointermove', onCurvePointerMove);
      canvas.addEventListener('pointerup', onCurvePointerUp);
      canvas.addEventListener('pointercancel', onCurvePointerUp);
      canvas.addEventListener('dblclick', onCurveDoubleClick);
      canvas.addEventListener('keydown', onCurveKeyDown);
      canvas.addEventListener('selectstart', function (event) { event.preventDefault(); });
    }

    // 设置 / 诊断
    const applyLevelButton = byId('apply-log-level');
    if (applyLevelButton) applyLevelButton.addEventListener('click', applyLogLevel);
    const refreshLogsButton = byId('refresh-logs');
    if (refreshLogsButton) refreshLogsButton.addEventListener('click', function () { refreshLogs(false); });
    const refreshDiagButton = byId('refresh-diag');
    if (refreshDiagButton) refreshDiagButton.addEventListener('click', function () { refreshDiagnostics(true); });
    const refreshDiag2Button = byId('refresh-diag2');
    if (refreshDiag2Button) refreshDiag2Button.addEventListener('click', function () { refreshDiagnostics(true); });
    const exportButton = byId('export-config');
    if (exportButton) exportButton.addEventListener('click', exportConfig);
    const importInput = byId('import-file');
    if (importInput) importInput.addEventListener('change', function () { importConfigFile(importInput.files && importInput.files[0], importInput); });

    const logsPre = byId('logs');
    if (logsPre) {
      logsPre.addEventListener('scroll', function () {
        state.logsPinned = logsPre.scrollHeight - logsPre.scrollTop - logsPre.clientHeight < 40;
      });
    }
    const ovRefreshLogs = byId('ov-refresh-logs');
    if (ovRefreshLogs) ovRefreshLogs.addEventListener('click', function () { refreshLogs(false); });

    const cover = byId('ov-cover');
    if (cover) {
      cover.addEventListener('error', function () {
        cover.classList.add('hidden');
      });
    }

    document.addEventListener('visibilitychange', function () {
      if (document.hidden) {
        stopPolling();
      } else {
        startPolling();
        tick();
      }
    });

    window.addEventListener('hashchange', applyRoute);
    window.addEventListener('resize', function () {
      if (state.page === 'calibration') {
        window.cancelAnimationFrame(state.canvasFrame);
        state.canvasFrame = window.requestAnimationFrame(drawCurve);
      }
    });
  }

  function renderCalibrationBadgesOnly(draft) {
    const label = CURVE_LABELS[draft.kind] || draft.kind;
    setText('cal-status-badge', label + (draft.muted ? ' · 已静音' : ' · ' + Math.round(clamp(draft.global, 0, 100)) + '%'));
  }

  // ---------------------------------------------------------------------------
  // Boot
  // ---------------------------------------------------------------------------
  function boot() {
    buildProtocolCards();
    bindStaticEvents();
    if (!location.hash || !/^#\/?(overview|protocols|output|calibration|settings)$/.test(location.hash)) {
      history.replaceState(null, '', '#/overview');
    }
    applyRoute();
    startPolling();
    tick();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})();
