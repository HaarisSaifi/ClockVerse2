import { serve } from "https://deno.land/std@0.168.0/http/server.ts";
import { createClient } from "https://esm.sh/@supabase/supabase-js@2";

const SUPABASE_URL = Deno.env.get('SUPABASE_URL')!;
const SUPABASE_SERVICE_ROLE_KEY = Deno.env.get('SUPABASE_SERVICE_ROLE_KEY')!;

serve(async (req: Request) => {
  if (req.method === 'OPTIONS') {
    return new Response('ok', {
      headers: {
        'Access-Control-Allow-Origin': '*',
        'Access-Control-Allow-Methods': 'POST',
        'Access-Control-Allow-Headers': 'Content-Type, Authorization'
      }
    });
  }

  let payload;
  try {
    payload = await req.json();
  } catch (_) {
    return new Response(JSON.stringify({ valid: false, error: 'Invalid JSON payload' }), {
      status: 400,
      headers: { 'Content-Type': 'application/json' }
    });
  }

  const { license_key, machine_id } = payload;
  if (!license_key || !machine_id) {
    return new Response(JSON.stringify({ valid: false, error: 'Missing license_key or machine_id' }), {
      status: 400,
      headers: { 'Content-Type': 'application/json' }
    });
  }

  const supabase = createClient(SUPABASE_URL, SUPABASE_SERVICE_ROLE_KEY);

  const { data: license, error } = await supabase
    .from('licenses')
    .select('*')
    .eq('license_key', license_key)
    .single();

  if (error || !license) {
    return new Response(JSON.stringify({ valid: false, error: 'Invalid license key' }), {
      headers: { 'Content-Type': 'application/json' }
    });
  }

  if (license.revoked_at) {
    return new Response(JSON.stringify({ valid: false, error: 'License has been revoked' }), {
      headers: { 'Content-Type': 'application/json' }
    });
  }

  if (license.expires_at && new Date(license.expires_at) < new Date()) {
    return new Response(JSON.stringify({ valid: false, error: 'License has expired' }), {
      headers: { 'Content-Type': 'application/json' }
    });
  }

  const maxDevices = license.tier === 'studio' ? 3 : 1;

  const { data: activations } = await supabase
    .from('activations')
    .select('*')
    .eq('license_id', license.id)
    .eq('machine_id', machine_id);

  const { count } = await supabase
    .from('activations')
    .select('*', { count: 'exact', head: true })
    .eq('license_id', license.id);

  if (activations && activations.length > 0) {
    await supabase
      .from('activations')
      .update({ last_seen_at: new Date().toISOString() })
      .eq('license_id', license.id)
      .eq('machine_id', machine_id);

    return new Response(JSON.stringify({
      valid: true,
      tier: license.tier,
      activated: true,
      expires_at: license.expires_at
    }), {
      headers: { 'Content-Type': 'application/json' }
    });
  }

  if ((count || 0) >= maxDevices) {
    return new Response(JSON.stringify({
      valid: false,
      error: `Device limit reached (${maxDevices} max device allowed for ${license.tier.toUpperCase()})`
    }), {
      headers: { 'Content-Type': 'application/json' }
    });
  }

  await supabase
    .from('activations')
    .insert({ license_id: license.id, machine_id: machine_id });

  if (!license.activated_at) {
    await supabase
      .from('licenses')
      .update({ activated_at: new Date().toISOString(), machine_id: machine_id })
      .eq('id', license.id);
  }

  return new Response(JSON.stringify({
    valid: true,
    tier: license.tier,
    activated: true,
    expires_at: license.expires_at
  }), {
    headers: { 'Content-Type': 'application/json' }
  });
});
