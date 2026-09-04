#!/bin/bash

# Configuration
SUPABASE_URL="${SUPABASE_URL:-https://your-project.supabase.co}"
ANON_KEY="${ANON_KEY:-your-anon-key-here}"
WEBHOOK_SECRET="${WEBHOOK_SECRET:-whsec_test123456789}"

echo "🧪 Testing ClockVerse Payment Flow..."

# 1. Generate signature for Pro license
echo "📝 Generating test signature..."
SIGNATURE=$(python generate_test_signature.py test-pro-license.json $WEBHOOK_SECRET | grep "Signature:" | cut -d' ' -f2)

# 2. Test webhook
echo "💳 Testing Razorpay webhook..."
RESPONSE=$(curl -s -X POST $SUPABASE_URL/functions/v1/razorpay-webhook \
  -H "Content-Type: application/json" \
  -H "x-razorpay-signature: $SIGNATURE" \
  -d @test-pro-license.json)

echo "Response: $RESPONSE"

# 3. Extract license key
LICENSE_KEY=$(echo $RESPONSE | grep -o '"license_key":"[^"]*"' | cut -d'"' -f4)
echo "🔑 License Key: $LICENSE_KEY"

# 4. Validate license
echo "✅ Validating license..."
VALIDATION=$(curl -s -X POST $SUPABASE_URL/functions/v1/validate-license \
  -H "Authorization: Bearer $ANON_KEY" \
  -H "Content-Type: application/json" \
  -d "{\"license_key\":\"$LICENSE_KEY\",\"machine_id\":\"test-machine-$(date +%s)\"}")

echo "Validation: $VALIDATION"

echo "✨ Test complete!"
