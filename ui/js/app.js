// Recovery Studio: native operations only. No fabricated files or progress.
const $ = id => document.getElementById(id);
const native = window.__TAURI__;
const invoke = (command, args = {}) => native?.core?.invoke ? native.core.invoke(command, args) : Promise.reject(new Error('Open the ClockVerse desktop app to access files. This browser view is a UI preview.'));
const bytes = n => { if (!Number.isFinite(n)) return '—'; const i = Math.min(4, Math.floor(Math.log(Math.max(n, 1)) / Math.log(1024))); return `${(n / 1024 ** i).toFixed(i ? 1 : 0)} ${['B', 'KiB', 'MiB', 'GiB', 'TiB'][i]}`; };
function element(tag, text, className) { const el = document.createElement(tag); if (text !== undefined) el.textContent = text; if (className) el.className = className; return el; }
function button(label, action, className = 'button secondary') { const el = element('button', label, className); el.type = 'button'; el.addEventListener('click', async () => { el.disabled = true; try { await action(el); } catch (e) { showError(e); } finally { el.disabled = false; } }); return el; }
function showError(error) { $('scan-error').textContent = String(error?.message || error); }

let mode = 'drive'; let report = null; let page = 0; let scanning = false; let started = 0; let ticker;
let systemDrives = [];
const PAGE_SIZE = 40;

$('connection').textContent = native ? '● Desktop connected' : '○ Browser preview';
$('connection').classList.toggle('connected', Boolean(native));

// Mode switcher: Easy vs Pro
$('mode-easy').onclick = () => {
  document.body.classList.add('easy-mode');
  $('mode-easy').classList.add('selected');
  $('mode-pro').classList.remove('selected');
};
$('mode-pro').onclick = () => {
  document.body.classList.remove('easy-mode');
  $('mode-pro').classList.add('selected');
  $('mode-easy').classList.remove('selected');
};

function setMode(next) {
  if (scanning) return;
  mode = next;
  $('mode-drive').classList.toggle('selected', mode === 'drive');
  $('mode-image').classList.toggle('selected', mode === 'image');
  $('mode-folder').classList.toggle('selected', mode === 'folder');

  $('drive-group').classList.toggle('hidden', mode !== 'drive');
  $('path-group').classList.toggle('hidden', mode === 'drive');

  if (mode === 'drive') {
    $('source-hint').textContent = 'Directly scan USB pen drives, SD cards, or disk partitions without creating image files.';
    $('safety-tip').innerHTML = 'No image file required for direct drive scans.<br><small>ClockVerse uses read-only access (FILE_SHARE_READ) to safeguard existing data.</small>';
  } else if (mode === 'image') {
    $('source-hint').textContent = 'Deleted without a backup? Scan an offline disk or volume image. Original filenames may survive in NTFS records.';
    $('source').placeholder = 'D:\\disk-image.img';
    $('safety-tip').innerHTML = 'No snapshot required for image recovery.<br><small>Keep the original drive idle until recovery is complete.</small>';
  } else {
    $('source-hint').textContent = 'Copy existing files from a folder. Folder mode cannot see permanently deleted files; use a drive or image for that.';
    $('source').placeholder = 'D:\\Documents';
    $('safety-tip').innerHTML = 'Folder mode is for file migration.<br><small>To recover deleted files, select a drive or image.</small>';
  }
}

$('mode-drive').onclick = () => setMode('drive');
$('mode-image').onclick = () => setMode('image');
$('mode-folder').onclick = () => setMode('folder');

$('browse-source').onclick = async () => {
  try {
    const path = await invoke(mode === 'image' ? 'select_image_file' : 'select_folder');
    if (path) $('source').value = path;
  } catch(e) { showError(e); }
};

$('browse-destination').onclick = async () => {
  try {
    const path = await invoke('select_folder');
    if (path) $('destination').value = path;
  } catch(e) { showError(e); }
};

