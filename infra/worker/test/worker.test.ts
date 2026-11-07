import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { mintToken, verifyToken } from "../src/lib";

const SECRET = "test-app-secret";
const KEY = "test-signing-key";

function b64(s: string): string {
  return btoa(s);
}

function envelope(from: string, kind: string, payload: unknown, extra: Record<string, unknown> = {}): string {
  return JSON.stringify({ v: 1, from, seq: 1, ts_ms: Date.now(), kind, payload: b64(JSON.stringify(payload)), sig: "", ...extra });
}

function decode(raw: string): any {
  const e = JSON.parse(raw);
  return { ...e, body: JSON.parse(atob(e.payload)) };
}

async function token(): Promise<string> {
  const r = await SELF.fetch("https://w/token", { method: "POST", body: JSON.stringify({ app_secret: SECRET }) });
  expect(r.status).toBe(200);
  return ((await r.json()) as { token: string }).token;
}

async function socket(path: string): Promise<{ ws: WebSocket; next: (ms?: number) => Promise<any> }> {
  const r = await SELF.fetch(`https://w${path}`, { headers: { Upgrade: "websocket" } });
  expect(r.status).toBe(101);
  const ws = r.webSocket!;
  ws.accept();
  const queue: string[] = [];
  const waiters: ((s: string) => void)[] = [];
  ws.addEventListener("message", (ev) => {
    const data = typeof ev.data === "string" ? ev.data : new TextDecoder().decode(ev.data as ArrayBuffer);
    const w = waiters.shift();
    if (w) w(data);
    else queue.push(data);
  });
  const next = (ms = 3000) =>
    new Promise<any>((resolve, reject) => {
      const q = queue.shift();
      if (q) return resolve(decode(q));
      const w = (s: string) => {
        clearTimeout(t);
        resolve(decode(s));
      };
      const t = setTimeout(() => {
        waiters.splice(waiters.indexOf(w), 1);
        reject(new Error("no message"));
      }, ms);
      waiters.push(w);
    });
  return { ws, next };
}

describe("tokens", () => {
  it("rejects a bad app secret with 401", async () => {
    const r = await SELF.fetch("https://w/token", { method: "POST", body: JSON.stringify({ app_secret: "nope" }) });
    expect(r.status).toBe(401);
  });

  it("mints a token with STUN defaults and no TURN without TURN secrets", async () => {
    const r = await SELF.fetch("https://w/token", { method: "POST", body: JSON.stringify({ app_secret: SECRET }) });
    const j = (await r.json()) as any;
    expect(j.token).toMatch(/^[\w-]+\.[\w-]+$/);
    expect(j.stun).toContain("stun:stun.cloudflare.com:3478");
    expect(j.turn).toBeNull();
    expect(j.expires_in).toBe(3600);
  });

  it("expires after one hour and rejects tampering", async () => {
    const now = 1_000_000;
    const t = await mintToken(KEY, now);
    expect(await verifyToken(KEY, t, now + 3599)).toBe(true);
    expect(await verifyToken(KEY, t, now + 3601)).toBe(false);
    expect(await verifyToken("other-key", t, now)).toBe(false);
    const [body, sig] = t.split(".");
    expect(await verifyToken(KEY, `${body}x.${sig}`, now)).toBe(false);
    expect(await verifyToken(KEY, null, now)).toBe(false);
  });

  it("refuses websockets without a valid token", async () => {
    const r = await SELF.fetch("https://w/ws?token=bogus", { headers: { Upgrade: "websocket" } });
    expect(r.status).toBe(401);
    const s = await SELF.fetch("https://w/session/abc/ws", { headers: { Upgrade: "websocket" } });
    expect(s.status).toBe(401);
  });
});

describe("session lifecycle", { timeout: 20000 }, () => {
  it("lists, relays by `to`, and deletes the session when the last member leaves", async () => {
    const t = await token();
    const lobby = await socket(`/ws?token=${t}`);
    lobby.ws.send(envelope("a".repeat(64), "session_list", {}));
    expect((await lobby.next()).body.list).toEqual([]);

    lobby.ws.send(envelope("a".repeat(64), "session_create", { id: "s1", name: "Kitchen" }));
    const alice = await socket(`/session/s1/ws?token=${t}`);
    alice.ws.send(envelope("a".repeat(64), "hello", { card: { name: "alice" } }, { sid: "s1" }));
    let listed = await lobby.next();
    expect(listed.kind).toBe("sessions");
    expect(listed.body.list[0]).toMatchObject({ id: "s1", name: "Kitchen", members: ["a".repeat(64)] });

    const bob = await socket(`/session/s1/ws?token=${t}`);
    bob.ws.send(envelope("b".repeat(64), "hello", { card: { name: "bob" } }, { sid: "s1" }));
    // Bob gets alice's stored hello, alice gets bob's.
    expect((await bob.next()).from).toBe("a".repeat(64));
    expect((await alice.next()).from).toBe("b".repeat(64));
    listed = await lobby.next();
    expect(listed.body.list[0].members.sort()).toEqual(["a".repeat(64), "b".repeat(64)]);

    // Peer-addressed relay; spoofed `from` is dropped.
    bob.ws.send(envelope("b".repeat(64), "offer", { x: 1 }, { to: "a".repeat(64), sid: "s1" }));
    const got = await alice.next();
    expect(got.kind).toBe("offer");
    expect(got.from).toBe("b".repeat(64));
    bob.ws.send(envelope("c".repeat(64), "offer", { x: 2 }, { to: "a".repeat(64) }));
    await expect(alice.next(300)).rejects.toThrow();

    bob.ws.close();
    const left = await alice.next();
    expect(left.kind).toBe("member_left");
    expect(left.body).toEqual({ session: "s1", key: "b".repeat(64) });
    listed = await lobby.next();
    expect(listed.body.list[0].members).toEqual(["a".repeat(64)]);

    alice.ws.close();
    listed = await lobby.next();
    expect(listed.body.list).toEqual([]);
  });
});
