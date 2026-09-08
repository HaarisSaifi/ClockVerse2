import { invoke } from './event-stream.js';

export class TimeCapsuleUI {
  constructor(container) {
    this.container = container;
    this.folders = [];
    this.historyFolder = null;
    this.historySnapshots = [];
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
        try {
          selected = await invoke('select_folder');
        } catch (_) {
          if (window.__TAURI__.dialog?.open) {
            selected = await window.__TAURI__.dialog.open({
              directory: true,
              multiple: false,
              title: 'Select folder to protect with Time Capsule'
            });
          }
        }
      }

      if (selected) {
        if (Array.isArray(selected)) selected = selected[0];
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
      this.showToast(`✓ "${name}" is now continuously protected`);
    } catch (e) {
      this.showToast(`Protection failed: ${e}`, true);
    }
  }

  async snapshotNow(path) {
    try {
      await invoke('time_capsule_snapshot', { folderPath: path });
      this.showToast('✓ New byte-exact snapshot created');
      await this.refresh();
      if (this.historyFolder === path) {
        await this.viewHistory(path);
      }
    } catch (e) {
      this.showToast(`Snapshot failed: ${e}`, true);
    }
  }

  async viewHistory(path) {
    this.historyFolder = path;
    try {
      this.historySnapshots = await invoke('time_capsule_history', { folderPath: path }) || [];
      this.render();
    } catch (e) {
      this.showToast(`Could not load snapshot history: ${e}`, true);
    }
  }

  async rollback(folderPath, snapshotId) {
    if (!confirm('Are you sure you want to roll back this folder to this snapshot? All deleted or modified files will be resurrected.')) {
      return;
    }

    try {
      const restored = await invoke('time_capsule_rollback', { folderPath, snapshotId });
      this.showToast(`✓ Rollback complete! ${restored} files restored with 100% byte fidelity.`);
      await this.refresh();
    } catch (e) {
      this.showToast(`Rollback failed: ${e}`, true);
    }
  }

  render() {
    if (!this.container) return;

    if (!this.folders || this.folders.length === 0) {
      this.container.innerHTML = `
        <div class="capsule-empty">
          <div class="capsule-icon">🛡️</div>
          <h3>No Protected Folders Registered</h3>
          <p>Protect any project folder or pendrive. Continuous SHA-256 snapshots ensure you never lose work.</p>
          <button class="btn-primary" id="btn-capsule-protect-init">
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
        ${this.folders.map((f) => {
          const statusClass = typeof f.status === 'string' ? f.status.toLowerCase() : 'active';
          const sizeMb = ((f.total_bytes || 0) / 1024 / 1024).toFixed(1);
          const lastTime = f.last_snapshot ? new Date(f.last_snapshot / 1000).toLocaleTimeString() : 'Never';
          const isHistoryActive = this.historyFolder === f.path;

          return `
            <div class="capsule-card">
              <div class="capsule-header">
                <span class="capsule-status ${statusClass}"></span>
                <h4>${escapeHtml(f.name)}</h4>
              </div>
              <div class="capsule-path" title="${escapeHtml(f.path)}">${escapeHtml(f.path)}</div>
              <div class="capsule-stats">
                <span><strong>${f.file_count || 0}</strong> files tracked</span>
                <span><strong>${sizeMb}</strong> MB</span>
              </div>
              <div class="capsule-last">
                Last snapshot: <strong>${lastTime}</strong>
              </div>
              <div class="capsule-action-row" style="display: flex; gap: 8px; margin-top: 10px;">
                <button class="btn-secondary btn-capsule-snap" data-path="${escapeHtml(f.path)}" style="flex: 1;">
                  📸 Snapshot
                </button>
                <button class="btn-secondary btn-capsule-history" data-path="${escapeHtml(f.path)}" style="flex: 1;">
                  ⟲ ${isHistoryActive ? 'Close Rollback' : 'Rollback'}
                </button>
              </div>

              ${isHistoryActive ? `
                <div class="history-drawer" style="margin-top: 12px; border-top: 1px solid rgba(255,255,255,0.08); padding-top: 10px;">
                  <div style="font-size: 11px; font-weight: 600; color: var(--accent-solar); margin-bottom: 6px;">
                    AVAILABLE SNAPSHOT RESTORE POINTS:
                  </div>
                  ${this.historySnapshots.length === 0 ? '<div style="font-size: 11px; color: #64748b;">No snapshots yet.</div>' : `
                    <div style="display: flex; flex-direction: column; gap: 6px; max-height: 180px; overflow-y: auto;">
                      ${this.historySnapshots.map((s, sIdx) => {
                        const sTime = new Date(s.created_at / 1000).toLocaleString();
                        return `
                          <div style="background: rgba(0,0,0,0.4); padding: 6px 8px; border-radius: 6px; display: flex; justify-content: space-between; align-items: center;">
                            <div>
                              <div style="font-size: 11px; color: #fff; font-family: var(--font-mono);">${sTime}</div>
                              <div style="font-size: 10px; color: #64748b;">${s.entries.length} files • ${(s.total_size / 1024).toFixed(0)} KB</div>
                            </div>
                            <button class="btn-primary btn-do-rollback" data-folder="${escapeHtml(f.path)}" data-snap="${s.id}" style="padding: 4px 10px; font-size: 10px;">
                              Restore
                            </button>
                          </div>
                        `;
                      }).join('')}
                    </div>
                  `}
                </div>
              ` : ''}
            </div>
          `;
        }).join('')}
      </div>

      <div style="margin-top: 16px; display: flex; gap: 12px; align-items: center;">
        <button class="btn-secondary" id="btn-capsule-protect-another">
          + Protect Another Folder
        </button>
        <button class="btn-subtle" id="btn-capsule-refresh">
          ↻ Refresh Status
        </button>
      </div>
    `;

    // Wire snapshot now
    this.container.querySelectorAll('.btn-capsule-snap').forEach(btn => {
      btn.addEventListener('click', (e) => {
        const path = e.currentTarget.getAttribute('data-path');
        if (path) this.snapshotNow(path);
      });
    });

    // Wire history view
    this.container.querySelectorAll('.btn-capsule-history').forEach(btn => {
      btn.addEventListener('click', (e) => {
        const path = e.currentTarget.getAttribute('data-path');
        if (this.historyFolder === path) {
          this.historyFolder = null;
          this.render();
        } else {
          this.viewHistory(path);
        }
      });
    });

    // Wire rollback execution
    this.container.querySelectorAll('.btn-do-rollback').forEach(btn => {
      btn.addEventListener('click', (e) => {
        const folder = e.currentTarget.getAttribute('data-folder');
        const snap = e.currentTarget.getAttribute('data-snap');
        if (folder && snap) this.rollback(folder, snap);
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
    toast.style.position = 'fixed';
    toast.style.bottom = '24px';
    toast.style.right = '24px';
    toast.style.background = isError ? 'rgba(255, 51, 102, 0.95)' : 'rgba(16, 185, 129, 0.95)';
    toast.style.color = '#fff';
    toast.style.padding = '10px 18px';
    toast.style.borderRadius = '8px';
    toast.style.fontSize = '12px';
    toast.style.fontWeight = '600';
    toast.style.zIndex = '9999';
    toast.style.boxShadow = '0 6px 20px rgba(0,0,0,0.5)';
    document.body.appendChild(toast);
    setTimeout(() => {
      toast.style.opacity = '0';
      toast.style.transition = 'opacity 0.3s ease';
      setTimeout(() => toast.remove(), 300);
    }, 3500);
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
