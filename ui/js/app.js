import { HoloCore } from './holo-core.js';
import { ModeController } from './mode-controller.js';
import { PreviewBay } from './preview-bay.js';
import { onEngineEvent, invoke } from './event-stream.js';

const holo = new HoloCore(document.getElementById('holo-canvas'));
const modes = new ModeController();
const terminal = document.getElementById('terminal');
const counter = document.getElementById('counter');
const previewBay = new PreviewBay(document.getElementById('preview-grid-container'));
let restored = 0;
let lastScanTarget = null;

function logLine(msg) {
  const ts = new Date().toISOString().slice(11, 23); // millisecond timestamps
  terminal.textContent += `${ts}  ${msg}\n`;
  terminal.scrollTop = terminal.scrollHeight;
}

// Initial system status
logLine('CLOCKVERSE FORENSIC ENGINE INITIALIZED');
logLine('System Architecture: Rust SectorForge + Python Sidecar + Tauri Shell');
logLine('Mode: Quantum ChronoScan active');

// Problem #4 fix: Mode FSM event queue and suspend/resume wiring
let scanPaused = false;
const eventQueue = [];

modes.on('sector', 'suspend', () => {
  scanPaused = true;
  logLine('⏸ SCAN SUSPENDED (mode switch)');
});

modes.on('sector', 'resume', () => {
  scanPaused = false;
  logLine('▶ SCAN RESUMED');
  while (eventQueue.length > 0) {
    handleEngineEvent(eventQueue.shift());
  }
});

modes.on('chrono', 'suspend', () => {
  scanPaused = true;
  logLine('⏸ CHRONO SUSPENDED (mode switch)');
});

modes.on('chrono', 'resume', () => {
  scanPaused = false;
  logLine('▶ CHRONO RESUMED');
  while (eventQueue.length > 0) {
    handleEngineEvent(eventQueue.shift());
  }
});

function handleEngineEvent(e) {
  if (scanPaused) {
    eventQueue.push(e);
    return;
  }

  switch (e.type) {
    case 'scan_started':
      lastScanTarget = e.target;
      logLine(`▶ SCAN STARTED target=${e.target} total_sectors=${e.total_sectors}`);
      break;
    case 'sector_result':
      holo.ignite(e.particle_index, e.state_code);  // crystal ignites = real progress
      logLine(`CARVE cluster=${e.cluster} sig=${e.signature} confidence=${e.confidence}`);
      break;
    case 'file_verified':
      logLine(`✓ VERIFIED path=${e.path} sha256=${e.sha256.slice(0, 16)}...`);
      break;
    case 'file_restored':
      counter.textContent = ++restored;             // count-up on real write confirm
      logLine(`★ RESTORED path=${e.path} (${e.bytes} bytes)`);
      break;
    case 'scan_complete':
      logLine(`✓ COMPLETE found=${e.found} verified=${e.verified} failures=${e.failures}`);
      // Preview Bay: load recovered files once the scan finishes.
      if (lastScanTarget) previewBay.loadFromImage(lastScanTarget);
      break;
    case 'error':
      logLine(`✖ ERROR [${e.code}] ${e.message}`);
      break;
    default:
      if (e.type) logLine(`EVENT [${e.type}]`);
      break;
  }
}

// Register the engine event handler
onEngineEvent(handleEngineEvent);

// File restored via Preview Bay → counter roll-up.
document.addEventListener('file-restored', (e) => {
  counter.textContent = ++restored;
  logLine(`RESTORED ${e.detail.name} → ${e.detail.bytes} bytes`);
});

// Integrity Gate failure → restore blocked, honourable honesty rule.
document.addEventListener('gate-failed', (e) => {
  logLine(`GATE FAIL ${e.detail.name} — truncated/corrupt, restore blocked`);
});

const modeToggle = document.getElementById('mode-toggle');
if (modeToggle) {
  modeToggle.addEventListener('click', () => {
    modes.toggle();
    logLine(`FSM MODE SWAPPED → ${modes.mode.toUpperCase()}`);
  });
}

// Problem #3 fix: File picker & scan type handling
const browseBtn = document.getElementById('btn-browse');
const imageInput = document.getElementById('image-file-input');
const selectedFileLabel = document.getElementById('selected-file');

