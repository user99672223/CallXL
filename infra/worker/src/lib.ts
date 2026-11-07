/** Helpers for the CallXL Worker (tokens, TURN, envelopes). Kept out of the entry
 * module: workerd only accepts handlers and Durable Object classes as entry exports. */
export interface Env {
  LOBBY: DurableObjectNamespace;
  SESSION: DurableObjectNamespace;
  APP_SECRET: string;
  TOKEN_SIGNING_KEY: string;
  TURN_KEY_ID?: string;
  TURN_API_TOKEN?: string;
}

export const TOKEN_TTL_S = 3600;
export const STUN_URLS = ["stun:stun.cloudflare.com:3478", "stun:stun.l.google.com:19302"];

// ---------------------------------------------------------------------------- tokens

export const enc = new TextEncoder();

export function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function b64urlDecode(s: string): Uint8Array {
  const pad = s.length % 4 === 0 ? "" : "=".repeat(4 - (s.length % 4));
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + pad);
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

export async function hmac(key: string, data: string): Promise<Uint8Array> {
  const k = await crypto.subtle.importKey("raw", enc.encode(key), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await crypto.subtle.sign("HMAC", k, enc.encode(data)));
}

export function timingSafeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let d = 0;
  for (let i = 0; i < a.length; i++) d |= a[i] ^ b[i];
  return d === 0;
}

export async function mintToken(key: string, nowS: number, ttlS = TOKEN_TTL_S): Promise<string> {
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const body = b64url(enc.encode(JSON.stringify({ exp: nowS + ttlS, n: b64url(nonce) })));
  return `${body}.${b64url(await hmac(key, body))}`;
}

export async function verifyToken(key: string, token: string | null, nowS: number): Promise<boolean> {
  if (!token) return false;
  const [body, sig] = token.split(".");
  if (!body || !sig) return false;
  let given: Uint8Array;
  try {
    given = b64urlDecode(sig);
  } catch {
    return false;
  }
  if (!timingSafeEqual(given, await hmac(key, body))) return false;
  try {
    const claims = JSON.parse(new TextDecoder().decode(b64urlDecode(body)));
    return typeof claims.exp === "number" && claims.exp > nowS;
  } catch {
    return false;
  }
}

export async function secretMatches(given: unknown, expected: string): Promise<boolean> {
  if (typeof given !== "string" || !expected) return false;
  // Compare digests so the comparison is constant-time regardless of length.
  const [a, b] = await Promise.all([
    crypto.subtle.digest("SHA-256", enc.encode(given)),
    crypto.subtle.digest("SHA-256", enc.encode(expected)),
  ]);
  return timingSafeEqual(new Uint8Array(a), new Uint8Array(b));
}

export interface TurnCredentials {
  urls: string[];
  username: string;
  credential: string;
  ttl: number;
}

export async function mintTurn(env: Env): Promise<TurnCredentials | null> {
  if (!env.TURN_KEY_ID || !env.TURN_API_TOKEN) return null;
  const ttl = 86400;
  const r = await fetch(`https://rtc.live.cloudflare.com/v1/turn/keys/${env.TURN_KEY_ID}/credentials/generate`, {
    method: "POST",
    headers: { Authorization: `Bearer ${env.TURN_API_TOKEN}`, "Content-Type": "application/json" },
    body: JSON.stringify({ ttl }),
  });
  if (!r.ok) return null;
  const j = (await r.json()) as { iceServers?: { urls: string[] | string; username: string; credential: string } };
  const s = j.iceServers;
  if (!s) return null;
  const urls = (Array.isArray(s.urls) ? s.urls : [s.urls]).filter((u) => u.startsWith("turn"));
  return { urls, username: s.username, credential: s.credential, ttl };
}

// ---------------------------------------------------------------------------- envelopes

export interface Envelope {
  v: number;
  from: string;
  to?: string;
  seq: number;
  ts_ms: number;
  kind: string;
  sid?: string;
  fp?: string;
  payload: string; // base64
  sig: string; // base64
}

let serverSeq = 0;

export function serverEnvelope(kind: string, payload: unknown, sid?: string): string {
  const env: Envelope = {
    v: 1,
    from: "server",
    seq: ++serverSeq,
    ts_ms: Date.now(),
    kind,
    payload: btoa(String.fromCharCode(...enc.encode(JSON.stringify(payload)))),
    sig: "",
  };
  if (sid) env.sid = sid;
  return JSON.stringify(env);
}

export function parseEnvelope(raw: string | ArrayBuffer): { env: Envelope; text: string } | null {
  try {
    const text = typeof raw === "string" ? raw : new TextDecoder().decode(raw);
    const env = JSON.parse(text) as Envelope;
    if (env.v !== 1 || typeof env.from !== "string" || typeof env.kind !== "string") return null;
    return { env, text };
  } catch {
    return null;
  }
}

export function payloadJson(env: Envelope): any {
  try {
    const bin = atob(env.payload);
    return JSON.parse(new TextDecoder().decode(Uint8Array.from(bin, (c) => c.charCodeAt(0))));
  } catch {
    return {};
  }
}

export const ID_RE = /^[A-Za-z0-9_-]{1,64}$/;

export interface SessionInfo {
  id: string;
  name?: string | null;
  members: string[];
  created_ms: number;
}

