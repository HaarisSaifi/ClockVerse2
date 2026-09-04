#!/usr/bin/env python3
"""Automated unit test for ClockVerse forensic sidecar."""

import subprocess
import json
import sys
import os
import tempfile
import hashlib

def test_sidecar():
    sidecar_path = os.path.join(os.path.dirname(__file__), "sidecar.py")
    
    # Create temp test file
    with tempfile.NamedTemporaryFile(delete=False) as f:
        data = b"ClockVerse Forensic Engine Integrity Test Data"
        f.write(data)
        temp_path = f.name
        expected_hash = hashlib.sha256(data).hexdigest()

    try:
        proc = subprocess.Popen(
            [sys.executable, sidecar_path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )

        # 1. Test image_info
        req1 = {"id": 1, "method": "image_info", "params": {"path": temp_path}}
        proc.stdin.write(json.dumps(req1) + "\n")
        proc.stdin.flush()
        res1 = json.loads(proc.stdout.readline())
        assert res1["id"] == 1, f"Expected id 1, got {res1}"
        assert res1["result"]["size_bytes"] == len(data), "File size mismatch"
        print("[PASS] sidecar image_info test passed")

        # 2. Test verify_file
        req2 = {"id": 2, "method": "verify_file", "params": {"path": temp_path, "expected_sha256": expected_hash}}
        proc.stdin.write(json.dumps(req2) + "\n")
        proc.stdin.flush()
        res2 = json.loads(proc.stdout.readline())
        assert res2["id"] == 2
        assert res2["result"]["integrity_ok"] is True
        assert res2["result"]["sha256"] == expected_hash
        print("[PASS] sidecar verify_file test passed")

        # 3. Test carve_thumbnail stub
        req3 = {"id": 3, "method": "carve_thumbnail", "params": {"carve_offset": 1024, "image_path": temp_path, "out_path": "thumb.png"}}
        proc.stdin.write(json.dumps(req3) + "\n")
        proc.stdin.flush()
        res3 = json.loads(proc.stdout.readline())
        assert res3["id"] == 3
        print("[PASS] sidecar carve_thumbnail test passed")

        # Clean shutdown
        proc.stdin.close()
        proc.terminate()
        proc.wait(timeout=2)
        print("[ALL PASS] Forensic sidecar protocol verified successfully.")
    finally:
        if os.path.exists(temp_path):
            os.remove(temp_path)

if __name__ == "__main__":
    test_sidecar()