if (browseBtn) {
  browseBtn.addEventListener('click', async () => {
    let filePath = null;
    if (typeof window.__TAURI__ !== 'undefined') {
      try {
        if (window.__TAURI__.dialog?.open) {
          filePath = await window.__TAURI__.dialog.open({
            filters: [{ name: 'Disk Images', extensions: ['dd', 'img', 'raw', 'E01'] }]
          });
        } else {
          filePath = await invoke('select_image_file');
        }
      } catch (_) {
        filePath = await invoke('select_image_file');
      }
    }

    if (filePath) {
      if (Array.isArray(filePath)) filePath = filePath[0];
      window.selectedImagePath = filePath;
      if (selectedFileLabel) selectedFileLabel.textContent = `Selected: ${filePath}`;
    } else if (imageInput) {
      // Browser dev fallback
      imageInput.click();
    }
  });
}

if (imageInput) {
  imageInput.addEventListener('change', (e) => {
    const file = e.target.files[0];
    if (file) {
      const fullPath = file.path || file.name;
      window.selectedImagePath = fullPath;
      if (selectedFileLabel) selectedFileLabel.textContent = `Selected: ${file.name}`;
    }
  });
}

document.querySelectorAll('input[name="scan-type"]').forEach(radio => {
  radio.addEventListener('change', (e) => {
    const isDrive = e.target.value === 'drive';
    const pickerSec = document.getElementById('file-picker-section');
    const driveWarn = document.getElementById('drive-warning');
    if (pickerSec) pickerSec.classList.toggle('hidden', isDrive);
    if (driveWarn) driveWarn.classList.toggle('hidden', !isDrive);
  });
});

const scanBtn = document.getElementById('btn-scan');
if (scanBtn) {
  scanBtn.addEventListener('click', async () => {
    const scanTypeRadio = document.querySelector('input[name="scan-type"]:checked');
    const scanType = scanTypeRadio ? scanTypeRadio.value : 'image';

    if (scanType === 'image' && !window.selectedImagePath) {
      alert('Please select a disk image file (.dd, .img, .raw) first.');
      return;
    }

    const target = scanType === 'image' ? window.selectedImagePath : '\\\\.\\PhysicalDrive0';

    const warn = document.getElementById('recovery-window');
    warn.textContent = '⚠ VaultGuard Active: Read-only access enforced. Minimal writes to this drive until recovery.';
    warn.classList.remove('hidden');
    scanBtn.disabled = true;
    scanBtn.innerHTML = `
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" class="spin">
        <circle cx="12" cy="12" r="10" stroke-dasharray="32" stroke-dashoffset="12"></circle>
      </svg>
      Scanning Platter...
    `;

    try {
      await invoke('start_scan', { target });
    } catch (err) {
      logLine(`Invoke error: ${err}`);
    } finally {
      setTimeout(() => {
        scanBtn.disabled = false;
        scanBtn.innerHTML = `
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5">
            <circle cx="12" cy="12" r="10"></circle>
            <polygon points="10 8 16 12 10 16 10 8"></polygon>
          </svg>
          Start Deep Scan
        `;
      }, 4000);
    }
  });
}

/* ============================================
   SESSION CONSTELLATION VIEW (Phase 1)
   ============================================ */
let constellationRows = 0;

function setConstellationStatus(text, color = 'var(--accent-emerald)') {
  const el = document.getElementById('constellation-status');
  if (el) { el.textContent = text; el.style.color = color; }
}

function metaChip(id, value) {
  const el = document.getElementById(id);
  if (el) el.textContent = value;
}

