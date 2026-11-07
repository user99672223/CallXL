/**
 * CallXL cloud signaling (CLAUDE.md §4, §5, §12).
 *
 *   POST /token {app_secret}      → {token (HMAC-SHA256, 1 h), stun, turn, expires_in}
 *   GET  /ws?token=…               → Lobby DO (one global instance): session list fan-out,
 *                                    session_create / session_list / session_join / session_leave
 *   GET  /session/<id>/ws?token=…  → Session DO: relays peer-addressed envelopes by `to`,
 *                                    tracks membership; the session ends with its last member
 *   GET  /health                   → 200
 *
 * The server never decrypts anything: peer-addressed payloads are end-to-end encrypted
 * (X25519 + ChaCha20-Poly1305) and signed (Ed25519) by the clients. It reads only the
 * routing fields of the envelope JSON (`from`, `to`, `kind`, `sid`).
 */
import { TOKEN_TTL_S, STUN_URLS, mintToken, verifyToken, secretMatches, mintTurn, serverEnvelope, parseEnvelope, payloadJson, ID_RE } from "./lib";
import type { Env, SessionInfo } from "./lib";

// ---------------------------------------------------------------------------- Lobby DO

export class Lobby implements DurableObject {
  constructor(private state: DurableObjectState, private env: Env) {}

  private async sessions(): Promise<Map<string, SessionInfo>> {
    const m = await this.state.storage.list<SessionInfo>({ prefix: "s:" });
    const out = new Map<string, SessionInfo>();
    for (const [, v] of m) out.set(v.id, v);
    return out;
  }

  private async broadcast(): Promise<void> {
    const list = [...(await this.sessions()).values()].filter((s) => s.members.length > 0);
    const msg = serverEnvelope("sessions", { list });
    for (const ws of this.state.getWebSockets()) {
      try {
        ws.send(msg);
      } catch {
        /* closed */
      }
    }
  }

  async fetch(req: Request): Promise<Response> {
    const url = new URL(req.url);
    if (url.pathname === "/internal/members" && req.method === "POST") {
      const { id, members } = (await req.json()) as { id: string; members: string[] };
      const key = `s:${id}`;
      const cur = await this.state.storage.get<SessionInfo>(key);
      if (members.length === 0) {
        await this.state.storage.delete(key);
      } else {
        await this.state.storage.put(key, { id, name: cur?.name ?? null, members, created_ms: cur?.created_ms ?? Date.now() });
      }
      await this.broadcast();
      return new Response("ok");
    }
    if (url.pathname === "/internal/sessions") {
      return Response.json([...(await this.sessions()).values()]);
    }
    if (req.headers.get("Upgrade") !== "websocket") return new Response("expected websocket", { status: 426 });
    const pair = new WebSocketPair();
    this.state.acceptWebSocket(pair[1]);
    return new Response(null, { status: 101, webSocket: pair[0] });
  }

  async webSocketMessage(ws: WebSocket, raw: string | ArrayBuffer): Promise<void> {
    const p = parseEnvelope(raw);
    if (!p) return;
    const { env } = p;
    switch (env.kind) {
      case "session_list": {
        const list = [...(await this.sessions()).values()].filter((s) => s.members.length > 0);
        ws.send(serverEnvelope("sessions", { list }));
        break;
      }
      case "session_create": {
        const body = payloadJson(env);
        const id = String(body.id ?? env.sid ?? "");
        if (!ID_RE.test(id)) {
          ws.send(serverEnvelope("error", { reason: "invalid session id" }));
          return;
        }
        const key = `s:${id}`;
        const cur = await this.state.storage.get<SessionInfo>(key);
        const name = typeof body.name === "string" ? body.name.slice(0, 80) : null;
        // Listed once its first member connects to the session socket.
        await this.state.storage.put(key, { id, name: cur?.name ?? name, members: cur?.members ?? [], created_ms: cur?.created_ms ?? Date.now() });
        break;
      }
      // Membership is owned by the Session DO (the member's session socket);
      // join/leave on the lobby socket are accepted for protocol symmetry.
      case "session_join":
      case "session_leave":
        break;
      default:
        break;
    }
  }

  async webSocketClose(ws: WebSocket): Promise<void> {
    try {
      ws.close();
    } catch {
      /* already closed */
    }
  }
}

// ---------------------------------------------------------------------------- Session DO

interface Attachment {
  key?: string;
  sid: string;
}

export class Session implements DurableObject {
  constructor(private state: DurableObjectState, private env: Env) {}

  private socketsFor(key: string): WebSocket[] {
    return this.state.getWebSockets().filter((ws) => (ws.deserializeAttachment() as Attachment | null)?.key === key);
  }

  private members(): string[] {
    const keys = new Set<string>();
    for (const ws of this.state.getWebSockets()) {
      const a = ws.deserializeAttachment() as Attachment | null;
      if (a?.key) keys.add(a.key);
    }
    return [...keys];
  }

