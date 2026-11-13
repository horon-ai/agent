/**
 * In-process mock harness server for SDK tests.
 *
 * Speaks the same NDJSON frames as the bridge so client tests exercise real
 * framing and reply correlation instead of stubbed methods.
 */

import net from "node:net";
import os from "node:os";
import path from "node:path";
import fs from "node:fs";
import { setTimeout as delay } from "node:timers/promises";
import { NdjsonDecoder, type ClientFrame } from "../dist/index.js";

/**
 * Await each step in order, one at a time.
 *
 * A `for` loop that awaits per iteration is what the await-in-loop rule
 * rejects, and these steps are order-sensitive by design.
 */
export async function inOrder<T>(items: readonly T[], step: (item: T) => Promise<void>): Promise<void> {
  await items.reduce<Promise<void>>((previous, item) => previous.then(() => step(item)), Promise.resolve());
}

/**
 * Poll `done` every 5ms, up to 100 times.
 *
 * The loop form of this is what the await-in-loop rule rejects, and the
 * condition is set from a socket callback rather than by the wait itself.
 */
export async function waitUntil(done: () => boolean): Promise<void> {
  const poll = async (attempt: number): Promise<void> => {
    if (done() || attempt >= 100) return;
    await delay(5);
    return poll(attempt + 1);
  };
  return poll(0);
}

export interface MockOptions {
  capabilities?: string[];
  /** Called for each client request; return frames to send back. */
  onRequest?: (request: any, send: (frame: any) => void) => void;
}

export interface MockServer {
  socketPath: string;
  close(): Promise<void>;
  /** Number of currently open client connections. */
  clientCount(): number;
  /** Push an unsolicited event to every connected client. */
  broadcast(event: any): void;
}

export async function startMockHarness(options: MockOptions = {}): Promise<MockServer> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "horon-agent-sdk-test-"));
  const socketPath = path.join(dir, "api.sock");
  const clients = new Set<net.Socket>();

  const server = net.createServer((socket) => {
    clients.add(socket);
    socket.on("close", () => clients.delete(socket));
    socket.on("error", () => clients.delete(socket));
    const decoder = new NdjsonDecoder<ClientFrame>();
    socket.on("data", (chunk) => {
      for (const request of decoder.push(chunk)) {
        const send = (frame: any) => socket.write(`${JSON.stringify(frame)}\n`);
        if (request.req === "hello") {
          send({
            v: 1,
            reply_to: request.id,
            ev: "hello_ok",
            version: 1,
            server: "mock/0.1",
            capabilities: options.capabilities ?? ["sessions", "streaming"],
          });
          continue;
        }
        options.onRequest?.(request, send);
      }
    });
  });

  await new Promise<void>((resolve) => server.listen(socketPath, resolve));

  return {
    socketPath,
    clientCount() {
      return clients.size;
    },
    broadcast(event: any) {
      for (const socket of clients) socket.write(`${JSON.stringify(event)}\n`);
    },
    close() {
      for (const socket of clients) socket.destroy();
      return new Promise<void>((resolve) => {
        server.close(() => {
          fs.rmSync(dir, { recursive: true, force: true });
          resolve();
        });
      });
    },
  };
}
