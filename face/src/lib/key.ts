// This device's key.
//
// An Ed25519 key made in this browser and kept in it. The private half
// is made unextractable: the page can ask the browser to sign with it,
// and nothing, the page included, can read it out. A browser offers that
// only to a page on a secure origin (https, or this machine itself), so
// over plain http there is no key and the page says so.
//
// The key can do nothing until the root key grants it something. The
// page asks for one right: `approval` on `approvals`.

const DB = "flare";
const STORE = "keys";
const NAME = "device";

export type DeviceKey = {
  /** The public key, 32 bytes. */
  publicKey: Uint8Array;
  /** An Ed25519 signature over `data`, 64 bytes. */
  sign: (data: Uint8Array) => Promise<Uint8Array>;
};

/** Why this page cannot hold a key here; null if it can. */
export async function unsupported(): Promise<string | null> {
  if (typeof window === "undefined" || !window.isSecureContext || !globalThis.crypto?.subtle) {
    return "The page is not on a secure origin. A browser lets a page keep a key only over https.";
  }
  if (typeof indexedDB === "undefined") return "This browser gives the page no place to keep a key.";
  try {
    await crypto.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]);
    return null;
  } catch {
    return "This browser cannot make Ed25519 keys. A current Safari, Chrome, Brave or Firefox can.";
  }
}

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("the browser's storage did not open"));
  });
}

function run<T>(mode: IDBTransactionMode, work: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  return open().then((db) => new Promise<T>((resolve, reject) => {
    const transaction = db.transaction(STORE, mode);
    const request = work(transaction.objectStore(STORE));
    transaction.oncomplete = () => { db.close(); resolve(request.result); };
    transaction.onerror = transaction.onabort = () => { db.close(); reject(transaction.error ?? new Error("the browser's storage refused")); };
  }));
}

async function wrap(pair: CryptoKeyPair): Promise<DeviceKey> {
  const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  return {
    publicKey,
    sign: async (data) => new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, pair.privateKey, data as BufferSource)),
  };
}

/** The key kept in this browser, if there is one. */
export async function load(): Promise<DeviceKey | null> {
  const pair = await run<CryptoKeyPair | undefined>("readonly", (store) => store.get(NAME) as IDBRequest<CryptoKeyPair | undefined>);
  return pair ? wrap(pair) : null;
}

/** Make this device's key and keep it. Refuses if there is one already. */
export async function create(): Promise<DeviceKey> {
  if (await load()) throw new Error("this browser already holds a key");
  const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]) as CryptoKeyPair;
  await run("readwrite", (store) => store.add(pair, NAME));
  return wrap(pair);
}

/** Delete this device's key. It cannot be brought back; its grant stays in the log until it is revoked. */
export async function forget(): Promise<void> {
  await run("readwrite", (store) => store.delete(NAME));
}
