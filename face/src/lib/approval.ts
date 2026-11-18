// An approval, as this page signs it.
//
// The page holds its own key, so it makes the submission itself: the
// canonical bytes of an `approval` on the `approvals` stream, exactly as
// island-core encodes them (crates/island-core/src/record.rs; the
// encoding is Taso's: integers little-endian, a byte string as a u64
// length and the bytes, an enum as a u8 tag, an absent option as 0), the
// submission's hash (BLAKE3 in key-derivation mode, with the domain as
// the context), and an Ed25519 signature over that hash.
//
// src/test/vectors/approval.json holds this file and island-core to the
// same bytes: a Rust test checks the vectors against island-core, the
// page's tests check this encoder against the vectors.

import { blake3 } from "@noble/hashes/blake3.js";

export const SUBMISSION_DOMAIN = "island.submission.v1";
const STREAM = "approvals";
/** The wire tag of the `approval` kind. */
const KIND_APPROVAL = 10;

const utf8 = new TextEncoder();

/** Bytes from hex; null unless it is exactly `length` bytes of hex. */
export function unhex(hex: string, length: number): Uint8Array | null {
  if (hex.length !== length * 2 || !/^[0-9a-f]*$/i.test(hex)) return null;
  const bytes = new Uint8Array(length);
  for (let i = 0; i < length; i++) bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return bytes;
}

export function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Standard base64, as the session protocol carries bytes. */
export function b64(bytes: Uint8Array): string {
  let text = "";
  for (const byte of bytes) text += String.fromCharCode(byte);
  return btoa(text);
}

function u64(value: bigint): Uint8Array {
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setBigUint64(0, value, true);
  return bytes;
}

/** The canonical bytes of an approval (or a decline) of `request` by `writer` at `counter`. */
export function approvalBytes(writer: Uint8Array, counter: bigint, request: Uint8Array, approve: boolean): Uint8Array {
  if (writer.length !== 32 || request.length !== 32) throw new Error("a key and a request hash are 32 bytes each");
  if (counter < 1n || counter > 0xffff_ffff_ffff_ffffn) throw new Error("a counter is at least 1");
  const stream = utf8.encode(STREAM);
  const parts = [
    u64(BigInt(stream.length)), stream, // the stream, a byte string
    writer,
    u64(counter),
    Uint8Array.of(KIND_APPROVAL),
    request,
    Uint8Array.of(approve ? 0 : 1), // approve = 0, decline = 1
    Uint8Array.of(0), // no passkey proof
  ];
  const bytes = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
  let at = 0;
  for (const part of parts) {
    bytes.set(part, at);
    at += part.length;
  }
  return bytes;
}

/** A submission's hash: what its writer signs. */
export function submissionHash(submission: Uint8Array): Uint8Array {
  return blake3(submission, { context: utf8.encode(SUBMISSION_DOMAIN), dkLen: 32 });
}
