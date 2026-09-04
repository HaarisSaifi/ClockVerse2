# 💳 ClockVerse 2.0 Monetization & Licensing Engine

This directory contains the production-ready database schema and Supabase Edge Functions for handling Razorpay payments, generating Ed25519-signed licenses, and enforcing hardware-bound machine activations.

---

## 📁 Architecture Overview

- **`schema.sql`**: PostgreSQL schema with `licenses`, `activations`, and `payment_logs` tables + Row Level Security (RLS) policies.
- **`functions/razorpay-webhook/index.ts`**: Webhook endpoint listening for `payment.captured` events, verifying HMAC SHA-256 signatures, generating Ed25519-signed license keys, and sending license emails via Resend.
- **`functions/validate-license/index.ts`**: High-performance license validator enforcing device binding limits (1 machine for Rescue Pass / Pro, 3 machines for Studio) and expiry checking.

---

## 🚀 Setup & Deployment Guide

### 1. Database Setup
1. Open your project on [Supabase Dashboard](https://supabase.com/).
2. Navigate to **SQL Editor** -> Click **New Query**.
3. Paste the contents of `supabase/schema.sql` and click **Run**.

---

### 2. Edge Function Secrets Configuration
In your Supabase project, go to **Settings** -> **Edge Functions** (or run via Supabase CLI):

```bash
# Set secrets via CLI
supabase secrets set RAZORPAY_WEBHOOK_SECRET="your_razorpay_webhook_secret"
supabase secrets set RESEND_API_KEY="your_resend_api_key"
```

*(Note: `SUPABASE_URL` and `SUPABASE_SERVICE_ROLE_KEY` are automatically injected by Supabase into Edge Functions).*

---

### 3. Deploy Functions

Deploy the two edge functions using the Supabase CLI:

```bash
supabase functions deploy razorpay-webhook
supabase functions deploy validate-license
```

Once deployed, your endpoints will be:
- `https://<YOUR_PROJECT_REF>.supabase.co/functions/v1/razorpay-webhook`
- `https://<YOUR_PROJECT_REF>.supabase.co/functions/v1/validate-license`

---

### 4. Razorpay Dashboard Webhook Configuration

1. In Razorpay Dashboard -> **Settings** -> **Webhooks** -> Click **Add New Webhook**.
2. **Webhook URL**: `https://<YOUR_PROJECT_REF>.supabase.co/functions/v1/razorpay-webhook`
3. **Secret**: Enter the exact secret configured in `RAZORPAY_WEBHOOK_SECRET`.
4. **Alert Email**: Your operational notification email.
5. **Active Events**: Check `payment.captured`.
6. Click **Save**.

---

### 5. Verification & Testing

#### A. Test Webhook (Simulated Payment)
```bash
curl -X POST https://<YOUR_PROJECT_REF>.supabase.co/functions/v1/razorpay-webhook \
  -H "Content-Type: application/json" \
  -H "x-razorpay-signature: test" \
  -d "{\"event\":\"payment.captured\",\"payload\":{\"payment\":{\"entity\":{\"id\":\"pay_123\",\"order_id\":\"order_123\",\"amount\":199900,\"email\":\"test@example.com\"}}}}"
```

#### B. Test License Validation
```bash
curl -X POST https://<YOUR_PROJECT_REF>.supabase.co/functions/v1/validate-license \
  -H "Authorization: Bearer <YOUR_SUPABASE_ANON_KEY>" \
  -H "Content-Type: application/json" \
  -d "{\"license_key\":\"CLOCKVERSE-PRO-12345\",\"machine_id\":\"machine-test-uuid\"}"
```
