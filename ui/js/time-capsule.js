import { invoke } from './event-stream.js';

export class TimeCapsuleUI {
  constructor(container) {
    this.container = container;
    this.folders = [];
    this.render();
  }

  async refresh() {
    try {
      this.folders = await invoke('time_capsule_list') || [];
      this.render();
    } catch (e) {
      console.error('Failed to load time capsules', e);
    }
  }

  async selectFolder() {
    try {
      let selected = null;
      if (typeof window.__TAURI__ !== 'undefined') {
        // First try the native folder selection command
        try {
          selected = await invoke('select_folder');
        } catch (_) {
          if (window.__TAURI__.dialog?.open) {
            selected = await window.__TAURI__.dialog.open({
              directory: true,
              multiple: false,
              title: 'Select folder to protect'
            });
          }
        }
      }

      if (selected) {
        await this.protect(selected);
      }
    } catch (e) {
      console.error('Folder selection failed:', e);
      this.showToast(`Folder selection failed: ${e}`, true);
    }
  }

  async protect(path) {
    if (!path) return;
    const parts = path.split(/[\\/]/).filter(Boolean);
    const name = parts[parts.length - 1] || path;
    try {
      await invoke('time_capsule_protect', { path, name });
      await this.refresh();
      this.showToast(`✓ "${name}" is now protected`);
    } catch (e) {
      this.showToast(`Protection failed: ${e}`, true);
    }
  }

  async snapshotNow(path) {
    try {
      await invoke('time_capsule_snapshot', { folderPath: path });
      this.showToast('Snapshot created successfully');
      await this.refresh();
    } catch (e) {
      this.showToast(`Snapshot failed: ${e}`, true);
    }
  }

  render() {
    if (!this.container) return;

    if (!this.folders || this.folders.length === 0) {
      this.container.innerHTML = `
        <div class="capsule-empty">
          <div class="capsule-icon">🛡️</div>
          <h3>No Protected Folders</h3>
          <p>Select a folder to start automatic background snapshots and restore insurance.</p>
          <button class="btn-magnetic" id="btn-capsule-protect-init">
            + Protect Folder
          </button>
        </div>
      `;
      const btn = this.container.querySelector('#btn-capsule-protect-init');
      if (btn) btn.addEventListener('click', () => this.selectFolder());
      return;
    }

    this.container.innerHTML = `
      <div class="capsule-grid">
        ${this.folders.map((f, idx) => {
          const statusClass = typeof f.status === 'string' ? f.status.toLowerCase() : 'active';
          const sizeMb = ((f.total_bytes || 0) / 1024 / 1024).toFixed(1);
          const lastTime = f.last_snapshot ? new Date(f.last_snapshot / 1000).toLocaleTimeString() : 'Never';
          return `
            <div class="capsule-card holo-card">
              <div class="capsule-header">
                <span class="capsule-status ${statusClass}"></span>
                <h4>${escapeHtml(f.name)}</h4>
              </div>
              <div class="capsule-path" title="${escapeHtml(f.path)}">${escapeHtml(f.path)}</div>
              <div class="capsule-stats">
                <span><strong>${f.file_count || 0}</strong> files</span>
                <span><strong>${sizeMb}</strong> MB</span>
              </div>
              <div class="capsule-last">
                Last snapshot: <strong>${lastTime}</strong>
              </div>
              <button class="btn-magnetic btn-capsule-snap" data-path="${escapeHtml(f.path)}">
                Snapshot Now
              </button>
            </div>
          `;
        }).join('')}
      </div>
      <div style="margin-top: 16px; display: flex; gap: 12px; align-items: center;">
        <button class="btn-magnetic" id="btn-capsule-protect-another">
          + Protect Another Folder
        </button>
        <button class="btn-magnetic" id="btn-capsule-refresh" style="opacity: 0.8;">
          ↻ Refresh
        </button>
      </div>
    `;

    // Wire action listeners without inline global handlers
    this.container.querySelectorAll('.btn-capsule-snap').forEach(btn => {
      btn.addEventListener('click', (e) => {
        const path = e.currentTarget.getAttribute('data-path');
        if (path) this.snapshotNow(path);
      });
    });

    const addBtn = this.container.querySelector('#btn-capsule-protect-another');
    if (addBtn) addBtn.addEventListener('click', () => this.selectFolder());

    const refreshBtn = this.container.querySelector('#btn-capsule-refresh');
    if (refreshBtn) refreshBtn.addEventListener('click', () => this.refresh());
  }

  showToast(msg, isError = false) {
    const toast = document.createElement('div');
    toast.className = `toast ${isError ? 'error' : 'success'}`;
    toast.textContent = msg;
    document.body.appendChild(toast);
    setTimeout(() => {
      toast.style.opacity = '0';
      toast.style.transform = 'translateY(10px)';
      toast.style.transition = 'opacity 0.3s ease, transform 0.3s ease';
      setTimeout(() => toast.remove(), 300);
    }, 3000);
  }
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}