function updateDrivePill() {
  const selectedPath = $('drive-select').value;
  const info = systemDrives.find(d => d.device_path === selectedPath || d.letter === selectedPath);
  const pill = $('drive-pill');
  if (!info) {
    pill.textContent = 'Select a valid drive';
    pill.className = 'drive-pill';
    return;
  }
  if (info.is_removable) {
    pill.className = 'drive-pill';
    pill.innerHTML = `<span>💾 <b>USB/SD Removable</b> (No TRIM)</span><span>Outlook: <b>${info.recovery_outlook}</b></span>`;
  } else if (info.is_ssd) {
    pill.className = 'drive-pill warning';
    pill.innerHTML = `<span>⚡ <b>NVMe / SATA SSD</b> (TRIM Active)</span><span>Outlook: <b>${info.recovery_outlook}</b></span>`;
  } else {
    pill.className = 'drive-pill';
    pill.innerHTML = `<span>💽 <b>Mechanical HDD</b> (No TRIM)</span><span>Outlook: <b>${info.recovery_outlook}</b></span>`;
  }
}
$('drive-select').onchange = updateDrivePill;

async function refreshDrives() {
  try {
    systemDrives = await invoke('list_system_drives');
    const select = $('drive-select');
    select.replaceChildren();
    const vssSelect = $('vss-volume');
    vssSelect.replaceChildren();

    if (!systemDrives.length) {
      select.append(element('option', 'No drives detected'));
      return;
    }

    for (const d of systemDrives) {
      const typeIcon = d.is_removable ? '💾 USB' : d.is_ssd ? '⚡ SSD' : '💽 HDD';
      const opt = element('option', `${d.letter} [${typeIcon}] ${d.label || 'Local Disk'} (${bytes(d.total_bytes)})`);
      opt.value = d.device_path;
      select.append(opt);

      // Volume for VSS
      const vssOpt = element('option', `${d.letter} (${d.label || 'Volume'})`);
      vssOpt.value = d.letter.endsWith('\\') ? d.letter : `${d.letter}\\`;
      vssSelect.append(vssOpt);
    }
    updateDrivePill();
  } catch (e) {
    $('drive-pill').textContent = 'Drive detection: ' + (e.message || e);
  }
}
$('refresh-drives').onclick = refreshDrives;

async function checkAdmin() {
  try {
    const isAdmin = await invoke('check_admin_privileges');
    const badge = $('admin-badge');
    const banner = $('elevation-banner');
    if (isAdmin) {
      badge.textContent = '🛡️ Admin: Raw Access OK';
      badge.className = 'badge admin-ok';
      badge.title = 'Running as Administrator: Raw drives and VSS are fully accessible.';
      if (banner) banner.classList.add('hidden');
    } else {
      badge.textContent = '⚠️ Standard User';
      badge.className = 'badge admin-warn';
      badge.title = 'Running as Standard User. Click "Relaunch as Admin" to enable raw physical drive access and VSS snapshots.';
      if (banner) banner.classList.remove('hidden');
    }
  } catch (_) {}
}

const elevateBtn = $('btn-elevate');
if (elevateBtn) {
  elevateBtn.onclick = async () => {
    elevateBtn.disabled = true;
    elevateBtn.textContent = 'Requesting elevation…';
    try {
      await invoke('relaunch_as_admin');
    } catch (e) {
      alert('Elevation: ' + (e.message || e));
      elevateBtn.disabled = false;
      elevateBtn.textContent = '⚡ 1-Click Relaunch as Admin';
    }
  };
}


function setBusy(busy) {
  scanning = busy;
  for (const id of ['scan','quick-scan','demo','source','destination','browse-source','browse-destination','mode-drive','mode-image','mode-folder','drive-select','refresh-drives']) {
    const el = $(id);
    if (el) el.disabled = busy;
  }
  $('cancel').disabled = !busy;
  $('scan').textContent = busy ? 'Deep scan running…' : 'Deep Scan →';
  $('quick-scan').textContent = busy ? 'Scanning…' : '⚡ Quick Scan (15s)';
  $('observatory').dataset.state = busy ? 'scanning' : 'done';
  if (busy) {
    started = Date.now();
    ticker = setInterval(() => { $('metric-time').textContent = `${Math.floor((Date.now()-started)/1000)}s`; }, 1000);
  } else {
    clearInterval(ticker);
  }
}

function progress(p) {
  $('phase').textContent = p.phase;
  $('metric-found').textContent = p.found;
  $('metric-read').textContent = bytes(p.bytes_scanned);
  const value = p.total_bytes ? Math.min(100, Math.floor(p.bytes_scanned / p.total_bytes * 100)) : 0;
  $('progress-fill').style.width = `${value}%`;
  $('progress').setAttribute('aria-valuenow', value);
  $('progress-detail').textContent = p.total_bytes ? `${bytes(p.bytes_scanned)} of ${bytes(p.total_bytes)} scanned · ${value}%` : 'Reading filesystem structures…';
}
if (native?.event?.listen) native.event.listen('recovery-progress', e => progress(e.payload)).catch(showError);

