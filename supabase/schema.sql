-- ClockVerse 2.0 Monetization & Licensing Schema
-- Run this in Supabase SQL Editor

-- 1. Licenses table
CREATE TABLE IF NOT EXISTS licenses (
    id UUID DEFAULT gen_random_uuid() PRIMARY KEY,
    license_key TEXT UNIQUE NOT NULL,
    tier TEXT NOT NULL CHECK (tier IN ('rescue_pass', 'pro', 'studio')),
    machine_id TEXT,
    activated_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    revoked_at TIMESTAMPTZ,
    razorpay_order_id TEXT UNIQUE,
    razorpay_payment_id TEXT,
    customer_email TEXT,
    metadata JSONB DEFAULT '{}'::jsonb
);

-- 2. Machine activations (device binding)
CREATE TABLE IF NOT EXISTS activations (
    id UUID DEFAULT gen_random_uuid() PRIMARY KEY,
    license_id UUID REFERENCES licenses(id) ON DELETE CASCADE,
    machine_id TEXT NOT NULL,
    activated_at TIMESTAMPTZ DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(license_id, machine_id)
);

-- 3. Payment logs (audit trail for webhook events)
CREATE TABLE IF NOT EXISTS payment_logs (
    id UUID DEFAULT gen_random_uuid() PRIMARY KEY,
    razorpay_event_id TEXT UNIQUE,
    event_type TEXT,
    payload JSONB,
    processed_at TIMESTAMPTZ DEFAULT NOW(),
    status TEXT DEFAULT 'success'
);

-- 4. Indexes for fast lookup
CREATE INDEX IF NOT EXISTS idx_licenses_key ON licenses(license_key);
CREATE INDEX IF NOT EXISTS idx_licenses_email ON licenses(customer_email);
CREATE INDEX IF NOT EXISTS idx_activations_machine ON activations(machine_id);

-- 5. Row Level Security (RLS) policies
ALTER TABLE licenses ENABLE ROW LEVEL SECURITY;
ALTER TABLE activations ENABLE ROW LEVEL SECURITY;
ALTER TABLE payment_logs ENABLE ROW LEVEL SECURITY;

-- Allow service_role to manage all tables
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_policies WHERE tablename = 'licenses' AND policyname = 'service_role_all_licenses'
    ) THEN
        CREATE POLICY service_role_all_licenses ON licenses FOR ALL TO service_role USING (true) WITH CHECK (true);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_policies WHERE tablename = 'activations' AND policyname = 'service_role_all_activations'
    ) THEN
        CREATE POLICY service_role_all_activations ON activations FOR ALL TO service_role USING (true) WITH CHECK (true);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_policies WHERE tablename = 'payment_logs' AND policyname = 'service_role_all_payment_logs'
    ) THEN
        CREATE POLICY service_role_all_payment_logs ON payment_logs FOR ALL TO service_role USING (true) WITH CHECK (true);
    END IF;
END $$;
