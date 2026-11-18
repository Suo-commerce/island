import { describe, expect, it } from "vitest";

import { approvalBytes, b64, hex, submissionHash, unhex, SUBMISSION_DOMAIN } from "@/lib/approval";
import vectors from "./vectors/approval.json";

/** An Ed25519 private key from its 32-byte seed, as the browser imports one. */
async function fromSeed(seed: Uint8Array): Promise<CryptoKey> {
  const pkcs8 = new Uint8Array([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20, ...seed]);
  return crypto.subtle.importKey("pkcs8", pkcs8, { name: "Ed25519" }, false, ["sign"]);
}

describe("an approval, as the page signs it", () => {
  it("is for the domain and stream island-core names", () => {
    expect([vectors.domain, vectors.stream]).toEqual([SUBMISSION_DOMAIN, "approvals"]);
    expect(vectors.vectors.length).toBeGreaterThanOrEqual(4);
  });

  it("has the bytes, the hash and the signature island-core gives it", async () => {
    const key = await fromSeed(unhex(vectors.seed, 32)!);
    for (const v of vectors.vectors) {
      const bytes = approvalBytes(unhex(v.writer, 32)!, BigInt(v.counter), unhex(v.request, 32)!, v.approve);
      expect(b64(bytes), `counter ${v.counter}`).toBe(v.submission);
      const hash = submissionHash(bytes);
      expect(hex(hash), `counter ${v.counter}`).toBe(v.hash);
      // What the browser signs is what ed25519-dalek signs: the hash itself.
      const signature = new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, key, hash as BufferSource));
      expect(b64(signature), `counter ${v.counter}`).toBe(v.signature);
    }
  });

  it("refuses what is not a key, a request hash or a counter", () => {
    const k = new Uint8Array(32);
    expect(() => approvalBytes(new Uint8Array(31), 1n, k, true)).toThrow();
    expect(() => approvalBytes(k, 1n, new Uint8Array(33), true)).toThrow();
    expect(() => approvalBytes(k, 0n, k, true)).toThrow();
    expect(unhex("zz".repeat(32), 32)).toBeNull();
    expect(unhex("ab".repeat(31), 32)).toBeNull();
  });
});