function renderConstellation(rows) {
  const grid = document.getElementById('constellation-grid');
  if (!grid) return;
  if (rows.length === 0) {
    grid.innerHTML = `
      <div class="constellation__empty">
        <div class="constellation__empty-ring"></div>
        <span>No telemetry indexed yet</span>
        <span class="dim">Ingest a JSONL log to build your workspace constellation.</span>
      </div>`;
    setConstellationStatus('IDLE');
    metaChip('meta-events', '0');
    metaChip('meta-files', '0');
    metaChip('meta-ts', 't-0');
    return;
  }
  grid.classList.add('constellation__grid');
  grid.innerHTML = rows.map((r, i) => {
    const dot = r.op === 'delete' ? 'delete' : r.op === 'patch' ? 'patch' : 'write';
    const ts = `t+${r.ts}`;
    const ops = r.ops > 1 ? `${r.ops} ops` : '';
    return `
      <div class="constellation__row" style="animation-delay:${i * 40}ms">
        <span class="constellation__dot constellation__dot--${dot}"></span>
        <span class="constellation__path" title="${escapeHtml(r.path)}">${escapeHtml(r.path)}</span>
        <span class="constellation__ops">${ops}</span>
        <span class="constellation__ts">${ts}</span>
      </div>`;
  }).join('');
  setConstellationStatus('LIVE');
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

// Build constellation rows from a full reconstruct map (file_path -> FileState)
function buildConstellationRows(files, maxEvents) {
  const list = [];
  for (const [path, st] of Object.entries(files || {})) {
    const op = st.bytes && st.patch_count === 0 ? 'write'
      : st.bytes && st.patch_count > 0 ? 'patch' : 'delete';
    list.push({ path, op, ts: st.last_ts || 0, ops: st.patch_count || 0 });
  }
  list.sort((a, b) => b.ts - a.ts);
  return list.slice(0, maxEvents || 60);
}

async function refreshConstellation() {
  try {
    const summary = await invoke('session_summary');
    metaChip('meta-events', summary.event_count ?? 0);
    metaChip('meta-files', summary.file_count ?? 0);
    metaChip('meta-ts', `t+${summary.max_ts_micros ?? 0}`);
    if ((summary.event_count ?? 0) > 0) setConstellationStatus('LIVE');
  } catch (e) {
    // browser-dev mode: summary invoke is mocked -> silent
  }
}

async function ingestFile(file) {
  const text = await file.text();
  logLine(`▶ INGEST ${file.name} (${file.size} bytes)`);
  try {
    const res = await invoke('chrono_ingest', { jsonl: text });
    logLine(`✓ INDEXED → ${res}`);

    // Reconstruct current state and paint the constellation.
    const snapshot = await invoke('chrono_time_travel', { as_of_micros: Number.MAX_SAFE_INTEGER });
    const rows = buildConstellationRows(snapshot.files);
    renderConstellation(rows);
    await refreshConstellation();
  } catch (err) {
    logLine(`✖ INGEST ERROR: ${err}`);
  }
}

const jsonlPicker = document.getElementById('jsonl-picker');
if (jsonlPicker) {
  jsonlPicker.addEventListener('change', (e) => {
    const file = e.target.files[0];
    if (file) ingestFile(file);
    e.target.value = '';
  });
}

// Add an "Ingest Log" entry point into the scan card header area.
const scanCard = document.getElementById('scan-card');
if (scanCard) {
  const ingestBtn = document.createElement('button');
  ingestBtn.className = 'btn-magnetic';
  ingestBtn.id = 'btn-ingest';
  ingestBtn.style.marginLeft = '10px';
  ingestBtn.innerHTML = '⟳';
  ingestBtn.title = 'Ingest telemetry log (.jsonl)';
  ingestBtn.addEventListener('click', () => jsonlPicker && jsonlPicker.click());
  scanCard.querySelector('h2').after(ingestBtn);
}

// Browser-dev mode: mock a small telemetry ingest so the UI is previewable.
if (typeof window.__TAURI__ === 'undefined') {
  setTimeout(() => {
    const sample = [
      { type: 'scan_started', target: 'workspace', total_sectors: 0 }
    ];
    const mockFile = `
{"ts_micros":1000,"file_path":"src/main.py","op":{"kind":"write","content":[109,97,105,110]}}
{"ts_micros":2000,"file_path":"src/main.py","op":{"kind":"patch","offset":0,"delete_len":4,"insert":[109,97,110]}}
{"ts_micros":1500,"file_path":"src/utils.py","op":{"kind":"write","content":[117,116,105,108]}}
`;
    setTimeout(() => {
      ingestFile(new File([mockFile], 'session.log', { type: 'text/plain' }));
    }, 1200);
  }, 600);
}

export {};
