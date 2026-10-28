'use strict';
// FusionPlay fnOS management UI. All management goes through the Rust host's
// JSON API; the UI never fakes device/protocol state.
const base = location.pathname.replace(/\/+$/, '').replace(/\/index\.html$/, '');
const api = (path) => `${base}/${path}`.replace(/\/{2,}/g, '/');
const $ = (id) => document.getElementById(id);
const state = { config: null, devices: [], status: null };

const PROTOCOL_LABEL = { airplay: 'AirPlay 2', dlna: 'DLNA', miplay: '小米妙播' };
const SOURCE_LABEL = { airplay: 'AirPlay 2', dlna: 'DLNA', miplay: '小米妙播' };

function dot(el, ok, warn) {
  if (!el) return;
  el.className = 'dot ' + (ok ? 'ok' : warn ? 'warn' : 'bad');
}
function fmtSecs(secs) {
  secs = Math.max(0, Math.floor(secs || 0));
  const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), s = secs % 60;
  return (h ? h + 'h ' : '') + (m ? m + 'm ' : '') + s + 's';
}
function fmtDb(value) { return value == null ? '—' : (Math.round(value * 10) / 10) + ' dB'; }
function el(tag, cls, text) { const node = document.createElement(tag); if (cls) node.className = cls; if (text != null) node.textContent = text; return node; }

async function request(path, options) {
  const response = await fetch(api(path), { cache: 'no-store', ...options });
  const text = await response.text();
  let body = null;
  try { body = text ? JSON.parse(text) : null; } catch (e) { body = { raw: text }; }
  if (!response.ok) throw new Error((body && (body.error || body.raw)) || `HTTP ${response.status}`);
  return body;
}

