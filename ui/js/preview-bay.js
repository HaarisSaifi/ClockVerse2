// Preview Bay — thumbnail grid for recovered files.
// Free users browse thumbnails; restore is gated behind Pro.
import { invoke } from './event-stream.js';

export class PreviewBay {
  constructor(container) {
    this.container = container;
    this.files = [];
    this.currentImage = null;
  }

  async loadFromImage(imagePath) {
    this.currentImage = imagePath;
    try {
      const result = await invoke('sidecar_list_partitions', { imagePath });
      if (result?.status === 'ok' && result.partitions.length > 0) {
        const ntfs = result.partitions.find(p => (p.description || '').includes('NTFS'));
        if (ntfs) {
          const files = await invoke('scan_deleted_files', { imagePath });
          this.files = files || [];
          this.render();
        }
      }
    } catch (err) {
      this.renderEmpty('No previewable partitions found');
    }
  }

  render() {
    if (!this.container) return;
    this.container.innerHTML = '';
    if (this.files.length === 0) {
      this.renderEmpty('No deleted files detected');
      return;
    }
    const grid = document.createElement('div');
    grid.className = 'preview-grid';
    for (const f of this.files) {
      const card = document.createElement('div');
      card.className = 'preview-card holo-card';
      card.innerHTML = `
        <div class="preview-thumb" data-offset="${f.record_number}">⏳</div>
        <div class="preview-name">${escapeHtml(f.name)}</div>
        <div class="preview-meta">${(f.size_bytes / 1024).toFixed(1)} KB</div>
        <div class="confidence-ring" style="--confidence: ${f.fixup_ok ? 0.85 : 0.40}"></div>
      `;
      card.addEventListener('click', (e) => this.extractFile(f, e));
      grid.appendChild(card);
    }
    this.container.appendChild(grid);
    // Lazy-load thumbnails (IntersectionObserver — sirf visible cards)
    this.observeThumbs();
  }

  renderEmpty(msg) {
    if (!this.container) return;
    this.container.innerHTML = `<div class="constellation__empty"><span>${msg}</span></div>`;
  }

  observeThumbs() {
    if (!this.currentImage) return;
    const io = new IntersectionObserver(async (entries) => {
      for (const e of entries) {
        if (!e.isIntersecting) continue;
        io.unobserve(e.target);
        const offset = e.target.dataset.offset;
        const out = `/tmp/thumb_${offset}.png`;
        try {
          const res = await invoke('sidecar_thumbnail', {
            imagePath: this.currentImage, offset: parseInt(offset, 10), outPath: out
          });
          if (res?.status === 'ok') {
            e.target.innerHTML = `<img src="file://${out}" alt="">`;
          } else {
            e.target.textContent = '📄';
          }
        } catch (_) {
          e.target.textContent = '📄';
        }
      }
    }, { rootMargin: '50px' });
    this.container.querySelectorAll('.preview-thumb').forEach(el => io.observe(el));
  }

  async extractFile(f, e) {
    const out = `/tmp/recovered_${f.name}`;
    try {
      const bytes = await invoke('extract_deleted_file', {
        imagePath: this.currentImage, recordNumber: f.record_number, outputPath: out
      });
      if (bytes) {
        // Integrity Gate — restore se PEHLE verify
        const ok = await invoke('verify_carved_file', { path: out });
        if (ok) {
          // teal already emit ho chuka (FileVerified). Ab restore = violet.
          document.dispatchEvent(new CustomEvent('file-restored', {
            detail: { name: f.name, bytes }
          }));
        } else {
          // Gate FAIL — file restore mat karo, UI pe "unverified" dikhao.
          e?.currentTarget?.classList?.add('unverified');
          document.dispatchEvent(new CustomEvent('gate-failed', {
            detail: { name: f.name }
          }));
        }
      }
    } catch (err) {
      console.error('extract failed', err);
    }
  }
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}
