'use strict';
/*
 * Central draft store.
 *
 * One `draftConfig` (a clone of the last server config, with user edits applied)
 * plus a set of dirty leaf keys. Every poll merges fresh server data into the
 * draft but keeps dirty fields, so polling never overwrites an edit. Saves send
 * the complete draft back to PUT /api/config.
 */
window.FP = window.FP || {};

(function (FP) {
  const util = FP.util;
  const DEVICE_PREFIX = 'audio.device|';
  const ADD_PREFIX = 'audio.add|';
  const REMOVE_PREFIX = 'audio.remove|';
  const CURVE_KINDS = ['linear', 'soft', 'balanced', 'custom'];
  const DEVICE_FIELDS = ['enabled', 'gain_db', 'delay_ms', 'sample_rate', 'curve'];

  function parseDeviceKey(key) {
    if (key.indexOf(DEVICE_PREFIX) !== 0) return null;
    const rest = key.slice(DEVICE_PREFIX.length);
    const split = rest.indexOf('|');
    if (split < 0) return null;
    return { id: rest.slice(0, split), field: rest.slice(split + 1) };
  }

  function normalizePoints(points) {
    if (!Array.isArray(points)) return [];
    return points
      .filter(function (point) { return Array.isArray(point) && point.length === 2; })
      .map(function (point) {
        return [
          util.round(util.clamp(util.finite(point[0], 0), 0, 1), 4),
          util.round(util.clamp(util.finite(point[1], 0), 0, 1), 4)
        ];
      })
      .sort(function (a, b) { return a[0] - b[0]; });
  }

  function normalizeCurve(curve) {
    const source = curve && typeof curve === 'object' ? curve : {};
    const kind = CURVE_KINDS.indexOf(source.kind) >= 0 ? source.kind : 'balanced';
    return { kind: kind, points: normalizePoints(source.points) };
  }

  function validateCurve(curve, label) {
    if (!curve || typeof curve !== 'object') {
      return label + '曲线格式不合法';
    }
    if (CURVE_KINDS.indexOf(curve.kind) < 0) {
      return label + '曲线类型不合法';
    }
    if (curve.kind !== 'custom') return null;
    const points = Array.isArray(curve.points) ? curve.points : [];
    let lastX = 0;
    let lastY = 0;
    for (let i = 0; i < points.length; i += 1) {
      const point = points[i];
      if (!Array.isArray(point) || point.length !== 2 ||
          !Number.isFinite(Number(point[0])) || !Number.isFinite(Number(point[1]))) {
        return label + '曲线控制点必须是数值';
      }
      const x = Number(point[0]);
      const y = Number(point[1]);
      if (x < 0 || x > 1 || y < 0 || y > 1) {
        return label + '曲线控制点必须在 0%–100% 之间';
      }
      if (x < lastX || y < lastY) {
        return label + '曲线控制点必须单调不减';
      }
      lastX = x;
      lastY = y;
    }
    return null;
  }

  FP.createStore = function () {
    const state = {
      health: null,
      status: null,
      baseline: null,
      draft: null,
      dirty: new Set(),
      saving: false,
      discovered: [],
      diagnostics: null,
      discoveryError: ''
    };

    const listeners = new Set();

    function emit(reason) {
      listeners.forEach(function (listener) {
        try { listener(reason || 'change'); } catch (error) { /* listeners must not break the store */ }
      });
    }

    function subscribe(listener) {
      listeners.add(listener);
      return function unsubscribe() { listeners.delete(listener); };
    }

    function clone(value) { return util.clone(value); }

    function getDraftDevice(id) {
      if (!state.draft || !state.draft.audio || !Array.isArray(state.draft.audio.devices)) return null;
      return state.draft.audio.devices.find(function (device) { return device.id === id; }) || null;
    }

    function getBaselineDevice(id) {
      if (!state.baseline || !state.baseline.audio || !Array.isArray(state.baseline.audio.devices)) {
        return null;
      }
      return state.baseline.audio.devices.find(function (device) { return device.id === id; }) || null;
    }

    function mergeDraft(base, oldDraft, dirty) {
      const next = clone(base);
      if (!next.audio) next.audio = { devices: [] };
      if (!Array.isArray(next.audio.devices)) next.audio.devices = [];
      if (!oldDraft) return next;
      const oldDevices = new Map();
      ((oldDraft.audio && oldDraft.audio.devices) || []).forEach(function (device) {
        oldDevices.set(device.id, device);
      });

      // 1) Plain fields (receiver / protocols / global audio).
      dirty.forEach(function (key) {
        if (key.indexOf(DEVICE_PREFIX) === 0 ||
            key.indexOf(ADD_PREFIX) === 0 ||
            key.indexOf(REMOVE_PREFIX) === 0) {
          return;
        }
        const path = key.split('.');
        if (path.length < 2) return;
        const value = util.getPath(oldDraft, path);
        if (value !== undefined) util.setPath(next, path, clone(value));
      });

      // 2) Removed devices.
      dirty.forEach(function (key) {
        if (key.indexOf(REMOVE_PREFIX) !== 0) return;
        const id = key.slice(REMOVE_PREFIX.length);
        next.audio.devices = next.audio.devices.filter(function (device) { return device.id !== id; });
      });

      // 3) Added devices.
      dirty.forEach(function (key) {
        if (key.indexOf(ADD_PREFIX) !== 0) return;
        const id = key.slice(ADD_PREFIX.length);
        if (next.audio.devices.some(function (device) { return device.id === id; })) return;
        const device = oldDevices.get(id);
        if (device) next.audio.devices.push(clone(device));
      });

      // 4) Per-device fields.
      dirty.forEach(function (key) {
        const parsed = parseDeviceKey(key);
        if (!parsed) return;
        let device = next.audio.devices.find(function (item) { return item.id === parsed.id; });
        const oldDevice = oldDevices.get(parsed.id);
        if (!device && oldDevice) {
          device = clone(oldDevice);
          next.audio.devices.push(device);
          dirty.add(ADD_PREFIX + parsed.id);
        }
        if (!device) return;
        if (oldDevice && Object.prototype.hasOwnProperty.call(oldDevice, parsed.field)) {
          device[parsed.field] = clone(oldDevice[parsed.field]);
        }
      });

      return next;
    }

    function pruneDirty() {
      if (!state.baseline || !state.draft) return;
      state.dirty.forEach(function (key) {
        if (key.indexOf(ADD_PREFIX) === 0) {
          const id = key.slice(ADD_PREFIX.length);
          if (getBaselineDevice(id)) state.dirty.delete(key);
          return;
        }
        if (key.indexOf(REMOVE_PREFIX) === 0) {
          const id = key.slice(REMOVE_PREFIX.length);
          if (!getBaselineDevice(id)) state.dirty.delete(key);
          return;
        }
        const parsed = parseDeviceKey(key);
        if (parsed) {
          const baseDevice = getBaselineDevice(parsed.id);
          const draftDevice = getDraftDevice(parsed.id);
          if (baseDevice && draftDevice && util.deepEqual(baseDevice[parsed.field], draftDevice[parsed.field])) {
            state.dirty.delete(key);
          }
          return;
        }
        const path = key.split('.');
        if (util.deepEqual(util.getPath(state.baseline, path), util.getPath(state.draft, path))) {
          state.dirty.delete(key);
        }
      });
    }

    function applyRemote(payload) {
      if (payload && payload.health) state.health = payload.health;
      if (payload && payload.status) state.status = payload.status;
      if (payload && payload.config && typeof payload.config === 'object') {
        const config = payload.config;
        if (!state.baseline) {
          state.baseline = clone(config);
          state.draft = clone(config);
        } else if (!util.deepEqual(state.baseline, config)) {
          state.baseline = clone(config);
          state.draft = state.dirty.size ? mergeDraft(state.baseline, state.draft, state.dirty) : clone(state.baseline);
          pruneDirty();
        }
      }
      emit('remote');
    }

    function adoptConfig(config, options) {
      state.baseline = clone(config);
      state.draft = clone(config);
      if (!options || options.clearDirty !== false) state.dirty.clear();
      emit('baseline');
    }

    function markPlain(key, value) {
      if (!state.draft) return;
      const path = key.split('.');
      util.setPath(state.draft, path, value);
      const baseValue = state.baseline ? util.getPath(state.baseline, path) : undefined;
      if (util.deepEqual(baseValue, value)) state.dirty.delete(key);
      else state.dirty.add(key);
      emit('dirty');
    }

    function setReceiverField(field, value) {
      markPlain('receiver.' + field, value);
    }

    function setProtocolEnabled(key, enabled) {
      markPlain('protocols.' + key + '.enabled', !!enabled);
    }

    function setMiplayDeviceType(value) {
      markPlain('protocols.miplay.device_type', String(value || 'television'));
    }

    function setGlobal(field, value) {
      markPlain('audio.' + field, value);
    }

    function setDeviceField(id, field, value) {
      if (DEVICE_FIELDS.indexOf(field) < 0) return;
      const device = getDraftDevice(id);
      if (!device) return;
      device[field] = value;
      const key = DEVICE_PREFIX + id + '|' + field;
      const baseDevice = getBaselineDevice(id);
      if (baseDevice && util.deepEqual(baseDevice[field], value)) state.dirty.delete(key);
      else state.dirty.add(key);
      emit('dirty');
    }

    function addDevice(device) {
      if (!state.draft || !device || !device.id) return;
      if (getDraftDevice(device.id)) return;
      const entry = {
        id: device.id,
        name: device.name || device.id,
        enabled: device.enabled !== false,
        gain_db: util.finite(device.gain_db, 0),
        delay_ms: util.finite(device.delay_ms, 0),
        sample_rate: Math.max(0, Math.round(util.finite(device.sample_rate, 0))),
        curve: normalizeCurve(device.curve)
      };
      state.draft.audio.devices.push(entry);
      state.dirty.add(ADD_PREFIX + entry.id);
      emit('dirty');
    }

    function removeDevice(id) {
      if (!state.draft) return;
      const existedInBaseline = !!getBaselineDevice(id);
      state.draft.audio.devices = state.draft.audio.devices.filter(function (device) {
        return device.id !== id;
      });
      state.dirty.forEach(function (key) {
        if (key.indexOf(DEVICE_PREFIX + id + '|') === 0) state.dirty.delete(key);
      });
      if (state.dirty.has(ADD_PREFIX + id)) {
        state.dirty.delete(ADD_PREFIX + id);
      } else if (existedInBaseline) {
        state.dirty.add(REMOVE_PREFIX + id);
      } else {
        state.dirty.delete(REMOVE_PREFIX + id);
      }
      emit('dirty');
    }

    function hasDirty() { return state.dirty.size > 0; }

    function dirtyScopes() {
      const scopes = { audio: false, receive: false };
      state.dirty.forEach(function (key) {
        if (key.indexOf('receiver.') === 0 || key.indexOf('protocols.') === 0) scopes.receive = true;
        else scopes.audio = true;
      });
      return scopes;
    }

    function revert() {
      if (!state.baseline) return;
      state.draft = clone(state.baseline);
      state.dirty.clear();
      emit('revert');
    }

    function validate() {
      if (!state.baseline || !state.draft) {
        return { ok: false, message: '配置尚未加载完成，请稍后重试。' };
      }
      const name = String(state.draft.receiver && state.draft.receiver.name || '').trim();
      if (!name) return { ok: false, message: '接收器名称不能为空。' };
      const audio = state.draft.audio || {};
      const devices = Array.isArray(audio.devices) ? audio.devices : [];
      for (let i = 0; i < devices.length; i += 1) {
        const device = devices[i];
        const label = '设备「' + (device.name || device.id) + '」';
        const gain = Number(device.gain_db);
        if (!Number.isFinite(gain) || gain < -30 || gain > 0) {
          return { ok: false, message: label + '的响度平衡必须在 -30–0 dB。' };
        }
        const delay = Number(device.delay_ms);
        if (!Number.isFinite(delay) || delay < 0 || delay > 2000) {
          return { ok: false, message: label + '的同步延迟必须在 0–2000 ms。' };
        }
        const curveError = validateCurve(device.curve, label);
        if (curveError) return { ok: false, message: curveError + '。' };
      }
      return { ok: true, message: '' };
    }

    function buildSavePayload() {
      if (!state.draft) return null;
      const payload = clone(state.draft);
      payload.receiver = payload.receiver || {};
      payload.receiver.name = String(payload.receiver.name || '').trim();
      payload.receiver.interface = payload.receiver.interface || '';
      payload.config_version = payload.config_version || 2;
      payload.audio = payload.audio || {};
      payload.audio.global_attenuation_percent =
        util.round(util.clamp(util.finite(payload.audio.global_attenuation_percent, 100), 0, 100), 1);
      payload.audio.muted = !!payload.audio.muted;
      payload.audio.devices = (Array.isArray(payload.audio.devices) ? payload.audio.devices : [])
        .map(function (device) {
          return {
            id: device.id,
            name: device.name || '',
            enabled: !!device.enabled,
            gain_db: util.round(util.clamp(util.finite(device.gain_db, 0), -30, 0), 1),
            delay_ms: util.round(util.clamp(util.finite(device.delay_ms, 0), 0, 2000), 1),
            sample_rate: Math.max(0, Math.round(util.finite(device.sample_rate, 0))),
            curve: normalizeCurve(device.curve)
          };
        });
      return payload;
    }

    return {
      state: state,
      subscribe: subscribe,
      applyRemote: applyRemote,
      adoptConfig: adoptConfig,
      setReceiverField: setReceiverField,
      setProtocolEnabled: setProtocolEnabled,
      setMiplayDeviceType: setMiplayDeviceType,
      setGlobal: setGlobal,
      setDeviceField: setDeviceField,
      addDevice: addDevice,
      removeDevice: removeDevice,
      getDraft: function () { return state.draft; },
      getBaseline: function () { return state.baseline; },
      getDevice: getDraftDevice,
      getBaselineDevice: getBaselineDevice,
      getDirtyKeys: function () { return Array.from(state.dirty); },
      hasDirty: hasDirty,
      dirtyScopes: dirtyScopes,
      revert: revert,
      validate: validate,
      buildSavePayload: buildSavePayload,
      setSaving: function (saving) { state.saving = !!saving; emit('saving'); },
      isSaving: function () { return state.saving; },
      setDiscovered: function (devices, error) {
        state.discovered = Array.isArray(devices) ? devices : [];
        state.discoveryError = error || '';
        emit('discovery');
      },
      setDiagnostics: function (diagnostics) { state.diagnostics = diagnostics; emit('diagnostics'); },
      notify: function (reason) { emit(reason); },
      getDeviceStatus: function (id) {
        const status = state.status;
        const devices = status && Array.isArray(status.devices) ? status.devices : [];
        return devices.find(function (device) { return device.id === id; }) || null;
      },
      getDiscoveredDevice: function (id) {
        return state.discovered.find(function (device) { return device.id === id; }) || null;
      },
      normPoints: normalizePoints,
      normalizeCurve: normalizeCurve
    };
  };
})(window.FP);