async function runScan(query = '') {
  if (scanning || backupBusy) { showError('A disk operation is already running.'); return; }
  
  let target = '';
  if (mode === 'drive') {
    target = $('drive-select').value;
  } else {
    target = $('source').value.trim();
  }
  const destination = $('destination').value.trim();

  if (!target || !destination) {
    showError('Please choose both a source and a recovery destination folder.');
    return;
  }

  // Pre-check anti self-overwrite
  const targetUpper = target.toUpperCase();
  const destUpper = destination.toUpperCase();
  if ((targetUpper.startsWith('\\\\.\\') && destUpper.startsWith(targetUpper.slice(4, 6))) ||
      (targetUpper.length >= 2 && targetUpper[1] === ':' && destUpper.startsWith(targetUpper.slice(0, 2)))) {
    showError('Anti-Overwrite Block: You cannot save recovered files to the same drive you are scanning! Choose a DIFFERENT drive.');
    return;
  }

  $('scan-error').textContent = '';
  setBusy(true);

  const isQuick = query.startsWith(':quick:');
  $('activity-badge').textContent = isQuick ? 'QUICK MFT SCAN' : (query ? 'TARGETED SEARCH' : 'DEEP RECOVERING');
  $('phase').textContent = isQuick ? 'Inspecting Master File Table (MFT)…' : 'Scanning sectors & signatures…';
  $('progress-detail').textContent = isQuick ? 'Quickly recovering non-overwritten NTFS records.' : 'Preparing read-only source access…';
  $('progress-fill').style.width = '0%';
  $('progress').setAttribute('aria-valuenow', 0);
  $('metric-found').textContent = '0';

  try {
    const next = await invoke('recovery_scan', { target, destination, query: query || null });
    report = next;
    page = 0;
    $('filter').value = '';
    $('origin').value = '';
    renderFiles();
    $('metric-found').textContent = report.files.length;
    $('metric-read').textContent = bytes(report.bytes_scanned);
    const partial = report.partial || report.cancelled || (report.total_bytes && report.bytes_scanned < report.total_bytes);
    $('activity-badge').textContent = report.cancelled ? 'STOPPED' : partial ? 'PARTIAL RESULTS' : 'SCAN FINISHED';
    $('phase').textContent = report.files.length ? `${report.files.length} files. A new beginning.` : 'No deleted files found.';
    $('progress-detail').textContent = `${partial ? 'Partial scan. ' : ''}${report.skipped} candidates skipped.`;
    $('open-output').disabled = false;
    if (query && !isQuick) {
      chat('assistant', `Targeted scan ${report.cancelled ? 'stopped' : 'finished'}: ${report.files.length} files saved.`);
    }
  } catch(e) {
    showError(e);
    $('activity-badge').textContent = 'NEEDS ATTENTION';
    $('phase').textContent = 'Let’s check the source.';
    $('progress-detail').textContent = 'The scan did not finish. Run as Administrator if scanning raw drives.';
  } finally {
    setBusy(false);
  }
}

$('quick-scan').onclick = () => runScan(':quick:');
$('scan').onclick = () => runScan('');
$('cancel').onclick = async () => {
  try {
    await invoke('recovery_cancel');
    $('cancel').disabled = true;
    $('activity-badge').textContent = 'STOPPING';
  } catch(e) { showError(e); }
};

$('demo').onclick = async () => {
  try {
    if (!$('destination').value.trim()) {
      const dest = await invoke('select_folder');
      if (!dest) return;
      $('destination').value = dest;
    }
    $('demo').disabled = true;
    const path = await invoke('create_demo_platter');
    setMode('image');
    $('source').value = path;
    await runScan();
  } catch(e) { showError(e); } finally { $('demo').disabled = scanning; }
};

$('open-output').onclick = () => invoke('open_in_explorer', { path: report.output_dir }).catch(showError);

let activeCategory = 'all';

function getFileCategory(extension) {
  const ext = (extension || '').toLowerCase().trim();
  if (['jpg', 'jpeg', 'png', 'gif', 'bmp', 'webp', 'svg', 'ico'].includes(ext)) return 'images';
  if (['pdf', 'docx', 'doc', 'xlsx', 'xls', 'pptx', 'txt', 'csv', 'json', 'log', 'xml', 'md'].includes(ext)) return 'docs';
  if (['mp4', 'mov', 'mkv', 'avi', 'mp3', 'wav', 'flac', 'riff'].includes(ext)) return 'media';
  if (['zip', 'rar', '7z', 'tar', 'gz', 'bz2'].includes(ext)) return 'archives';
  return 'other';
}

