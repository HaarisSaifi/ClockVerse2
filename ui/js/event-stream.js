// In-memory mock folders for browser dev
const mockCapsuleFolders = [];
// Tauri event bridge. In browser-dev mode, falls back to a mock feed
// so UI work never blocks on the Rust engine.
const isTauri = typeof window.__TAURI__ !== 'undefined';

let mockEmitter = null;

// Browser-dev mock state for chrono phase-1 commands
const mockStore = {
  files: new Map(), // path -> { bytes: string, last_ts, patch_count, op }
  eventCount: 0,
};

function mockApplyOp(path, op, ts) {
  let st = mockStore.files.get(path) || { bytes: '', last_ts: 0, patch_count: 0, op: 'write' };
  if (op.kind === 'write') {
    st.bytes = new TextDecoder().decode(new Uint8Array(op.content || []));
    st.op = 'write';
  } else if (op.kind === 'patch') {
    const arr = Array.from(st.bytes.split(''));
    const start = Math.min(op.offset || 0, arr.length);
    const end = Math.min(start + (op.delete_len || 0), arr.length);
    const ins = new TextDecoder().decode(new Uint8Array(op.insert || []));
    arr.splice(start, end - start, ins);
    st.bytes = arr.join('');
    st.patch_count = (st.patch_count || 0) + 1;
    st.op = 'patch';
  } else if (op.kind === 'delete') {
    st.bytes = '';
    st.patch_count = 0;
    st.op = 'delete';
  }
  st.last_ts = ts;
  mockStore.files.set(path, st);
}

export async function onEngineEvent(handler) {
  if (isTauri) {
    const { listen } = window.__TAURI__.event;
    return await listen('engine', (e) => handler(e.payload));
  }

  // Browser dev mode: hold reference to handler for mock invoke triggers
  mockEmitter = handler;

  // Initial welcome burst
  let i = 0;
  const id = setInterval(() => {
    handler({
      type: 'sector_result',
      particle_index: i++,
      state_code: 1,
      cluster: 1204556 + i,
      signature: i % 2 === 0 ? 'FFD8FF' : '89504E',
      confidence: 0.94
    });
    if (i > 60) clearInterval(id);
  }, 25);

  return () => clearInterval(id);
}

export async function invoke(cmd, args) {
  if (isTauri) return window.__TAURI__.core.invoke(cmd, args);
  console.log('[mock invoke]', cmd, args);

  if (cmd === 'start_scan' && mockEmitter) {
    mockEmitter({ type: 'scan_started', target: args.target || 'Disk0', total_sectors: 500000 });
    let count = 0;
    const interval = setInterval(() => {
      count++;
      const particleIdx = Math.floor(Math.random() * 200000);
      const state = Math.random() > 0.4 ? 2 : 1; // 1 = carved, 2 = verified
      mockEmitter({
        type: 'sector_result',
        particle_index: particleIdx,
        state_code: state,
        cluster: 2048000 + count * 8,
        signature: 'FFD8FFE0',
        confidence: 0.96
      });
      if (count % 3 === 0) {
        mockEmitter({ type: 'file_restored', path: `/restored/image_${count}.jpg`, bytes: 40960 });
      }
      if (count >= 150) {
        clearInterval(interval);
        mockEmitter({ type: 'scan_complete', found: count, verified: Math.floor(count * 0.8), failures: 0 });
      }
    }, 20);
  }

  // --- Phase 1: chrono commands (browser-dev mocks) ---
  if (cmd === 'chrono_ingest') {
    const lines = String(args.jsonl || '').split(/\r?\n/).filter(Boolean);
    for (const line of lines) {
      try {
        const ev = JSON.parse(line);
        mockApplyOp(ev.file_path, ev.op, ev.ts_micros);
        mockStore.eventCount++;
        if (mockEmitter) {
          mockEmitter({
            type: 'chrono_event_ingested',
            path: ev.file_path,
            ts_micros: ev.ts_micros,
            op_kind: ev.op.kind || 'unknown'
          });
        }
      } catch (_) { /* skip corrupt line */ }
    }
    const files = Array.from(mockStore.files.keys());
    if (mockEmitter) mockEmitter({ type: 'session_updated', session_id: 'active', event_count: mockStore.eventCount, file_count: files.length });
    return `indexed ${lines.length} lines; total events now ${mockStore.eventCount}`;
  }

  if (cmd === 'session_summary') {
    return {
      session_id: 'active',
      event_count: mockStore.eventCount,
      file_count: mockStore.files.size,
      max_ts_micros: Math.max(0, ...Array.from(mockStore.files.values()).map(v => v.last_ts)),
      files: Array.from(mockStore.files.keys()),
    };
  }

    if (cmd === 'get_temp_dir') {
    return '/tmp/';
  }

  
  if (cmd === 'time_capsule_list') {
    return mockCapsuleFolders;
  }
  if (cmd === 'time_capsule_protect') {
    const f = {
      path: args.path,
      name: args.name || args.path.split(/[\/]/).pop(),
      added_at: Date.now() * 1000,
      last_snapshot: Date.now() * 1000,
      file_count: 12,
      total_bytes: 1024 * 1024 * 5,
      status: 'Active'
    };
    mockCapsuleFolders.push(f);
    return f;
  }
  if (cmd === 'time_capsule_snapshot') {
    const found = mockCapsuleFolders.find(f => f.path === args.folderPath);
    if (found) found.last_snapshot = Date.now() * 1000;
    return Date.now() * 1000;
  }
  if (cmd === 'select_folder') {
    return 'D:\\Projects\\ClockVerse';
  }

  if (cmd === 'select_image_file') {
    return null;
  }
  if (cmd === 'chrono_time_travel') {
    const out = {};
    for (const [k, v] of mockStore.files) {
      if (v.last_ts <= args.as_of_micros) out[k] = { bytes: v.bytes, last_ts: v.last_ts, patch_count: v.patch_count };
    }
    return { as_of_micros: args.as_of_micros, files: out };
  }
}
