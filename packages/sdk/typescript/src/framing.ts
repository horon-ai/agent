/** NDJSON line framing over a byte stream. */

import { StringDecoder } from "node:string_decoder";

/**
 * Largest single frame this decoder accepts, in bytes.
 *
 * The bridge caps an inbound frame at 64 MiB (`MAX_FRAME_BYTES` in
 * `harness-api-server`) because a 32 MiB aggregate PDF budget expands to
 * ~43 MiB of base64 plus metadata. Outbound frames are the same traffic in the
 * other direction: a session-history or side-panel event carrying that PDF is
 * the largest legitimate event a client ever sees, so the ceiling is the same
 * 64 MiB. A lower cap would refuse a frame the server considers valid, and a
 * higher one would leave a stream that never sends a newline free to grow.
 * The two constants must stay equal.
 */
export const MAX_FRAME_BYTES = 64 * 1024 * 1024;

/**
 * Largest unterminated remainder the decoder holds, in bytes.
 *
 * The remainder is a frame whose newline has not arrived yet, so it cannot
 * legitimately be larger than a whole frame. Keeping the two equal means the
 * decoder refuses an oversized frame as soon as it can tell, whether or not the
 * newline has arrived.
 */
export const MAX_RESIDUAL_BYTES = MAX_FRAME_BYTES;

/**
 * Frames above this size are transient (history, transcripts, file reads).
 * Their buffers are released instead of pinning that capacity for the rest of
 * the connection, mirroring `LARGE_FRAME_BYTES` in `harness-api-server`.
 */
export const RELEASE_FRAME_BYTES = 256 * 1024;

export type FrameLimit = "frame" | "remainder";

/**
 * A frame, or the remainder of one, grew past its byte cap.
 *
 * A named error rather than a generic one: a caller has to tell an oversized
 * frame (the peer is broken or hostile; drop the connection) from a JSON parse
 * failure (one bad frame) without matching on message text.
 */
export class FrameTooLargeError extends Error {
  readonly limit: FrameLimit;
  readonly limitBytes: number;
  readonly observedBytes: number;

  constructor(limit: FrameLimit, limitBytes: number, observedBytes: number) {
    super(
      limit === "frame"
        ? `NDJSON frame of ${observedBytes} bytes exceeds the ${limitBytes} byte frame limit`
        : `NDJSON remainder of ${observedBytes} bytes exceeds the ${limitBytes} byte buffer limit`,
    );
    this.name = "FrameTooLargeError";
    this.limit = limit;
    this.limitBytes = limitBytes;
    this.observedBytes = observedBytes;
  }
}

export interface NdjsonDecoderOptions {
  /** Largest single frame, in bytes. Defaults to {@link MAX_FRAME_BYTES}. */
  maxFrameBytes?: number;
  /** Largest unterminated remainder, in bytes. Defaults to {@link MAX_RESIDUAL_BYTES}. */
  maxResidualBytes?: number;
}

/**
 * Incremental newline-delimited JSON decoder.
 *
 * Sockets split frames anywhere, so decoding per chunk is wrong; this keeps a
 * remainder buffer and only emits complete lines. Blank lines are skipped,
 * matching the Rust reader.
 *
 * Bytes are decoded incrementally as well. Decoding each chunk on its own turns
 * a character whose UTF-8 bytes straddle two chunks into U+FFFD, which silently
 * corrupts every non-ASCII payload that lands on a chunk boundary; the
 * `StringDecoder` holds the incomplete sequence until its remaining bytes
 * arrive, so the character is emitted whole.
 */
export class NdjsonDecoder<T = unknown> {
  private buffer = "";
  /** Bytes of `buffer`, tracked incrementally so the cap check costs O(chunk). */
  private bufferBytes = 0;
  /**
   * UTF-8 state across chunk boundaries. `string` chunks are already decoded
   * text and never enter this decoder; transports hand over `Buffer`s.
   */
  private readonly text = new StringDecoder("utf8");
  private readonly maxFrameBytes: number;
  private readonly maxResidualBytes: number;

  constructor(options: NdjsonDecoderOptions = {}) {
    this.maxFrameBytes = options.maxFrameBytes ?? MAX_FRAME_BYTES;
    this.maxResidualBytes = options.maxResidualBytes ?? MAX_RESIDUAL_BYTES;
  }

  get bufferedBytes(): number {
    return this.bufferBytes;
  }

  push(chunk: Buffer | string): T[] {
    const text = typeof chunk === "string" ? chunk : this.text.write(chunk);
    if (text === "") return [];
    const textBytes = Buffer.byteLength(text);
    // Refuse a chunk that cannot belong to an acceptable frame before
    // concatenating it: the concatenation would allocate a second copy of a
    // buffer that is already too large. A chunk carrying a newline is let
    // through, because its frames are bounded by the per-frame check below.
    if (this.bufferBytes + textBytes > this.maxResidualBytes && !text.includes("\n")) {
      const observed = this.bufferBytes + textBytes;
      this.discard();
      throw new FrameTooLargeError("remainder", this.maxResidualBytes, observed);
    }
    this.buffer += text;
    this.bufferBytes += textBytes;
    const out: T[] = [];
    let index = this.buffer.indexOf("\n");
    while (index !== -1) {
      const frame = this.buffer.slice(0, index);
      const line = frame.trim();
      const frameBytes = Buffer.byteLength(frame);
      this.buffer = this.buffer.slice(index + 1);
      // The newline goes with the frame, so it leaves the buffer too.
      this.bufferBytes -= frameBytes + 1;
      if (frameBytes > this.maxFrameBytes) {
        this.discard();
        throw new FrameTooLargeError("frame", this.maxFrameBytes, frameBytes);
      }
      if (line !== "") out.push(JSON.parse(line));
      if (frameBytes > RELEASE_FRAME_BYTES) this.releaseLargeFrame();
      index = this.buffer.indexOf("\n");
    }
    // The newline never arrived: the remainder is a frame that is still growing.
    if (this.bufferBytes > this.maxResidualBytes) {
      const observed = this.bufferBytes;
      this.discard();
      throw new FrameTooLargeError("remainder", this.maxResidualBytes, observed);
    }
    return out;
  }

  /**
   * Copy the remainder out of a large frame's string.
   *
   * V8 keeps a `slice` result as a view over the string it came from, so a
   * small remainder cut out of a multi-megabyte frame keeps that frame's
   * storage alive for the rest of the connection. Rebuilding the remainder from
   * its own bytes copies it out. The round trip is lossless because `buffer`
   * holds decoded UTF-8, which never contains a lone surrogate.
   */
  private releaseLargeFrame(): void {
    if (this.buffer === "") return;
    this.buffer = Buffer.from(this.buffer, "utf8").toString("utf8");
  }

  /**
   * Drop the partial frame, its bytes, and any half-decoded character.
   *
   * A frame that broke a cap can never be completed, and what follows it on the
   * wire cannot be trusted to start on a frame boundary. Starting clean means a
   * caller that keeps reading sees the next frame rather than the tail of the
   * rejected one, and no half-character from the rejected stream can leak into
   * the next frame.
   */
  private discard(): void {
    this.buffer = "";
    this.bufferBytes = 0;
    this.text.end();
  }
}

export function encodeFrame(frame: unknown): string {
  return `${JSON.stringify(frame)}\n`;
}
