#!/usr/bin/env python3
"""ClockVerse forensic sidecar.

Invisible background process. Rust spawns it and talks JSON-RPC
over stdin/stdout, one message per line. Heavy forensic libs
(pytsk3, pyewf, Pillow, ffmpeg bindings) live HERE, never in the UI.

Protocol (newline-delimited JSON):
  request : {"id": 1, "method": "image_info", "params": {"path": "..."}}
  response: {"id": 1, "result": {...}}  or  {"id": 1, "error": "..."}
"""
import json
import sys
import hashlib
import os

# Optional imports — graceful degradation agar libs missing hain
try:
    import pytsk3
    HAS_TSK = True
except ImportError:
    HAS_TSK = False

try:
    from PIL import Image
    HAS_PIL = True
except ImportError:
    HAS_PIL = False

def log(msg):  # diagnostics go to stderr — stdout is the protocol channel
    print(f"[sidecar] {msg}", file=sys.stderr, flush=True)

def method_image_info(path):
    st = os.stat(path)
    return {
        "path": path,
        "size_bytes": st.st_size,
        "readonly_ok": True,
        "modified_epoch": st.st_mtime
    }

def method_verify_file(path, expected_sha256=None):
    if not os.path.exists(path):
        raise FileNotFoundError(f"Path not found: {path}")
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):  # 1MB blocks
            h.update(block)
    digest = h.hexdigest()
    ok = (expected_sha256 is None) or (digest.lower() == expected_sha256.lower())
    return {"sha256": digest, "integrity_ok": ok}

def method_carve_thumbnail(carve_offset, image_path, out_path):
    """Extract a thumbnail from a carved file at the given offset."""
    if not HAS_PIL:
        return {"thumbnail": None, "status": "pillow_missing", "error": "Pillow not installed"}
    try:
        import io
        with open(image_path, "rb") as f:
            f.seek(carve_offset)
            header = f.read(1 << 20)  # 1 MB — header + some data
        img = Image.open(io.BytesIO(header))
        img.thumbnail((256, 256))
        os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
        img.save(out_path, "PNG")
        return {"thumbnail": out_path, "status": "ok", "size": list(img.size)}
    except Exception as e:
        return {"thumbnail": None, "status": "failed", "error": str(e)}

def method_tsk_list_partitions(image_path):
    """pytsk3 se partition table list karo (EWF/raw dono support)."""
    if not HAS_TSK:
        return {"partitions": [], "status": "tsk_missing", "error": "pytsk3 not installed"}
    try:
        img = pytsk3.Img_Info(image_path)
        vol = pytsk3.Volume_Info(img)
        parts = []
        for part in vol:
            parts.append({
                "slot": part.slot_num,
                "start_sector": part.start,
                "sector_count": part.len,
                "description": part.desc.decode("utf-8", errors="replace"),
                "flags": int(part.flags),
            })
        return {"partitions": parts, "status": "ok", "sector_size": vol.info.block_size}
    except Exception as e:
        return {"partitions": [], "status": "failed", "error": str(e)}

def method_tsk_list_files(image_path, partition_offset=0):
    """pytsk3 se deleted files list karo (alternative to our MFT parser)."""
    if not HAS_TSK:
        return {"files": [], "status": "tsk_missing"}
    try:
        img = pytsk3.Img_Info(image_path)
        fs = pytsk3.FS_Info(img, offset=partition_offset)
        root = fs.open_dir(path="/")
        deleted = []
        for entry in root:
            try:
                if entry.info.name.name in [b".", b".."]:
                    continue
                if entry.info.meta and entry.info.meta.flags & pytsk3.TSK_FS_META_FLAG_UNALLOC:
                    deleted.append({
                        "name": entry.info.name.name.decode("utf-8", errors="replace"),
                        "size": entry.info.meta.size,
                        "mtime": entry.info.meta.mtime,
                        "inode": entry.info.meta.addr,
                    })
            except Exception:
                continue
        return {"files": deleted, "status": "ok"}
    except Exception as e:
        return {"files": [], "status": "failed", "error": str(e)}

METHODS = {
    "image_info": lambda p: method_image_info(p["path"]),
    "verify_file": lambda p: method_verify_file(p["path"], p.get("expected_sha256")),
    "carve_thumbnail": lambda p: method_carve_thumbnail(
        p["carve_offset"], p["image_path"], p["out_path"]
    ),
    "tsk_list_partitions": lambda p: method_tsk_list_partitions(p["image_path"]),
    "tsk_list_files": lambda p: method_tsk_list_files(
        p["image_path"], p.get("partition_offset", 0)
    ),
}

def handle_line(line):
    line = line.strip()
    if not line:
        return None
    try:
        req = json.loads(line)
        method = req.get("method")
        handler = METHODS.get(method)
        if handler is None:
            raise ValueError(f"unknown method: {method}")
        result = handler(req.get("params", {}))
        return {"id": req.get("id"), "result": result}
    except Exception as e:
        req_id = None
        try:
            req_id = json.loads(line).get("id")
        except Exception:
            pass
        return {"id": req_id, "error": str(e)}

def main():
    log("sidecar up (tsk=%s, pil=%s)" % (HAS_TSK, HAS_PIL))
    for line in sys.stdin:
        out = handle_line(line)
        if out is not None:
            print(json.dumps(out), flush=True)

if __name__ == "__main__":
    main()