function setCategory(cat) {
  activeCategory = cat;
  for (const c of ['all', 'images', 'docs', 'media', 'archives']) {
    const el = $('cat-' + c);
    if (el) el.classList.toggle('active', c === cat);
  }
  page = 0;
  renderFiles();
}

for (const cat of ['all', 'images', 'docs', 'media', 'archives']) {
  const el = $('cat-' + cat);
  if (el) el.onclick = () => setCategory(cat);
}

function filteredFiles() {
  const q = $('filter').value.toLowerCase().trim(), origin = $('origin').value;
  return (report?.files || []).filter(f => {
    const matchesQ = !q || `${f.name} ${f.extension}`.toLowerCase().includes(q);
    const matchesOrigin = !origin || f.origin.startsWith(origin);
    const matchesCat = activeCategory === 'all' || getFileCategory(f.extension) === activeCategory;
    return matchesQ && matchesOrigin && matchesCat;
  });
}

function renderFiles() {
  const files = filteredFiles(), pages = Math.max(1, Math.ceil(files.length/PAGE_SIZE));
  page = Math.min(page, pages - 1);
  const allFiles = report?.files || [];
  if ($('count-all')) $('count-all').textContent = allFiles.length;
  if ($('count-images')) $('count-images').textContent = allFiles.filter(f => getFileCategory(f.extension) === 'images').length;
  if ($('count-docs')) $('count-docs').textContent = allFiles.filter(f => getFileCategory(f.extension) === 'docs').length;
  if ($('count-media')) $('count-media').textContent = allFiles.filter(f => getFileCategory(f.extension) === 'media').length;
  if ($('count-archives')) $('count-archives').textContent = allFiles.filter(f => getFileCategory(f.extension) === 'archives').length;

  $('result-count').textContent = report?.files.length || 0;
  $('result-summary').textContent = `${files.length} matching · files already saved`;
  $('warnings').replaceChildren();
  for (const warning of report?.warnings || []) $('warnings').append(element('p', warning));
  $('file-list').replaceChildren();
  if (!files.length) {
    const empty = element('div', undefined, 'empty-state');
    empty.append(element('h3', 'No matching files in this view.'), element('p', 'Try selecting "All Files" or run a targeted search in the assistant.'));
    $('file-list').append(empty);
  }
  for (const f of files.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE)) {
    const row = element('div', undefined, 'file-row'), detail = element('div');
    detail.style.minWidth = '0';
    const title = element('div', f.name, 'file-name');
    title.title = f.name;
    detail.append(title, element('div', f.integrity, 'file-detail'));
    detail.title = `SHA-256: ${f.sha256}\n${f.path}`;
    const cat = getFileCategory(f.extension);
    row.append(
      element('div', (f.extension || 'file').slice(0, 5).toUpperCase(), `file-icon cat-${cat}`),
      detail,
      element('span', f.origin, 'origin-label'),
      element('span', bytes(f.size_bytes), 'file-size'),
      button('Preview', () => previewFile(f))
    );
    $('file-list').append(row);
  }
  $('page-info').textContent = `${files.length ? page + 1 : 0} / ${files.length ? pages : 0}`;
  $('prev').disabled = page === 0;
  $('next').disabled = page + 1 >= pages;
}

$('filter').oninput = $('origin').onchange = () => { page = 0; renderFiles(); };
$('prev').onclick = () => { page--; renderFiles(); };
$('next').onclick = () => { page++; renderFiles(); };

window.addEventListener('focus', () => {
  refreshDrives();
  checkAdmin();
});


function chat(role, text) {
  const item = element('div', text, `chat-message ${role}`);
  $('chat-log').append(item);
  while ($('chat-log').children.length > 60) $('chat-log').firstChild.remove();
  $('chat-log').scrollTop = $('chat-log').scrollHeight;
  return item;
}

async function exportSnapshot(snapshotId, relPath = null) {
  let destination = $('destination').value.trim();
  if (!destination) destination = await invoke('select_folder');
  if (!destination) return;
  const path = await backupJob(() => invoke('capsule_export', { snapshotId, relPath, destination }));
  chat('assistant', `Snapshot content verified and exported to ${path}.`);
  await invoke('open_in_explorer', { path });
}

