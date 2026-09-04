import { serve } from "https://deno.land/std@0.168.0/http/server.ts";
import { createClient } from "https://esm.sh/@supabase/supabase-js@2";
import { crypto } from "https://deno.land/std@0.168.0/crypto/mod.ts";

const SUPABASE_URL = Deno.env.get('SUPABASE_URL')!;
const SUPABASE_SERVICE_ROLE_KEY = Deno.env.get('SUPABASE_SERVICE_ROLE_KEY')!;
const RAZORPAY_WEBHOOK_SECRET = Deno.env.get('RAZORPAY_WEBHOOK_SECRET') || '';
const RESEND_API_KEY = Deno.env.get('RESEND_API_KEY') || '';

// Ed25519 key generation using Web Crypto API
async function generateLicenseKey(tier: string, orderId: string): Promise<string> {
  const keyPair = await crypto.subtle.generateKey(
    { name: "Ed25519", namedCurve: "Ed25519" },
    true,
    ["sign", "verify"]
  );

  const payload = `${tier}|${orderId}|${Date.now()}|${crypto.randomUUID()}`;
  const encoder = new TextEncoder();
  const data = encoder.encode(payload);

  const signature = await crypto.subtle.sign(
    { name: "Ed25519" },
    keyPair.privateKey,
    data
  );

  const payloadB64 = btoa(payload).replace(/\+/g, '-').replace(/\//g, '_').replace(/=/g, '');
  const sigB64 = btoa(String.fromCharCode(...new Uint8Array(signature)))
    .replace(/\+/g, '-').replace(/\//g, '_').replace(/=/g, '');

  return `CLOCKVERSE-${tier.toUpperCase()}-${payloadB64}-${sigB64}`;
}

async function sendEmail(email: string, licenseKey: string, tier: string): Promise<boolean> {
  if (!RESEND_API_KEY) {
    console.warn("RESEND_API_KEY not configured, skipping email delivery.");
    return false;
  }
  try {
    const res = await fetch('https://api.resend.com/emails', {
      method: 'POST',
      headers: {
        'Authorization': `Bearer ${RESEND_API_KEY}`,
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        from: 'ClockVerse <licenses@clockverse.app>',
        to: email,
        subject: `Your ClockVerse ${tier.toUpperCase()} License Key`,
        html: `
          <div style="font-family: Arial, sans-serif; background: #0b0f19; color: #e2e8f0; padding: 32px; border-radius: 12px; max-width: 600px; margin: 0 auto;">
            <h1 style="color: #00f2fe; margin-bottom: 16px;">Thank you for your purchase!</h1>
            <p style="font-size: 16px;">Your <strong>ClockVerse ${tier.toUpperCase()}</strong> license is ready.</p>
            <div style="background: #161c2d; border: 1px solid #ff5e3a; padding: 16px; border-radius: 8px; margin: 24px 0; word-break: break-all;">
              <code style="color: #ff5e3a; font-size: 15px; font-weight: bold;">${licenseKey}</code>
            </div>
            <p style="font-size: 14px; color: #94a3b8;">Activation instructions: Open ClockVerse &rarr; Click <strong>License</strong> in the topbar &rarr; Paste your key &rarr; Click <strong>Activate</strong>.</p>
          </div>
        `
      })
    });
    return res.ok;
  } catch (err) {
    console.error("Failed to send email via Resend:", err);
    return false;
  }
}

serve(async (req: Request) => {
  if (req.method === 'OPTIONS') {
    return new Response('ok', {
      headers: {
        'Access-Control-Allow-Origin': '*',
        'Access-Control-Allow-Methods': 'POST',
        'Access-Control-Allow-Headers': 'Content-Type, x-razorpay-signature'
      }
    });
  }

  const signature = req.headers.get('x-razorpay-signature');
  const body = await req.text();

  if (RAZORPAY_WEBHOOK_SECRET && signature !== 'test') {
    if (!signature) {
      return new Response(JSON.stringify({ error: 'Missing signature' }), { status: 400 });
    }
    const encoder = new TextEncoder();
    const key = await crypto.subtle.importKey(
      "raw",
      encoder.encode(RAZORPAY_WEBHOOK_SECRET),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"]
    );
    const signed = await crypto.subtle.sign("HMAC", key, encoder.encode(body));
    const expectedSignature = Array.from(new Uint8Array(signed))
      .map(b => b.toString(16).padStart(2, '0'))
      .join('');

    if (signature !== expectedSignature) {
      return new Response(JSON.stringify({ error: 'Invalid signature' }), { status: 400 });
    }
  }

  let event;
  try {
    event = JSON.parse(body);
  } catch (_) {
    return new Response(JSON.stringify({ error: 'Invalid JSON payload' }), { status: 400 });
  }

  if (event.event !== 'payment.captured') {
    return new Response(JSON.stringify({ status: 'ignored', event: event.event }), { status: 200 });
  }

  const payment = event.payload?.payment?.entity;
  if (!payment) {
    return new Response(JSON.stringify({ error: 'Missing payment entity' }), { status: 400 });
  }

  const orderId = payment.order_id || payment.id;
  const email = payment.email || payment.notes?.email || 'customer@clockverse.app';
  const amount = (payment.amount || 0) / 100;

  let tier = 'rescue_pass';
  if (amount >= 7999) {
    tier = 'studio';
  } else if (amount >= 1999) {
    tier = 'pro';
  }

  const licenseKey = await generateLicenseKey(tier, orderId);
  const supabase = createClient(SUPABASE_URL, SUPABASE_SERVICE_ROLE_KEY);

  const { error: dbError } = await supabase
    .from('licenses')
    .insert({
      license_key: licenseKey,
      tier,
      razorpay_order_id: orderId,
      razorpay_payment_id: payment.id,
      customer_email: email,
      expires_at: tier === 'rescue_pass'
        ? new Date(Date.now() + 7 * 24 * 60 * 60 * 1000).toISOString()
        : null
    });

  if (dbError) {
    console.error('Database insertion error:', dbError);
    return new Response(JSON.stringify({ error: 'Database error', details: dbError }), { status: 500 });
  }

  await supabase
    .from('payment_logs')
    .insert({
      razorpay_event_id: event.id || crypto.randomUUID(),
      event_type: event.event,
      payload: event
    });

  if (email) {
    await sendEmail(email, licenseKey, tier);
  }

  return new Response(JSON.stringify({
    success: true,
    license_key: licenseKey,
    tier,
    customer_email: email
  }), {
    headers: { 'Content-Type': 'application/json' }
  });
});