  private async report(sid: string): Promise<void> {
    const lobby = this.env.LOBBY.get(this.env.LOBBY.idFromName("global"));
    await lobby.fetch("https://lobby/internal/members", {
      method: "POST",
      body: JSON.stringify({ id: sid, members: this.members() }),
    });
  }

  async fetch(req: Request): Promise<Response> {
    const url = new URL(req.url);
    const sid = url.searchParams.get("sid") ?? "";
    if (url.pathname === "/internal/members") return Response.json(this.members());
    if (req.headers.get("Upgrade") !== "websocket") return new Response("expected websocket", { status: 426 });
    const pair = new WebSocketPair();
    this.state.acceptWebSocket(pair[1]);
    pair[1].serializeAttachment({ sid } satisfies Attachment);
    return new Response(null, { status: 101, webSocket: pair[0] });
  }

  async webSocketMessage(ws: WebSocket, raw: string | ArrayBuffer): Promise<void> {
    const p = parseEnvelope(raw);
    if (!p) return;
    const { env, text } = p;
    const att = (ws.deserializeAttachment() as Attachment | null) ?? { sid: "" };
    if (env.kind === "hello") {
      const first = !att.key;
      if (att.key && att.key !== env.from) return; // one identity per socket
      att.key = env.from;
      ws.serializeAttachment(att);
      await this.state.storage.put(`hello:${env.from}`, text);
      // Relay the (signed) hello to everyone else; replay theirs to the newcomer.
      for (const other of this.state.getWebSockets()) {
        if (other === ws) continue;
        try {
          other.send(text);
        } catch {
          /* closed */
        }
      }
      if (first) {
        for (const key of this.members()) {
          if (key === env.from) continue;
          const h = await this.state.storage.get<string>(`hello:${key}`);
          if (h) ws.send(h);
        }
        await this.report(att.sid);
      }
      return;
    }
    if (!att.key || env.from !== att.key) return; // must hello first; no spoofed `from`
    if (env.to) {
      for (const dst of this.socketsFor(env.to)) {
        try {
          dst.send(text);
        } catch {
          /* closed */
        }
      }
    }
  }

  private async drop(ws: WebSocket): Promise<void> {
    const att = ws.deserializeAttachment() as Attachment | null;
    try {
      ws.close();
    } catch {
      /* already closed */
    }
    if (!att?.key) return;
    const key = att.key;
    // Another socket of the same device may still be connected (reconnect overlap).
    const still = this.state.getWebSockets().some((o) => o !== ws && (o.deserializeAttachment() as Attachment | null)?.key === key);
    if (still) return;
    await this.state.storage.delete(`hello:${key}`);
    const msg = serverEnvelope("member_left", { session: att.sid, key }, att.sid);
    for (const other of this.state.getWebSockets()) {
      if (other === ws) continue;
      try {
        other.send(msg);
      } catch {
        /* closed */
      }
    }
    // The session ends when its last member leaves.
    if (this.members().filter((k) => k !== key).length === 0) await this.state.storage.deleteAll();
    await this.reportWithout(att.sid, key);
  }

  private async reportWithout(sid: string, gone: string): Promise<void> {
    const lobby = this.env.LOBBY.get(this.env.LOBBY.idFromName("global"));
    await lobby.fetch("https://lobby/internal/members", {
      method: "POST",
      body: JSON.stringify({ id: sid, members: this.members().filter((k) => k !== gone) }),
    });
  }

  async webSocketClose(ws: WebSocket): Promise<void> {
    await this.drop(ws);
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    await this.drop(ws);
  }
}

// ---------------------------------------------------------------------------- router

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    const now = Math.floor(Date.now() / 1000);
    if (url.pathname === "/health") return new Response("ok");

    if (url.pathname === "/token" && req.method === "POST") {
      let body: { app_secret?: unknown } = {};
      try {
        body = await req.json();
      } catch {
        return new Response("bad request", { status: 400 });
      }
      if (!(await secretMatches(body.app_secret, env.APP_SECRET))) return new Response("unauthorized", { status: 401 });
      const token = await mintToken(env.TOKEN_SIGNING_KEY, now);
      return Response.json({ token, stun: STUN_URLS, turn: await mintTurn(env), expires_in: TOKEN_TTL_S });
    }

    if (url.pathname === "/ws") {
      if (!(await verifyToken(env.TOKEN_SIGNING_KEY, url.searchParams.get("token"), now)))
        return new Response("unauthorized", { status: 401 });
      return env.LOBBY.get(env.LOBBY.idFromName("global")).fetch(req);
    }

    const m = url.pathname.match(/^\/session\/([A-Za-z0-9_-]{1,64})\/ws$/);
    if (m) {
      if (!(await verifyToken(env.TOKEN_SIGNING_KEY, url.searchParams.get("token"), now)))
        return new Response("unauthorized", { status: 401 });
      const fwd = new URL(req.url);
      fwd.searchParams.set("sid", m[1]);
      return env.SESSION.get(env.SESSION.idFromName(m[1])).fetch(new Request(fwd.toString(), req));
    }
    return new Response("not found", { status: 404 });
  },
};
