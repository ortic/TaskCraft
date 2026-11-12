// Passkeys in the browser: the server's options (binary values as base64url) → the WebAuthn API,
// and its answers back as JSON (the shape of `PublicKeyCredential.toJSON()`).

/** Whether this browser can use passkeys (WebAuthn needs https or localhost). */
export const passkeysSupported = () => typeof window !== 'undefined' && !!window.PublicKeyCredential && window.isSecureContext

function fromB64(s: string): ArrayBuffer {
  const b = atob(s.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (s.length % 4)) % 4))
  const out = new Uint8Array(b.length)
  for (let i = 0; i < b.length; i++) out[i] = b.charCodeAt(i)
  return out.buffer
}

function toB64(buf: ArrayBuffer | null | undefined): string | null {
  if (!buf) return null
  let s = ''
  for (const byte of new Uint8Array(buf)) s += String.fromCharCode(byte)
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

type Json = Record<string, any>

const credentialList = (list: Json[] | undefined): PublicKeyCredentialDescriptor[] =>
  (list ?? []).map((c) => ({ type: 'public-key', transports: c.transports, id: fromB64(c.id) }))

/** Runs `navigator.credentials.create()` with the server's options; returns the JSON to send back. */
export async function createPasskey(options: Json): Promise<Json> {
  const publicKey: PublicKeyCredentialCreationOptions = {
    ...(options as PublicKeyCredentialCreationOptions),
    challenge: fromB64(options.challenge),
    user: { ...options.user, id: fromB64(options.user.id) },
    excludeCredentials: credentialList(options.excludeCredentials),
  }
  const cred = (await navigator.credentials.create({ publicKey })) as PublicKeyCredential | null
  if (!cred) throw new Error('no passkey was created')
  const r = cred.response as AuthenticatorAttestationResponse
  return {
    id: cred.id,
    rawId: toB64(cred.rawId),
    type: cred.type,
    response: { clientDataJSON: toB64(r.clientDataJSON), attestationObject: toB64(r.attestationObject), transports: r.getTransports?.() ?? [] },
  }
}

/** Runs `navigator.credentials.get()` with the server's options; returns the JSON to send back. */
export async function signWithPasskey(options: Json): Promise<Json> {
  const publicKey: PublicKeyCredentialRequestOptions = {
    ...(options as PublicKeyCredentialRequestOptions),
    challenge: fromB64(options.challenge),
    allowCredentials: credentialList(options.allowCredentials),
  }
  const cred = (await navigator.credentials.get({ publicKey })) as PublicKeyCredential | null
  if (!cred) throw new Error('no passkey was used')
  const r = cred.response as AuthenticatorAssertionResponse
  return {
    id: cred.id,
    rawId: toB64(cred.rawId),
    type: cred.type,
    response: { clientDataJSON: toB64(r.clientDataJSON), authenticatorData: toB64(r.authenticatorData), signature: toB64(r.signature), userHandle: toB64(r.userHandle) },
  }
}

/** A passkey dialog the person closed or that timed out (not worth an error message). */
export const cancelled = (e: unknown) => e instanceof DOMException && (e.name === 'NotAllowedError' || e.name === 'AbortError')
