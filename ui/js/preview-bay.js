// Preview Bay — Real recovered files grid with instant 1-click restore
import { invoke } from './event-stream.js';

export class PreviewBay {
  constructor(container) {
    this.container = container;
    this.files = [];
    this.renderEmpty('Ready. Select an image or run Demo Platter to preview carved files.');
  }

  setRecoveredFiles(files) {
    this.files = files || [];
    this.render();
  }

  renderEmpty(msg) {
    if (!this.container) return;
    this.container.innerHTML = `
      <div class="preview-empty">
        <div class="empty-icon-radar"></div>
        <p class="empty-title">${msg}</p>
        <p class="empty-sub">Deep Sector carving automatically indexes JPEG, PNG, PDF, ZIP, and MP4 entities.</p>
      </div>
    `;
  }

  render() {
    if (!this.container) return;
    this.container.innerHTML = '';
    if (!this.files || this.files.length === 0) {
      this.renderEmpty('No carved entities detected on target platter.');
      return;
    }

    const header = document.createElement('div');
    header.className = 'preview-bay-header';
    header.innerHTML = `
      <span class="preview-count-badge">${this.files.length} ENTITIES CARVED</span>
      <span class="preview-help-text">Click 'Restore' to export file to your Downloads folder.</span>
    `;
    this.container.appendChild(header);

    const grid = document.createElement('div');
    grid.className = 'preview-grid';

    for (const f of this.files) {
      const card = document.createElement('div');
      card.className = 'recovered-card';

      const ext = (f.extension || 'bin').toUpperCase();
      const isImg = ['JPG', 'JPEG', 'PNG'].includes(ext);
      const icon = isImg ? '🖼️' : ext === 'PDF' ? '📄' : ext === 'ZIP' ? '📦' : '🎬';
      const sizeStr = f.size_bytes > 1048576 
        ? `${(f.size_bytes / 1048576).toFixed(2)} MB`
        : `${(f.size_bytes / 1024).toFixed(1)} KB`;

      card.innerHTML = `
        <div class="card-thumb-box">
          ${isImg && f.path ? `<img class="card-img-thumb" src="${typeof window.__TAURI__ !== 'undefined' && window.__TAURI__.core?.convertFileSrc ? window.__TAURI__.core.convertFileSrc(f.path) : `file://${f.path.replace(/\\/g, '/')}`}" onerror="this.outerHTML='<span class=\'fallback-icon\'>${icon}</span>'" />` : `<span class="fallback-icon">${icon}</span>`}
          <span class="card-ext-badge">${ext}</span>
        </div>
        <div class="card-details">
          <div class="card-filename" title="${escapeHtml(f.name)}">${escapeHtml(f.name)}</div>
          <div class="card-meta-row">
            <span class="card-size">${sizeStr}</span>
            <span class="card-conf">${Math.round((f.confidence || 0.9) * 100)}% Match</span>
          </div>
          <button class="btn-restore-card" data-path="${escapeHtml(f.path)}" data-name="${escapeHtml(f.name)}">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
              <polyline points="7 10 12 15 17 10"></polyline>
              <line x1="12" y1="15" x2="12" y2="3"></line>
            </svg>
            Restore
          </button>
        </div>
      `;

      const restoreBtn = card.querySelector('.btn-restore-card');
      restoreBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        this.restoreFile(f, restoreBtn);
      });

      grid.appendChild(card);
    }

    this.container.appendChild(grid);
  }

  async restoreFile(file, btnEl) {
    if (!file || !file.path) return;
    btnEl.disabled = true;
    btnEl.textContent = 'Restoring...';

    try {
      const savedPath = await invoke('restore_file_to_disk', {
        sourcePath: file.path
      });

      btnEl.classList.add('restored');
      btnEl.disabled = false;
      btnEl.innerHTML = '✓ Open in Explorer';
      btnEl.title = `Saved to: ${savedPath}. Click to open folder.`;
      btnEl.onclick = (e) => {
        e.stopPropagation();
        invoke('open_in_explorer', { path: savedPath });
      };

      document.dispatchEvent(new CustomEvent('file-restored', {
        detail: { name: file.name, bytes: file.size_bytes, savedPath }
      }));
    } catch (err) {
      console.error('Failed to restore file:', err);
      btnEl.disabled = false;
      btnEl.textContent = 'Failed';
      setTimeout(() => { btnEl.textContent = 'Restore'; }, 2000);
    }
  }
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}