$('chat-form').onsubmit = async e => {
  e.preventDefault();
  const query = $('chat-query').value.trim();
  if (!query) return;
  chat('user', query);
  $('chat-query').value = '';
  $('chat-send').disabled = true;
  try {
    const reply = await invoke('recovery_search', { query });
    const message = chat('assistant', `Searching for “${reply.query}”. I checked: ${reply.checked.join('; ')}. Found ${reply.matches.length} scan matches and ${reply.snapshots.length} snapshot versions.`);
    for (const f of reply.matches.slice(0, 5)) message.append(button(`Locate ${f.name}`, () => invoke('open_in_explorer', { path: f.path })));
    for (const s of reply.snapshots.slice(0, 5)) message.append(button(`Restore ${s.rel_path} · ${new Date(s.created_at/1000).toLocaleDateString()}`, () => exportSnapshot(s.snapshot_id, s.rel_path)));
    if (reply.matches.length > 5 || reply.snapshots.length > 5) message.append(element('small', 'Showing the first 5 of each source. Refine your query to narrow results.'));
    message.append(element('p', reply.guidance));
    message.append(button(`Search for “${reply.query}”`, async () => { await runScan(reply.query); }));
    $('chat-log').scrollTop = $('chat-log').scrollHeight;
  } catch(e) {
    chat('assistant', String(e?.message || e));
  } finally {
    $('chat-send').disabled = false;
  }
};

// ----------------------------------------------------
// 0 MB VSS Snapshots Integration
// ----------------------------------------------------
async function refreshVssList() {
  const container = $('vss-list');
  try {
    const copies = await invoke('vss_list');
    container.replaceChildren();
    if (!copies.length) {
      container.append(element('p', 'No shadow copies found on this system.', 'helper'));
      return;
    }
    for (const s of copies) {
      const item = element('div', undefined, 'vss-item');
      const info = element('div');
      info.append(
        element('strong', `${s.volume} Snapshot`),
        element('div', `${new Date(s.creation_time).toLocaleString()} · ID: ${s.id.slice(0, 8)}`, 'helper')
      );
      const restoreBtn = button('Restore File', async () => {
        let dest = $('destination').value.trim();
        if (!dest) dest = await invoke('select_folder');
        if (!dest) return;
        const rel = prompt('Enter relative path inside shadow copy to restore (e.g. Users\\Haaris\\Documents\\file.txt):');
        if (!rel) return;
        try {
          await invoke('vss_restore', { deviceObject: s.device_object, relativePath: rel, destination: dest });
          alert(`Successfully restored ${rel} to ${dest}`);
        } catch(e) {
          alert('VSS restore error: ' + (e.message || e));
        }
      });
      item.append(info, restoreBtn);
      container.append(item);
    }
  } catch(e) {
    container.replaceChildren(element('p', 'VSS Access: ' + (e.message || e) + ' (Requires Administrator privileges)', 'helper'));
  }
}
$('vss-refresh').onclick = refreshVssList;

$('vss-create').onclick = async () => {
  const volume = $('vss-volume').value;
  if (!volume) { alert('Select a volume first.'); return; }
  $('vss-create').disabled = true;
  $('vss-status').textContent = 'Requesting Windows Volume Shadow Copy Service…';
  try {
    const created = await invoke('vss_create', { volume });
    $('vss-status').textContent = `0 MB Snapshot created! ID: ${created.id.slice(0, 8)}`;
    await refreshVssList();
  } catch(e) {
    $('vss-status').textContent = 'Creation failed: ' + (e.message || e);
  } finally {
    $('vss-create').disabled = false;
  }
};