function renderStatus() {
  const s = state.status;
  if (!s) return;
  dot($('health'), true);
  $('health-text').textContent = '运行中';
  $('version').textContent = 'v' + s.version;
  $('ov-uptime').textContent = fmtSecs(s.uptime);
  $('ov-frames').textContent = (s.audio && s.audio.master_frames ? s.audio.master_frames.toLocaleString() : '0') + ' 帧';
  const source = s.current_source ? (SOURCE_LABEL[s.current_source] || s.current_source) : null;
  $('ov-source').textContent = source || '空闲（等待投放）';
  $('source-badge').textContent = source || '空闲';
  $('source-badge').className = 'badge ' + (source ? 'ok' : '');
  const media = s.media || {};
  $('ov-title').textContent = media.title ? `${media.title}${media.artist ? ' · ' + media.artist : ''}` : '—';
  const sv = s.sender_volume || {};
  const vol = [];
  if (sv.airplay_db != null) vol.push(`AirPlay ${fmtDb(sv.airplay_db)}`);
  if (sv.dlna_percent != null) vol.push(`DLNA ${sv.dlna_percent}%`);
  if (sv.miplay_percent != null) vol.push(`妙播 ${sv.miplay_percent}%`);
  $('ov-volume').textContent = vol.length ? vol.join(' / ') : '—';
  const enabled = (s.devices || []).filter((d) => d.enabled);
  $('ov-devices').textContent = enabled.length ? enabled.map((d) => d.name || d.id).join('、') : '未选择输出设备';
  $('errors').classList.toggle('hidden', !(s.errors && s.errors.length));
  $('errors').textContent = (s.errors || []).slice(-3).join('\n');

  const protocols = $('protocols');
  protocols.innerHTML = '';
  const proto = s.protocols || {};
  let enabledCount = 0;
  for (const key of ['airplay', 'dlna', 'miplay']) {
    const info = proto[key] || {};
    const switchInfo = state.config ? state.config.protocols[key] : { enabled: info.enabled };
    if (switchInfo.enabled) enabledCount += 1;
    const tr = document.createElement('tr');
    const tdName = el('td', null, PROTOCOL_LABEL[key]);
    const tdSwitch = el('td');
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.checked = !!switchInfo.enabled;
    checkbox.addEventListener('change', async () => {
      try {
        const config = JSON.parse(JSON.stringify(state.config));
        config.protocols[key].enabled = checkbox.checked;
        state.config = (await request('api/config', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(config) })).config;
        toast(`已${checkbox.checked ? '开启' : '关闭'} ${PROTOCOL_LABEL[key]}（协议宿主按新配置重启后生效）`);
        refresh();
      } catch (error) { toast('保存失败：' + error.message); checkbox.checked = !checkbox.checked; }
    });
    tdSwitch.appendChild(checkbox);
    const stateText = info.state || (switchInfo.enabled ? 'starting' : 'disabled');
    const badge = el('span', 'badge ' + (stateText === 'running' || stateText === 'listening' ? 'ok' : stateText === 'disabled' ? '' : 'warn'), stateText);
    tr.append(tdName, tdSwitch, el('td', null, ''), el('td', 'muted', info.detail || ''));
    tr.children[2].appendChild(badge);
    protocols.appendChild(tr);
  }
  $('protos-badge').textContent = `${enabledCount}/3 已启用`;

  // devices table
  const rows = $('devices');
  rows.innerHTML = '';
  const configDevices = state.config ? state.config.audio.devices : [];
  const merged = [];
  for (const info of state.devices) {
    const setting = configDevices.find((d) => d.id === info.id) || { id: info.id, name: info.name, enabled: false, gain_db: 0, delay_ms: 0, sample_rate: 0 };
    merged.push({ info, setting });
  }
  for (const setting of configDevices) {
    if (!state.devices.some((info) => info.id === setting.id)) {
      merged.push({ info: { id: setting.id, name: setting.name || setting.id, online: false, sample_rates: [], channels: 0 }, setting });
    }
  }
  if (!merged.length) { rows.innerHTML = '<tr><td colspan="10" class="muted">未发现 ALSA 输出设备</td></tr>'; return; }
  const live = new Map(((state.status && state.status.devices) || []).map((d) => [d.id, d]));
  for (const { info, setting } of merged) {
    const tr = document.createElement('tr');
    const enable = document.createElement('input'); enable.type = 'checkbox'; enable.checked = !!setting.enabled;
    enable.addEventListener('change', () => { setting.enabled = enable.checked; });
    const gain = document.createElement('input'); gain.type = 'range'; gain.min = '-30'; gain.max = '0'; gain.step = '0.5'; gain.value = String(setting.gain_db); gain.dataset.id = setting.id;
    const gainLabel = el('b', null, setting.gain_db + 'dB');
    gain.addEventListener('input', () => { gainLabel.textContent = gain.value + 'dB'; setting.gain_db = parseFloat(gain.value); });
    const gainWrap = el('div'); gainWrap.append(gain, gainLabel);
    const delay = document.createElement('input'); delay.type = 'number'; delay.min = '0'; delay.max = '2000'; delay.step = '1'; delay.value = String(setting.delay_ms); delay.style.width = '64px';
    delay.addEventListener('change', () => { setting.delay_ms = Math.max(0, Math.min(2000, parseFloat(delay.value) || 0)); delay.value = String(setting.delay_ms); });
    const test = el('button', 'btn small', '测试音');
    test.addEventListener('click', async () => {
      try { await request('api/devices/test', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id: setting.id }) }); toast('已播放 1 秒测试音'); }
      catch (error) { toast('测试失败：' + error.message); }
    });
    const status = live.get(setting.id) || {};
    const rates = (info.sample_rates || []).join('/') || '—';
    tr.append(
      (() => { const td = el('td'); td.appendChild(enable); return td; })(),
      el('td', null, info.name || setting.name || info.id),
      el('td', 'mono muted', info.id),
      el('td', null, info.online ? '在线' : '离线'),
      el('td', 'muted', `${rates} Hz${info.channels ? ' / ' + info.channels + 'ch' : ''}`),
      el('td', 'muted', status.buffer_ms != null ? status.buffer_ms.toFixed(0) + 'ms' : '—'),
      el('td', 'muted', status.drift_ppm != null ? status.drift_ppm.toFixed(1) + 'ppm' : '—'),
      (() => { const td = el('td'); td.appendChild(gainWrap); return td; })(),
      (() => { const td = el('td'); td.appendChild(delay); return td; })(),
      (() => { const td = el('td'); td.appendChild(test); return td; })()
    );
    rows.appendChild(tr);
  }

  // calibration controls reflect config without overriding user edits
  if (state.config && document.activeElement !== $('global-gain')) {
    $('global-gain').value = String(Math.round(state.config.audio.global_attenuation_percent));
    $('gain-label').textContent = Math.round(state.config.audio.global_attenuation_percent) + '%';
  }
  if (state.config) {
    $('curve-kind').value = (state.config.audio.curve && state.config.audio.curve.kind) || 'balanced';
    $('muted').checked = !!state.config.audio.muted;
    if (document.activeElement !== $('curve-points')) {
      $('curve-points').value = ((state.config.audio.curve && state.config.audio.curve.points) || []).map((p) => `${p[0]},${p[1]}`).join(';');
    }
  }
}

function drawPreview(samples) {
  const canvas = $('preview');
  const ctx = canvas.getContext('2d');
  const w = canvas.width, h = canvas.height;
  ctx.clearRect(0, 0, w, h);
  ctx.strokeStyle = '#30363d';
  ctx.beginPath(); ctx.moveTo(40, h - 24); ctx.lineTo(w - 12, h - 24); ctx.moveTo(40, 12); ctx.lineTo(40, h - 24); ctx.stroke();
  ctx.strokeStyle = '#58a6ff'; ctx.lineWidth = 2; ctx.beginPath();
  (samples || []).forEach((point, index) => {
    const x = 40 + point.logical * (w - 52);
    const y = (h - 24) - point.output * (h - 36);
    if (index === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  });
  ctx.stroke();
  ctx.fillStyle = '#8b949e'; ctx.font = '11px sans-serif';
  ctx.fillText('0%', 20, h - 20); ctx.fillText('100%', w - 40, h - 20); ctx.fillText('输出', 6, 20);
}

let toastTimer = null;
function toast(message) {
  let node = $('toast');
  if (!node) { node = el('div'); node.id = 'toast'; node.style.cssText = 'position:fixed;left:50%;bottom:24px;transform:translateX(-50%);background:#1f6feb;color:#fff;padding:10px 18px;border-radius:10px;font-size:13px;z-index:99;max-width:80vw'; document.body.appendChild(node); }
  node.textContent = message; node.style.opacity = '1';
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { node.style.opacity = '0'; }, 3200);
}

