/**
 * The SDK's error type.
 *
 * Lives in its own module because both the client and the launcher raise it,
 * and importing it from the client would make `launch` and `client` circular.
 */

/** A failure with a stable `code`, so callers branch on cause not message text. */
export class HarnessError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(`${code}: ${message}`);
    this.code = code;
    this.name = "HarnessError";
  }
}

/**
 * The `code` of a caught Node error, e.g. "ENOENT", or undefined without one.
 *
 * A `catch` binding is `unknown`, and Node's errno codes are the only reliable
 * way to tell one filesystem failure from another, so the launcher and the
 * binary resolver ask the same question here.
 */
export function nodeErrorCode(error: unknown): string | undefined {
  if (typeof error === "object" && error !== null && "code" in error && typeof error.code === "string") {
    return error.code;
  }
  return undefined;
}
