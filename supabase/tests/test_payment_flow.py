import hmac
import hashlib
import json
import os
import sys
import time
import urllib.request
import urllib.error

def make_request(url, data_bytes, headers):
    req = urllib.request.Request(url, data=data_bytes, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req) as response:
            return response.getcode(), response.read().decode('utf-8')
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode('utf-8')
    except Exception as e:
        return 0, str(e)

def run_suite(supabase_url, anon_key, webhook_secret):
    print("=" * 60)
    print("ClockVerse Monetization Test Suite Runner")
    print(f"Target URL: {supabase_url}")
    print("=" * 60)

    base_dir = os.path.dirname(os.path.abspath(__file__))
    pro_file = os.path.join(base_dir, "test-pro-license.json")

    with open(pro_file, "r", encoding="utf-8") as f:
        pro_body = f.read()

    # 1. Compute HMAC SHA-256 signature
    signature = hmac.new(
        webhook_secret.encode('utf-8'),
        pro_body.encode('utf-8'),
        hashlib.sha256
    ).hexdigest()

    # 2. Test Razorpay Webhook
    webhook_url = f"{supabase_url}/functions/v1/razorpay-webhook"
    print("\n[Step 1] Invoking Razorpay Webhook with valid signature...")
    headers = {
        "Content-Type": "application/json",
        "x-razorpay-signature": signature
    }
    code, res_text = make_request(webhook_url, pro_body.encode('utf-8'), headers)
    print(f"Status: {code}")
    print(f"Response: {res_text}")

    if code != 200:
        print("[FAIL] Webhook did not return 200 OK")
        return

    try:
        res_json = json.loads(res_text)
        license_key = res_json.get("license_key")
        tier = res_json.get("tier")
        print(f"[SUCCESS] License generated: {license_key} (Tier: {tier})")
    except Exception as e:
        print(f"[FAIL] Could not parse webhook response: {e}")
        return

    # 3. Test License Validation (Primary Device)
    val_url = f"{supabase_url}/functions/v1/validate-license"
    machine_1 = f"machine-test-{int(time.time())}"
    print(f"\n[Step 2] Validating license on Primary Device: {machine_1}...")

    val_payload = json.dumps({
        "license_key": license_key,
        "machine_id": machine_1
    }).encode('utf-8')

    val_headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {anon_key}"
    }

    code, val_text = make_request(val_url, val_payload, val_headers)
    print(f"Status: {code}")
    print(f"Response: {val_text}")

    # 4. Test Device Limit (Secondary Device for Pro license)
    machine_2 = f"machine-second-{int(time.time())}"
    print(f"\n[Step 3] Attempting activation on Second Device (should exceed 1-device limit for Pro): {machine_2}...")

    val_payload_2 = json.dumps({
        "license_key": license_key,
        "machine_id": machine_2
    }).encode('utf-8')

    code2, val_text_2 = make_request(val_url, val_payload_2, val_headers)
    print(f"Status: {code2}")
    print(f"Response: {val_text_2}")

    print("\n" + "=" * 60)
    print("Verification Completed!")
    print("=" * 60)

if __name__ == "__main__":
    url = os.environ.get("SUPABASE_URL", "https://your-project.supabase.co")
    anon = os.environ.get("SUPABASE_ANON_KEY", "your-anon-key")
    sec = os.environ.get("RAZORPAY_WEBHOOK_SECRET", "whsec_test123456789")

    if len(sys.argv) >= 4:
        url = sys.argv[1]
        anon = sys.argv[2]
        sec = sys.argv[3]
    elif url == "https://your-project.supabase.co":
        print("Usage: python test_payment_flow.py <SUPABASE_URL> <SUPABASE_ANON_KEY> <RAZORPAY_WEBHOOK_SECRET>")
        print("Or set environment variables: SUPABASE_URL, SUPABASE_ANON_KEY, RAZORPAY_WEBHOOK_SECRET")
        sys.exit(1)

    run_suite(url, anon, sec)
