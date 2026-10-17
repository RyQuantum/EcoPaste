# crosspaste-rs

Minimal Rust implementation of the [CrossPaste](https://github.com/CrossPaste/crosspaste-desktop)
LAN sync protocol, used by this EcoPaste fork to sync text and images with CrossPaste
desktop apps. Written by reading CrossPaste's source, so it is a derivative work and is
licensed **AGPL-3.0-only** (see `LICENSE`), unlike the rest of this repository.

Compatible with CrossPaste `SyncApi.VERSION` 3 (CrossPaste 2.2.x). Both sides must report
the same version or CrossPaste shows the device as incompatible.

## Implemented

- mDNS advertise/browse of `_crosspasteService._tcp` (SyncInfo in TXT, base64 chunks)
- Pairing v2 (key exchange + 6-digit SAS), as initiator and responder; we advertise
  `pairingVersion: 2`, so CrossPaste 2.2 negotiates down to it
- P-256 ECDSA identity, ECDH shared secret, AES-256-CBC message encryption (`secure: 1`)
- Routes: `/sync/telnet`, `/sync/syncInfo`, `/sync/heartbeat[/syncInfo]`,
  `/sync/trust/v2/{exchange,confirm,cancel}`, `/sync/control/{challenge,notifyExit,notifyRemove}`,
  `/sync/paste`, `/pull/file` (1 MiB chunks, encrypted as length-prefixed records),
  plus stubs for `/sync/showToken`, `/sync/showPairingCode`, `/pull/icon`, `/pull/pasteBatch`
- Paste types: text (also the text part of HTML/RTF/URL pastes) and single/multi images

## Not implemented

- Pairing v3 (SPAKE2 + PIN), v1 bearer tokens / QR pairing
- WebSocket (`/ws/sync`); CrossPaste falls back to HTTP for desktop peers
- Files, HTML/RTF formatting, colors, source app icons, history catch-up (`/pull/pasteBatch` returns `[]`),
  push-mode uploads (`X-Sync-Mode: push`)

## Test peer

`cargo run --bin cprs -- <data-dir> [port] [device-name]` starts a headless peer that prints
events as JSON lines and reads commands from stdin (`devices`, `add <host> <port>`,
`text <s>`, `image <path>`, `pair <id>`, `pair-finish <id> <code>`, `reject <id>`,
`unpair <id>`, `quit`). `CPRS_LOOPBACK=1` listens on 127.0.0.1 without mDNS.

## Tests

- `cargo test`: unit tests; hash vectors come from CrossPaste's own `CodecsUtils`.
- `tests/interop.py`: two `cprs` peers against an official CrossPaste running `--headless`,
  driven over its CLI socket: pairing both directions, text both ways, a two-chunk image
  through CrossPaste's relay. CI runs it in `.github/workflows/ry-crosspaste.yml` against the
  version pinned in `UPSTREAM` (and weekly against the latest release).
