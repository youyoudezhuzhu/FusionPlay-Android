'use strict';
/* Receive page: receiver identity (name / interface once) + protocol switches. */
window.FP = window.FP || {};

(function (FP) {
  const u = FP.util;

  FP.createReceivePage = function (ctx) {
    const store = ctx.store;
    const els = {};
    const refs = new Map();
    let bound = false;
    let rowsBuilt = false;
    let interfaceSig = '';

    function el(id) {
      if (!els[id]) els[id] = u.byId(id);
      return els[id];
    }

    function buildRows() {
      if (rowsBuilt) return;
      rowsBuilt = true;
      const list = el('receive-protocols');
      u.clearNode(list);
      FP.PROTOCOLS.forEach(function (meta) {
        const item = u.create('li');
        item.dataset.protocol = meta.key;
        const iconWrap = u.create('span', 'device-icon');
        iconWrap.appendChild(u.svgIcon(meta.icon));
        const main = u.create('div');
        main.style.minWidth = '0';
        const titleLine = u.create('div', 'protocol-title-line');
        titleLine.appendChild(u.create('h3', null, meta.label));
        const statePill = u.create('span', 'pill neutral', '未上报');
        titleLine.appendChild(statePill);
        const pending = u.create('span', 'tag', '待应用');
        pending.hidden = true;
        titleLine.appendChild(pending);
        main.appendChild(titleLine);
        main.appendChild(u.create('p', 'protocol-desc', meta.desc));

        // MiPlay exposes a sender-visible device identity (车机/电视/平板/音响).
        // It mirrors the Android app and changes the icon the sender shows.
        let identityButtons = null;
        if (meta.key === 'miplay') {
          const identity = u.create('div', 'identity-row');
          identity.appendChild(u.create('span', 'sub-label', '设备类型'));
          const grid = u.create('div', 'identity-grid');
          grid.setAttribute('role', 'radiogroup');
          grid.setAttribute('aria-label', '小米妙播设备类型');
          identityButtons = FP.MIPLAY_DEVICE_TYPES.map(function (item) {
            const button = u.create('button', 'identity-option');
            button.type = 'button';
            button.dataset.type = item.value;
            button.setAttribute('role', 'radio');
            button.setAttribute('aria-checked', 'false');
            button.setAttribute('aria-label', '以' + item.label + '身份广播');
            button.appendChild(u.svgIcon(item.icon, 'identity-icon'));
            button.appendChild(u.create('span', null, item.label));
            grid.appendChild(button);
            return button;
          });
          identity.appendChild(grid);
          main.appendChild(identity);
          main.appendChild(u.create('p', 'hint',
            '切换后保存并应用，接收器会以新的设备身份重新广播（发送端图标随之变化）。'));
        }

        const toggle = u.create('button', 'switch');
        toggle.type = 'button';
        toggle.setAttribute('role', 'switch');
        toggle.setAttribute('aria-checked', 'false');
        toggle.setAttribute('aria-label', '启用 ' + meta.label);
        toggle.appendChild(u.create('span'));

        const details = document.createElement('details');
        details.className = 'protocol-details';
        const summary = document.createElement('summary');
        summary.textContent = '端口 / 客户端 / 详情';
        details.appendChild(summary);
        const kv = document.createElement('dl');
        kv.className = 'kv compact';
        details.appendChild(kv);

        item.appendChild(iconWrap);
        item.appendChild(main);
        item.appendChild(toggle);
        item.appendChild(details);
        list.appendChild(item);
        refs.set(meta.key, {
          item: item, statePill: statePill, pending: pending, toggle: toggle, kv: kv,
          iconWrap: iconWrap, identityButtons: identityButtons
        });
      });
    }

    function renderInterfaceOptions() {
      const draft = store.getDraft();
      const diagnostics = store.state.diagnostics;
      const interfaces = (diagnostics && Array.isArray(diagnostics.interfaces)) ? diagnostics.interfaces : [];
      const current = draft && draft.receiver ? String(draft.receiver.interface || '') : '';
      const options = [''].concat(interfaces);
      if (current && options.indexOf(current) < 0) options.push(current);
      const signature = JSON.stringify(options);
      const select = el('receiver-interface');
      if (signature !== interfaceSig) {
        interfaceSig = signature;
        u.clearNode(select);
        options.forEach(function (value) {
          const option = document.createElement('option');
          option.value = value;
          option.textContent = value === '' ? '自动选择' : value;
          select.appendChild(option);
        });
      }
      u.setSelect(select, current);
    }

    function renderInputs() {
      const draft = store.getDraft();
      if (!draft || !draft.receiver) return;
      u.setInput(el('receiver-name'), draft.receiver.name || '');
      renderInterfaceOptions();
      const error = el('receiver-name-error');
      u.show(error, !String(draft.receiver.name || '').trim());
    }

    function renderProtocols() {
      const status = store.state.status;
      const protocols = (status && status.protocols) || {};
      const draft = store.getDraft();
      const draftProtocols = (draft && draft.protocols) || {};
      let enabledCount = 0;
      let errorCount = 0;
      let pendingCount = 0;

      FP.PROTOCOLS.forEach(function (meta) {
        const ref = refs.get(meta.key);
        if (!ref) return;
        const runtime = protocols[meta.key] || null;
        const draftEnabled = !!(draftProtocols[meta.key] && draftProtocols[meta.key].enabled);
        if (draftEnabled) enabledCount += 1;

        let text = '未上报';
        let tone = 'neutral';
        if (runtime) {
          if (!runtime.enabled) {
            text = '已停用';
            tone = 'neutral';
          } else if (runtime.state === 'error' || runtime.state === 'failed') {
            text = '异常';
            tone = 'danger';
            errorCount += 1;
          } else if (runtime.state) {
            text = u.stateLabel(runtime.state);
            tone = u.stateTone(runtime.state);
          }
        }
        u.setText(ref.statePill, text);
        ref.statePill.className = 'pill ' + tone;

        const pending = !!runtime && draftEnabled !== !!runtime.enabled;
        ref.pending.hidden = !pending;
        if (pending) pendingCount += 1;

        if (meta.key === 'miplay') {
          const selected = u.miplayDeviceType(draftProtocols.miplay && draftProtocols.miplay.device_type);
          if (ref.identityButtons) {
            ref.identityButtons.forEach(function (button) {
              const on = button.dataset.type === selected.value;
              button.classList.toggle('active', on);
              button.setAttribute('aria-checked', on ? 'true' : 'false');
            });
          }
          if (ref.iconWrap) {
            u.clearNode(ref.iconWrap);
            ref.iconWrap.appendChild(u.svgIcon(selected.icon));
          }
        }
        u.setSwitch(ref.toggle, draftEnabled);
        ref.toggle.setAttribute('aria-label', (draftEnabled ? '停用 ' : '启用 ') + meta.label);
        const rows = [
          ['端口', runtime && runtime.port ? String(runtime.port) : '—'],
          ['客户端', runtime ? String(runtime.clients || 0) : '—'],
          ['详情', runtime && runtime.detail ? String(runtime.detail) : '—']
        ];
        u.clearNode(ref.kv);
        rows.forEach(function (row) {
          const item = u.create('div');
          item.appendChild(u.create('dt', null, row[0]));
          item.appendChild(u.create('dd', null, row[1]));
          ref.kv.appendChild(item);
        });
      });

      const summary = el('receive-summary');
      let text = '加载中';
      let tone = 'neutral';
      if (status) {
        text = enabledCount + '/3 已启用';
        tone = enabledCount ? 'ok' : 'neutral';
        if (errorCount) {
          text = errorCount + ' 个异常';
          tone = 'danger';
        }
        if (pendingCount) {
          text += ' · ' + pendingCount + ' 项待应用';
          if (!errorCount) tone = 'warn';
        }
      }
      u.setText(summary, text);
      summary.className = 'pill ' + tone;
    }

    function render() {
      renderInputs();
      renderProtocols();
    }

    function refreshDiagnostics() {
      return FP.api.diagnostics().then(function (diagnostics) {
        store.setDiagnostics(diagnostics);
        renderInterfaceOptions();
      }).catch(function () {
        /* Interface list is optional; keep auto option available. */
      });
    }

    function bind() {
      if (bound) return;
      bound = true;
      buildRows();

      const nameInput = el('receiver-name');
      nameInput.addEventListener('input', function () {
        store.setReceiverField('name', nameInput.value);
        u.show(el('receiver-name-error'), !nameInput.value.trim());
      });
      nameInput.addEventListener('blur', function () {
        u.show(el('receiver-name-error'), !nameInput.value.trim());
      });

      el('receiver-interface').addEventListener('change', function () {
        store.setReceiverField('interface', el('receiver-interface').value);
      });

      el('receive-protocols').addEventListener('click', function (event) {
        const identityOption = event.target.closest('.identity-option');
        if (identityOption) {
          const identityItem = identityOption.closest('[data-protocol]');
          if (identityItem && identityItem.dataset.protocol === 'miplay') {
            store.setMiplayDeviceType(identityOption.dataset.type);
            renderProtocols();
          }
          return;
        }
        const toggle = event.target.closest('.switch');
        if (!toggle) return;
        const item = toggle.closest('[data-protocol]');
        if (!item) return;
        const key = item.dataset.protocol;
        const draft = store.getDraft();
        const current = !!(draft && draft.protocols && draft.protocols[key] && draft.protocols[key].enabled);
        store.setProtocolEnabled(key, !current);
        renderProtocols();
      });
    }

    return {
      enter: function () {
        bind();
        render();
        refreshDiagnostics();
      },
      leave: function () {},
      render: render
    };
  };
})(window.FP);
