// JSON-RPC client for imperium-core over a WebSocket. Browsers cannot set an Authorization header
// on WebSocket requests, so the token travels as the `imperium.token.<token>` subprotocol.

import type {
  ErrorObject,
  HandshakeParams,
  HandshakeResult,
  Notification,
  Response,
} from "@imperium/protocol";

export type DaemonConnection = { url: string; token: string };

export class RpcError extends Error {
  readonly code: number;
  /** The error's `data`, such as the budget that stopped a turn (`-32008`). */
  readonly data: unknown;
  constructor(error: ErrorObject) {
    super(error.message);
    this.code = error.code;
    this.data = error.data;
  }
}

type Pending = { resolve: (value: unknown) => void; reject: (error: Error) => void };

export class DaemonClient {
  private readonly socket: WebSocket;
  private readonly pending = new Map<number, Pending>();
  private readonly listeners = new Set<(notification: Notification) => void>();
  private nextId = 1;
  daemon: HandshakeResult | null = null;

  private constructor(socket: WebSocket) {
    this.socket = socket;
    socket.addEventListener("message", (event) => this.receive(String(event.data)));
    socket.addEventListener("close", () => {
      for (const { reject } of this.pending.values()) reject(new Error("imperium-core closed the connection"));
      this.pending.clear();
    });
  }

  /** Opens the socket and performs `system.handshake`. */
  static async connect(connection: DaemonConnection): Promise<DaemonClient> {
    const socket = new WebSocket(connection.url, ["imperium.v0", `imperium.token.${connection.token}`]);
    await new Promise<void>((resolve, reject) => {
      socket.addEventListener("open", () => resolve(), { once: true });
      socket.addEventListener("error", () => reject(new Error(`could not connect to ${connection.url}`)), {
        once: true,
      });
    });
    const client = new DaemonClient(socket);
    const params: HandshakeParams = {
      client: { name: "imperium-desktop", version: "0.1.0" },
      protocol: { major: 0, minor: 21 },
    };
    client.daemon = await client.call<HandshakeResult>("system.handshake", params);
    return client;
  }

  call<Result>(method: string, params: unknown = {}): Promise<Result> {
    const id = this.nextId++;
    return new Promise<Result>((resolve, reject) => {
      this.pending.set(id, { resolve: (value) => resolve(value as Result), reject });
      this.socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  }

  /** Subscribes to server notifications; returns an unsubscribe function. */
  onNotification(listener: (notification: Notification) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  onClose(listener: () => void): void {
    this.socket.addEventListener("close", listener, { once: true });
  }

  close(): void {
    this.socket.close();
  }

  private receive(text: string): void {
    const message = JSON.parse(text) as Partial<Response & Notification>;
    if (typeof message.id === "number" && this.pending.has(message.id)) {
      const pending = this.pending.get(message.id)!;
      this.pending.delete(message.id);
      const response = message as Response;
      if ("error" in response) pending.reject(new RpcError(response.error));
      else pending.resolve(response.result);
      return;
    }
    if (typeof message.method === "string") {
      for (const listener of this.listeners) listener(message as Notification);
    }
  }
}
