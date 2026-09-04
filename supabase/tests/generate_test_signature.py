import hmac
import hashlib
import sys

def generate_signature(payload_file, secret):
    with open(payload_file, 'r', encoding='utf-8') as f:
        body = f.read()

    expected_signature = hmac.new(
        secret.encode('utf-8'),
        body.encode('utf-8'),
        hashlib.sha256
    ).hexdigest()

    print(f"File: {payload_file}")
    print(f"Signature: {expected_signature}")
    print("\nCurl command:")
    print("curl -X POST https://your-project.supabase.co/functions/v1/razorpay-webhook \\")
    print(f'  -H "Content-Type: application/json" \\')
    print(f'  -H "x-razorpay-signature: {expected_signature}" \\')
    print(f'  -d @{payload_file}')

    return expected_signature

if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("Usage: python generate_test_signature.py <payload.json> <webhook_secret>")
        print("Example: python generate_test_signature.py test-pro-license.json whsec_test123456789")
        sys.exit(1)

    generate_signature(sys.argv[1], sys.argv[2])