async function refresh() {
  try {
    const [status, config, devices] = await Promise.all([request('api/status'), request('api/config'), request('api/devices')]);
    state.status = status; state.config = config; state.devices = devices.devices || [];
    renderStatus();
  } catch (error) {
    dot($('health'), false); $('health-text').textContent = '离线';
    console.error(error);
  }
}

async function refreshDiagnostics() {
  try {
    const diag = await request('api/diagnostics');
    $('diag-backend').textContent = diag.backend || '—';
    $('diag-ifaces').textContent = (diag.interfaces || []).join(', ') || '—';
    $('diag-logpath').textContent = diag.log_path || '—';
    $('diag-version').textContent = (diag.version || '') + ' / up ' + fmtSecs(diag.uptime);
    const logs = await request('api/logs');
    $('logs').textContent = (logs.lines || []).slice(-120).join('\n') || '(空)';
    $('logs').scrollTop = $('logs').scrollHeight;
    if (diag.alsa_pcm) $('diag-alsa').textContent = diag.alsa_pcm;
  } catch (error) { $('logs').textContent = '诊断加载失败：' + error.message; }
}

async function saveConfig(mutator, note) {
  try {
    const config = JSON.parse(JSON.stringify(state.config));
    mutator(config);
    const result = await request('api/config', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(config) });
    state.config = result.config;
    await request('api/devices/refresh', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
    toast(note || '已保存并应用');
    refresh();
  } catch (error) { toast('保存失败：' + error.message); }
}

function parsePoints(text) {
  return String(text || '').split(';').map((pair) => pair.split(',').map((v) => parseFloat(v.trim())))
    .filter((pair) => pair.length === 2 && pair.every((v) => Number.isFinite(v)))
    .map(([x, y]) => [Math.max(0, Math.min(1, x)), Math.max(0, Math.min(1, y))]);
}

$('global-gain').addEventListener('input', () => { $('gain-label').textContent = $('global-gain').value + '%'; });
$('save-audio').addEventListener('click', () => {
  const settings = state.devices.map((info) => {
    const configDevice = state.config.audio.devices.find((d) => d.id === info.id) || { id: info.id, name: info.name };
    return { ...configDevice, name: info.name || configDevice.name };
  });
  saveConfig((config) => {
    config.audio.devices = state.config.audio.devices.map((d) => ({ ...d }));
    for (const info of state.devices) {
      let entry = config.audio.devices.find((d) => d.id === info.id);
      if (!entry) {
        entry = { id: info.id, name: info.name, enabled: false, gain_db: 0, delay_ms: 0, sample_rate: 0 };
        config.audio.devices.push(entry);
      }
      entry.name = info.name;
    }
  }, '输出设备设置已保存');
});
$('refresh-devices').addEventListener('click', async () => {
  try { await request('api/devices/refresh', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' }); await refresh(); toast('设备列表已刷新'); }
  catch (error) { toast('刷新失败：' + error.message); }
});
$('apply-cal').addEventListener('click', async () => {
  const curve = { kind: $('curve-kind').value, points: parsePoints($('curve-points').value) };
  try {
    const preview = await request('api/calibration/preview', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ curve }) });
    drawPreview(preview.samples);
  } catch (error) { /* preview is best effort */ }
  saveConfig((config) => {
    config.audio.global_attenuation_percent = parseFloat($('global-gain').value);
    config.audio.muted = $('muted').checked;
    config.audio.curve = curve;
  }, '校准参数已应用');
});
$('reset-cal').addEventListener('click', () => {
  $('global-gain').value = '100'; $('gain-label').textContent = '100%';
  $('curve-kind').value = 'balanced'; $('curve-points').value = ''; $('muted').checked = false;
  saveConfig((config) => {
    config.audio.global_attenuation_percent = 100;
    config.audio.muted = false;
    config.audio.curve = { kind: 'balanced', points: [] };
  }, '已恢复默认校准');
});
$('curve-kind').addEventListener('change', async () => {
  try {
    const preview = await request('api/calibration/preview', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ curve: { kind: $('curve-kind').value, points: parsePoints($('curve-points').value) } }) });
    drawPreview(preview.samples);
  } catch (error) { /* ignore */ }
});
$('log-level').addEventListener('change', async () => {
  try { await request('api/logs/level', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ level: $('log-level').value }) }); toast('日志级别已切换为 ' + $('log-level').value); }
  catch (error) { toast('切换失败：' + error.message); }
});
$('load-diag').addEventListener('click', refreshDiagnostics);

refresh();
setInterval(refresh, 2000);
refreshDiagnostics();
setInterval(refreshDiagnostics, 10000);