// ----------------------------------------------------
// SSD Shadow Miner Integration
// ----------------------------------------------------
$('miner-btn').onclick = async () => {
  const query = $('miner-query').value.trim();
  const volume = $('vss-volume').value || 'C:\\';
  $('miner-btn').disabled = true;
  const container = $('miner-results');
  container.replaceChildren(element('p', 'Mining Windows Volume Shadow copies for deleted files…', 'helper'));
  try {
    const files = await invoke('ssd_mine_shadows', { volume, query });
    container.replaceChildren();
    if (!files.length) {
      container.append(element('p', 'No matching files found in shadow copies.', 'helper'));
      return;
    }
    for (const f of files) {
      const item = element('div', undefined, 'mined-item');
      const details = element('div');
      details.append(
        element('div', f.name, 'file-name'),
        element('div', `${bytes(f.size_bytes)} · Shadow Date: ${new Date(f.shadow_creation).toLocaleDateString()} · ${f.relative_path}`, 'file-detail')
      );
      const restBtn = button('Restore', async () => {
        let dest = $('destination').value.trim();
        if (!dest) dest = await invoke('select_folder');
        if (!dest) return;
        try {
          await invoke('ssd_restore_mined_file', { file: f, destination: dest });
          alert(`Successfully extracted ${f.name} to ${dest}`);
        } catch(e) {
          alert('Extraction failed: ' + (e.message || e));
        }
      });
      item.append(details, restBtn);
      container.append(item);
    }
  } catch(e) {
    container.replaceChildren(element('p', 'Mining error: ' + (e.message || e), 'helper'));
  } finally {
    $('miner-btn').disabled = false;
  }
};

// ----------------------------------------------------
// Incremental Time Capsule Backups (ZSTD Compressed)
// ----------------------------------------------------
async function refreshSnapshots() {
  try {
    const [settings, folders] = await Promise.all([invoke('capsule_policy'), invoke('time_capsule_list')]);
    $('snapshot-interval').value = String(settings.policy.interval_minutes);
    $('snapshot-budget').value = settings.policy.max_storage_bytes / 1024 ** 3;
    $('snapshot-keep').value = settings.policy.keep_versions;
    $('snapshot-prune').checked = settings.policy.prune_old_versions;
    $('storage-info').textContent = `${bytes(settings.used_bytes)} used (ZSTD Compressed) · ${bytes(settings.free_bytes)} free on disk · ${settings.storage_dir}`;
    $('snapshot-folders').replaceChildren();
    if (!folders.length) $('snapshot-folders').append(element('p', 'No folders added to archive yet.', 'helper'));
    for (const f of folders) {
      const card = element('div', undefined, 'snap-folder'), actions = element('div', undefined, 'input-row');
      card.append(element('h3', f.name), element('p', f.path, 'helper'), element('p', `${f.file_count} files · ${bytes(f.total_bytes)} · ${typeof f.status === 'string' ? f.status : f.status.Error}`, 'helper'));
      card.append(element('p', f.last_snapshot ? `Last checked ${new Date(f.last_snapshot / 1000).toLocaleString()}` : 'No successful capture yet', 'helper'));
      actions.append(
        button('Capture now', async () => {
          await backupJob(() => invoke('time_capsule_snapshot', { folderPath: f.path }));
          $('snapshot-status').textContent = 'Capture checked. Deduplicated & compressed with ZSTD.';
          await refreshSnapshots();
        }),
        button(f.status === 'Paused' ? 'Resume' : 'Pause', async () => {
          await invoke('capsule_pause', { folderPath: f.path, paused: f.status !== 'Paused' });
          await refreshSnapshots();
        }),
        button('Versions', async () => {
          const old = card.querySelector('.versions');
          if (old) { old.remove(); return; }
          const history = await invoke('time_capsule_history', { folderPath: f.path });
          const list = element('div', undefined, 'versions');
          for (const s of history.slice(0, 50)) {
            const row = element('div', undefined, 'snapshot-version');
            row.append(element('span', `${new Date(s.created_at / 1000).toLocaleString()} · ${s.entries.length} files`), button('Export copy', () => exportSnapshot(s.id)));
            list.append(row);
          }
          if (history.length > 50) list.append(element('p', 'Showing newest 50 versions.', 'helper'));
          card.append(list);
        })
      );
      card.append(actions);
      $('snapshot-folders').append(card);
    }
  } catch(e) {
    $('snapshot-status').textContent = String(e?.message || e);
  }
}

$('save-policy').onclick = async () => {
  const policy = {
    interval_minutes: Number($('snapshot-interval').value),
    max_storage_bytes: Math.round(Number($('snapshot-budget').value) * 1024 ** 3),
    keep_versions: Number($('snapshot-keep').value),
    prune_old_versions: $('snapshot-prune').checked
  };
  $('save-policy').disabled = true;
  try {
    await invoke('capsule_policy', { policy });
    $('snapshot-status').textContent = 'Settings saved. Retention applies after a new changed snapshot.';
    await refreshSnapshots();
  } catch(e) {
    $('snapshot-status').textContent = String(e);
  } finally {
    $('save-policy').disabled = false;
  }
};

$('protect').onclick = async () => {
  try {
    const path = await invoke('select_folder');
    if (!path) return;
    await backupJob(async () => {
      $('snapshot-status').textContent = 'Measuring folder...';
      const estimate = await invoke('capsule_estimate', { path });
      $('snapshot-status').textContent = `${estimate.files} files, ${bytes(estimate.total_bytes)} to read. Creating baseline with ZSTD compression...`;
      await invoke('time_capsule_protect', { path, name: path.split(/[\\/]/).filter(Boolean).pop() });
    });
    $('snapshot-status').textContent = 'Baseline saved with ZSTD compression. Scheduled captures are enabled.';
    await refreshSnapshots();
  } catch(e) {
    $('snapshot-status').textContent = String(e);
  }
};

async function previewFile(file) {
  const data = await invoke('recovery_preview', { fileId: file.id });
  $('preview-title').textContent = file.name;
  $('preview-meta').textContent = `${file.origin} · ${bytes(file.size_bytes)} · ${file.integrity}`;
  $('preview-content').replaceChildren();
  if (data.image) {
    const img = document.createElement('img');
    img.alt = file.name;
    img.src = data.image;
    $('preview-content').append(img);
  } else {
    $('preview-content').append(element('pre', data.text || data.message));
    if (data.text) $('preview-content').append(element('p', data.message, 'helper'));
  }
  $('preview-locate').onclick = () => invoke('open_in_explorer', { path: file.path }).catch(showError);
  $('preview-dialog').showModal();
}
$('preview-close').onclick = () => $('preview-dialog').close();

let backupBusy = false;
async function backupJob(action) {
  if (backupBusy || scanning) throw new Error('A disk operation is already running. Wait or stop it first.');
  backupBusy = true;
  $('backup-stop').disabled = false;
  for (const id of ['scan','quick-scan','demo','protect','backup-location','backup-open','backup-audit','backup-cleanup','save-policy']) {
    const el = $(id);
    if (el) el.disabled = true;
  }
  try { return await action(); }
  finally {
    backupBusy = false;
    $('backup-stop').disabled = true;
    for (const id of ['scan','quick-scan','demo','protect','backup-location','backup-open','backup-audit','backup-cleanup','save-policy']) {
      const el = $(id);
      if (el) el.disabled = false;
    }
  }
}

if (native?.event?.listen) {
  native.event.listen('snapshot-progress', e => {
    const p = e.payload;
    $('backup-progress').textContent = `${p.phase} · ${p.files} files · ${bytes(p.bytes_processed)} processed · ${bytes(p.bytes_stored)} written (ZSTD)`;
  }).catch(showError);
}

$('backup-stop').onclick = () => invoke('recovery_cancel').then(() => { $('backup-progress').textContent = 'Stopping safely...'; }).catch(showError);

for (const [id, cleanup] of [['backup-audit', false], ['backup-cleanup', true]]) {
  $(id).onclick = async () => {
    try {
      $('snapshot-status').textContent = cleanup ? 'Checking references and cleaning unused chunks...' : 'Verifying every referenced chunk...';
      const r = await backupJob(() => invoke('capsule_maintenance', { cleanup }));
      $('snapshot-status').textContent = cleanup ? `Reclaimed ${bytes(r.bytes)} in ${r.objects} unused chunks.` : `Verified ${r.objects} chunks (${bytes(r.bytes)}). ${r.damaged.length} damaged chunks.`;
      await refreshSnapshots();
    } catch(e) {
      $('snapshot-status').textContent = String(e);
    }
  };
}

for (const [id, existing] of [['backup-location', false], ['backup-open', true]]) {
  $(id).onclick = async () => {
    try {
      const path = await invoke('select_folder');
      if (!path) return;
      $('snapshot-status').textContent = existing ? 'Opening repository...' : 'Verifying and copying backup repository...';
      const location = await backupJob(() => invoke('capsule_location', { path, existing }));
      $('snapshot-status').textContent = `Backup location saved: ${location}.`;
      await refreshSnapshots();
    } catch(e) {
      $('snapshot-status').textContent = String(e);
    }
  };
}

document.addEventListener('visibilitychange', () => document.body.classList.toggle('document-hidden', document.hidden));

// Initialize workspace
setMode('drive');
checkAdmin();
refreshDrives();
refreshSnapshots();
refreshVssList();
