<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Phase 1 — Requirements Discovery: Issue #495

> **Issue:** [#495 — Replace gateway asymmetric key-pair usage in pairing with AES-256-GCM protected pairing requests](https://github.com/Alan-Jowett/sonde/issues/495)
> **Status:** Discovery complete — pending user review
> **Scope:** **Entire radio protocol and BLE pairing flow.** This is NOT a pairing-only change. HMAC-SHA256 is replaced by AES-256-GCM on **all** frames (WAKE, COMMAND, APP_DATA, PEER_REQUEST, PEER_ACK). The gateway Ed25519 keypair, gateway identity, ECDH, HKDF, and several BLE messages are retired.

---

## 1  Change Summary

**Core objective:** Replace the current HMAC-SHA256 frame authentication with AES-256-GCM authenticated encryption across the **entire** radio protocol, and simplify the BLE pairing flow by eliminating the gateway asymmetric keypair, ECDH key agreement, and HKDF key derivation.

### 1.1  Current Model

The current protocol uses two distinct cryptographic layers:

1. **Frame layer (all messages):** 11-byte cleartext header (`key_hint` 2B + `msg_type` 1B + `nonce` 8B) + CBOR payload + 32-byte HMAC-SHA256 tag (keyed with `node_psk`). No encryption — payload is authenticated but sent in cleartext.
2. **Pairing layer (PEER_REQUEST inner payload only):** ECDH (X25519) + HKDF-SHA256 + AES-256-GCM with gateway Ed25519 keypair converted to X25519. Phone HMAC-SHA256 authenticates pairing requests. `registration_proof` in PEER_ACK uses HMAC-SHA256.

Phase 1 (phone registration) uses a challenge–response protocol (`REQUEST_GW_INFO` / `GW_INFO_RESPONSE`) where the gateway signs a challenge with its Ed25519 key, followed by `REGISTER_PHONE` / `PHONE_REGISTERED` with ECDH-derived AES-256-GCM encryption to deliver a gateway-generated `phone_psk`.

### 1.2  Proposed Model

**One crypto scheme, one frame format, different keys per role:**

All frames (WAKE, COMMAND, APP_DATA, PEER_REQUEST, PEER_ACK) use the same format:
```
Header (AAD, cleartext): key_hint(2B) ‖ msg_type(1B) ‖ nonce(8B)
Body: AES-256-GCM ciphertext ‖ 16-byte GCM tag

gcm_nonce = SHA-256(psk)[0..3] ‖ msg_type(1B) ‖ frame_nonce(8B)   // 12 bytes
key       = PSK identified by key_hint
AAD       = full 11-byte header
```

| Message | key_hint identifies | PSK used |
|---------|---------------------|----------|
| WAKE | node | `node_psk` |
| COMMAND | node | `node_psk` |
| APP_DATA | node | `node_psk` |
| PEER_REQUEST | phone / pairing tool | `phone_psk` |
| PEER_ACK | node | `node_psk` |

**Phase 1 (phone registration) — simplified:**
```
Phone ──BLE LESC (Numeric Comparison)──► Gateway
  ├── REGISTER_PHONE(label, phone_psk) ───►│  ← phone generates PSK
  │◄── PHONE_REGISTERED(status, rf_channel, phone_key_hint) ──┤
```
- Phone generates 256-bit random PSK.
- BLE LESC provides confidentiality and authentication.
- `REQUEST_GW_INFO` / `GW_INFO_RESPONSE` **retired**.
- Gateway has **no keypair**, **no identity**.

**Phase 2 (node pairing) — unchanged relay, new crypto:**
- Phone creates `encrypted_payload` = AES-256-GCM(`phone_psk`, PairingRequest, AAD=`"sonde-pairing-v2"`).
- Node stores and relays opaque blob in PEER_REQUEST.
- PEER_REQUEST frame encrypted with `phone_psk` (`key_hint` = phone's key_hint).
- Gateway looks up phone PSK by `key_hint`, decrypts outer frame, decrypts inner payload.
- Gateway registers node, sends PEER_ACK encrypted with `node_psk` (proves registration).

### 1.3  What's Retired

| Retired element | Where specified today |
|-----------------|----------------------|
| Gateway Ed25519 keypair (generation, storage, backup/restore) | GW-1200, GW-1201, GW-1203, `ble-pairing-protocol.md` §2.1 |
| Gateway identity concept (`gateway_id`) | GW-1201, `security.md` §2.7.3, §7.1 |
| X25519 ECDH key agreement | GW-1202, PT-0405, PT-0902, `ble-pairing-protocol.md` §1.2, §5.4, §5.5, §6.4 |
| HKDF-SHA256 key derivation | PT-1101, `ble-pairing-protocol.md` §1.2, §5.5, §6.4 |
| Frame-level HMAC-SHA256 (ALL frames) | GW-0600, GW-0603, GW-1214, ND-0300, ND-0301, `protocol.md` §3.1, `security.md` §3.1 |
| `REQUEST_GW_INFO` / `GW_INFO_RESPONSE` messages | PT-0301, GW-1206, `ble-pairing-protocol.md` §5.2, §5.3 |
| Phone ephemeral X25519 keypair | PT-0304 (Phase 2 portion), PT-0405, `ble-pairing-protocol.md` §5.4, §6.4 |
| `registration_proof` HMAC in PEER_ACK | GW-1219, ND-0912, `ble-pairing-protocol.md` §7.2 |
| Ed25519→X25519 key conversion | GW-1202, PT-0902, `ble-pairing-protocol.md` §5.5 |
| Phone HMAC-SHA256 authentication of pairing requests | PT-0404, GW-1213, `ble-pairing-protocol.md` §6.4 |
| Trust-on-first-use (TOFU) of gateway public key | PT-0302, `ble-pairing-protocol.md` §5.3 |
| Gateway-generated `phone_psk` delivery via ECDH | GW-1209 (current form), PT-0303 (current form), `ble-pairing-protocol.md` §5.5 |

### 1.4  What Changes

| Aspect | Current | Proposed |
|--------|---------|----------|
| Frame authentication (ALL frames) | HMAC-SHA256 (32B tag, auth only) | AES-256-GCM (16B tag, auth + encryption) |
| Frame overhead | 43B (11B header + 32B HMAC) | 27B (11B header + 16B GCM tag) |
| Usable payload budget (250B frame) | 207B | 223B (+16B gained) |
| Frame nonce → GCM nonce | `nonce` used directly (8B in HMAC input) | `gcm_nonce` = SHA-256(psk)[0..3] ‖ msg_type(1B) ‖ nonce(8B) = 12B |
| Payload confidentiality | None (cleartext CBOR) | Yes (CBOR encrypted) |
| Phase 1 phone registration | Challenge–response + ECDH + HKDF + AES-GCM | Phone generates PSK, sends over BLE LESC |
| `REGISTER_PHONE` content | Ephemeral X25519 pubkey + label | `phone_psk` + label |
| `PHONE_REGISTERED` content | ECDH-encrypted PSK + channel + status | `status` (1B) + `rf_channel` (1B) + `phone_key_hint` (2B, BE u16) |
| PEER_REQUEST frame key | `node_psk` (HMAC) | `phone_psk` (AES-GCM) |
| PEER_REQUEST inner payload | ECDH-encrypted, HMAC-authenticated | AES-256-GCM(`phone_psk`, AAD=`"sonde-pairing-v2"`) |
| PEER_ACK authentication | HMAC + `registration_proof` | AES-GCM with `node_psk` (encryption = proof of registration) |
| Gateway state for pairing | Ed25519 seed + `gateway_id` + phone PSKs | Phone PSKs only |

### 1.5  What's Unchanged

- PSK generation (256-bit random via `getrandom::fill()`).
- `key_hint` derivation: `u16::from_be_bytes(SHA-256(psk)[30..32])`.
- BLE LESC (Numeric Comparison) for Phase 1 (phone ↔ gateway) and Phase 2 (phone ↔ node).
- Node stores and relays opaque `encrypted_payload` without understanding it.
- PairingRequest CBOR structure (`node_id`, `node_key_hint`, `node_psk`, `rf_channel`, `sensors`, `timestamp`).
- Deterministic CBOR encoding (RFC 8949 §4.2).
- Silent-discard error model — no error responses sent.
- Sequence-number replay protection for post-WAKE messages.
- Random nonces for PEER_REQUEST / PEER_ACK (not sequence numbers).
- Timestamp tolerance ±86 400 s for PairingRequest.
- Node ID uniqueness check.
- Phone PSK revocation semantics.

---

## 2  Change Manifest

### 2.1  `protocol.md`

This document defines the radio frame format. **Every section** describing HMAC-SHA256 is affected.

| Section | Topic | Action | Rationale |
|---------|-------|--------|-----------|
| §3.1 Frame structure | Header + Payload + HMAC-SHA256 | **MODIFY (major)** | Replace `header ‖ payload ‖ hmac[32]` with `header ‖ AES-256-GCM(payload) ‖ tag[16]`. GCM nonce = SHA-256(psk)[0..3] ‖ msg_type ‖ frame_nonce. AAD = 11-byte header. |
| §3.2 What is authenticated | HMAC covers header + payload | MODIFY | AEAD covers header (AAD) + payload (ciphertext). Payload is now encrypted AND authenticated. |
| §3.3 Gateway verification | Compute HMAC with candidate PSKs | MODIFY | Try AES-256-GCM-Open with candidate PSKs. First successful decryption = authenticated. |
| §3.4 Node verification | Compute HMAC with own PSK | MODIFY | AES-256-GCM-Open with node's own PSK. |
| §4 Replay protection | Nonce / sequence number | MODIFY | GCM nonce construction: 3-byte PSK-derived prefix + msg_type + 8-byte frame nonce. Sequence-number semantics unchanged. |
| HMAC trailer size | 32 bytes | MODIFY | GCM tag: 16 bytes. Total frame overhead drops from 43B to 27B. |
| msg_type 0x05 | PEER_REQUEST | MODIFY | Key used is `phone_psk` (not `node_psk`). `key_hint` identifies phone. |
| msg_type 0x84 | PEER_ACK | MODIFY | Now AES-GCM encrypted with `node_psk`. `registration_proof` field retired. |

### 2.2  `ble-pairing-protocol.md`

This document uses narrative requirements (MUST/SHALL), not formal REQ-IDs.

| Section | Topic | Action | Rationale |
|---------|-------|--------|-----------|
| §1.1 Actors table | Phone, Gateway, Node roles | MODIFY | Remove "encrypts payloads with gateway public key"; phone now generates PSK. Gateway has no keypair. |
| §1.2 Cryptographic primitives | Primitive list | **MODIFY (major)** | Remove Ed25519, X25519, HKDF-SHA256, HMAC-SHA256 (for frames). Replace with AES-256-GCM as sole frame/payload primitive. |
| §2.1 Gateway Ed25519 keypair | Keypair generation + storage | **RETIRE** | Gateway has no keypair. Entire section removed. |
| §2.2 Phone PSK | Gateway generates PSK | **MODIFY** | Phone generates PSK and sends it to gateway in `REGISTER_PHONE`. |
| §5.2 REQUEST_GW_INFO | Challenge–response request | **RETIRE** | No gateway authentication challenge. BLE LESC suffices. |
| §5.3 GW_INFO_RESPONSE | Ed25519 signature + public key | **RETIRE** | No gateway public key or identity. |
| §5.4 REGISTER_PHONE | Ephemeral X25519 pubkey + label | **MODIFY (major)** | Phone sends `phone_psk` + label. No ephemeral keypair. |
| §5.5 PHONE_REGISTERED | ECDH-encrypted PSK delivery | **MODIFY (major)** | Now carries `status` + `rf_channel` + `phone_key_hint` (2 bytes, BE u16). No encryption needed — BLE LESC protects the channel. |
| §5.7 Phone persistence | Persist `gw_public_key`, `gateway_id`, `phone_psk` | MODIFY | Remove `gw_public_key`, `gateway_id`. Persist `phone_psk`, `phone_key_hint`, `rf_channel`. |
| §6.4 Node pairing encryption | ECDH + HKDF + AES-GCM + HMAC | **MODIFY (major)** | Replace with AES-256-GCM(`phone_psk`, PairingRequest, AAD=`"sonde-pairing-v2"`). No ECDH, no HKDF, no phone HMAC. |
| §7.1 PEER_REQUEST format | Header + CBOR + 32B HMAC (node PSK) | **MODIFY (major)** | Header + AES-GCM ciphertext + 16B tag. Frame encrypted with `phone_psk`. `key_hint` identifies phone. |
| §7.2 PEER_ACK format | Header + CBOR + 32B HMAC + `registration_proof` | **MODIFY (major)** | Header + AES-GCM ciphertext + 16B tag. Frame encrypted with `node_psk`. `registration_proof` retired — AEAD encryption with `node_psk` proves gateway holds the key. |
| §7.3 Gateway PEER_REQUEST processing | 13-step ECDH + HMAC pipeline | **MODIFY (major)** | New pipeline: look up `phone_psk` by `key_hint` → AES-GCM-Open outer frame → extract `encrypted_payload` from CBOR → AES-GCM-Open inner payload (AAD=`"sonde-pairing-v2"`) → parse PairingRequest → verify timestamp → check node_id uniqueness → register → send PEER_ACK. |
| §7.4 HMAC bootstrap rationale | Security analysis | **RETIRE** | No HMAC to bootstrap. AEAD provides auth + encryption atomically. |
| §11 Constants and sizes | HKDF info strings, sizes | **MODIFY** | Remove all HKDF info strings. Remove HMAC sizes. Add GCM tag size (16B), GCM nonce construction, AAD=`"sonde-pairing-v2"`. |

### 2.3  `security.md`

| Section | Topic | Action | Rationale |
|---------|-------|--------|-----------|
| §2.1 Per-node symmetric keys | PSK used for HMAC-SHA256 | **MODIFY** | PSK used for AES-256-GCM (auth + encryption). |
| §2.7.1 Phone PSK lifecycle — Issuance | Gateway generates PSK, ECDH-encrypted delivery | **MODIFY (major)** | Phone generates PSK. Sent in `REGISTER_PHONE` over BLE LESC. No ECDH. |
| §2.7.2 Trust properties | HMAC-based authorization | MODIFY | Authorization via AEAD decryption, not HMAC. |
| §2.7.3 Gateway Ed25519 keypair | Keypair for signing + ECDH | **RETIRE** | No gateway keypair. Entire subsection removed. |
| §3.1 Frame structure | Header + Payload + HMAC-SHA256 | **MODIFY (major)** | Header (AAD) + AES-256-GCM(Payload) + 16B GCM tag. |
| §3.2 What is authenticated | HMAC covers header + payload | MODIFY | AEAD: header as AAD, payload encrypted and authenticated. |
| §3.3 Gateway verification | HMAC with candidate PSKs | MODIFY | AES-256-GCM-Open with candidate PSKs. |
| §3.4 Node verification | HMAC with own PSK | MODIFY | AES-256-GCM-Open with own PSK. |
| §4.1 Replay protection | HMAC-authenticated nonces | MODIFY | AEAD-authenticated nonces (same semantics, different primitive). |
| §5.1 Key is identity | `key_hint` lookup + HMAC | MODIFY | `key_hint` lookup + AEAD open. |
| §6 Failure modes | Invalid HMAC → discard | MODIFY | Invalid GCM tag → discard. |
| §7.1 Gateway identity | "No persistent cryptographic identity" | MODIFY | Statement becomes even stronger — no keypair at all, not just "authority from key database." |
| §8 Summary table | HMAC-SHA256 per frame | **MODIFY** | AES-256-GCM per frame (auth + encryption). |
| §9.3 Pairing channel tradeoffs | ECDH envelope discussion | MODIFY | Remove ECDH references. Pairing payload encrypted with `phone_psk`. |
| Referenced IDs: GW-0601a | Master key for Ed25519 seed | **MODIFY** | No Ed25519 seed to encrypt. Master key still protects PSK database. |

### 2.4  `ble-pairing-tool-requirements.md`

| REQ-ID | Title | Action | Rationale |
|--------|-------|--------|-----------|
| PT-0301 | Gateway authentication (REQUEST_GW_INFO / GW_INFO_RESPONSE) | **RETIRE** | No gateway challenge–response. BLE LESC provides authentication. |
| PT-0302 | Trust-on-first-use (TOFU) | **RETIRE** | No gateway public key to pin. |
| PT-0303 | Phone registration (REGISTER_PHONE / PHONE_REGISTERED) | **MODIFY (major)** | Phone generates 256-bit `phone_psk` from CSPRNG, writes `REGISTER_PHONE(label, phone_psk)`, receives `PHONE_REGISTERED(status, rf_channel, phone_key_hint)`. No ECDH, no HKDF, no AES-GCM decryption. |
| PT-0304 | Ephemeral key zeroing | **MODIFY** | Remove all ephemeral X25519 references. Zeroing applies to `phone_psk` and AES-GCM key material only. |
| PT-0403 | PairingRequest CBOR construction | UNAFFECTED | CBOR structure unchanged. |
| PT-0404 | Phone HMAC authentication | **RETIRE** | No phone HMAC. AEAD provides authentication. |
| PT-0405 | Gateway public key encryption | **RETIRE** | No ECDH. Replace with AES-256-GCM(`phone_psk`, PairingRequest, AAD=`"sonde-pairing-v2"`). |
| PT-0406 | Encrypted payload size validation | **MODIFY** | New overhead: nonce[12] + GCM tag[16] = 28B (was 94B). More CBOR space available. |
| PT-0407 | NODE_PROVISION transmission | **MODIFY** | `encrypted_payload` format changes (no `eph_public`, no `phone_hmac`). |
| PT-0408 | Node PSK zeroing | UNAFFECTED | Still zero `node_psk` after provisioning. |
| PT-0800 | Pairing store contents | **MODIFY** | Remove `gw_public_key`, `gateway_id`. Store `phone_psk`, `phone_key_hint`, `rf_channel`, `phone_label`. |
| PT-0900 | No key material in logs | UNAFFECTED | Still applies. |
| PT-0901 | CSPRNG for all randomness | UNAFFECTED | Still applies. |
| PT-0902 | Ed25519 ↔ X25519 conversion safety | **RETIRE** | No Ed25519 or X25519. |
| PT-0904 | BLE pairing mode enforcement | UNAFFECTED | LESC Numeric Comparison still required. |
| PT-1100 | Required primitives | **MODIFY (major)** | Remove: Ed25519 verification, X25519 ECDH, Ed25519→X25519 conversion, HKDF-SHA256, HMAC-SHA256. Retain: AES-256-GCM, SHA-256 (key_hint derivation), CSPRNG. |
| PT-1101 | HKDF parameters | **RETIRE** | No HKDF. |
| PT-1102 | AES-GCM AAD | **MODIFY** | Inner payload AAD = `"sonde-pairing-v2"`. No `gateway_id` in AAD. |
| PT-1103 | Deterministic CBOR encoding | UNAFFECTED | |
| PT-1201 | Phase 1 happy path test | **MODIFY** | Test new simplified Phase 1 flow (phone generates PSK, simple ACK). |
| PT-1202 | Phase 1 error path tests | **MODIFY** | Remove: signature verification failures, ECDH failures. Add: BLE LESC failures, registration window closed. |
| PT-1203 | Phase 2 happy path test | **MODIFY** | Test AES-256-GCM with `phone_psk`, no ECDH. |
| PT-1204 | Phase 2 error path tests | **MODIFY** | Add: wrong PSK, AAD mismatch. Remove: ECDH failures, HMAC failures. |

### 2.5  `gateway-requirements.md`

| REQ-ID | Title | Action | Rationale |
|--------|-------|--------|-----------|
| GW-0600 | HMAC-SHA256 message authentication | **MODIFY (major)** | Replace HMAC-SHA256 with AES-256-GCM for ALL inbound/outbound frames. GCM nonce = SHA-256(psk)[0..3] ‖ msg_type ‖ frame_nonce. AAD = 11-byte header. |
| GW-0601 | Per-node key management | UNAFFECTED | PSK lookup by `key_hint` unchanged. |
| GW-0601a | Key store encryption at rest | **MODIFY** | No Ed25519 seed to protect. Master key still encrypts PSK database. |
| GW-0601b | OS-native master key protection | UNAFFECTED | `KeyProvider` trait unchanged. |
| GW-0602 | Replay protection — sequence numbers | UNAFFECTED | Same semantics, now AEAD-protected. |
| GW-0603 | Authentication overhead budget | **MODIFY** | 11B header + 16B GCM tag = 27B overhead (was 43B). Payload budget increases by 16B. |
| GW-0805 | State export/import | **MODIFY** | Remove Ed25519 seed and `gateway_id` from export bundle. Only PSKs + node registry + phone PSKs. |
| GW-1000 | Gateway failover / replaceability | **MODIFY** | No keypair to replicate. Only node key database needs to be shared. Simpler failover. |
| GW-1001 | Exportable / importable state | **MODIFY** | Remove keypair and `gateway_id` from state definition. |
| GW-1200 | Ed25519 keypair generation | **RETIRE** | No gateway keypair. |
| GW-1201 | Gateway identity generation | **RETIRE** | No `gateway_id`. |
| GW-1202 | Ed25519→X25519 conversion | **RETIRE** | No ECDH. |
| GW-1203 | Ed25519 seed replication | **RETIRE** | No seed to replicate. |
| GW-1204 | BLE GATT server | MODIFY | Remove `REQUEST_GW_INFO` / `GW_INFO_RESPONSE` handling. Simplify to `REGISTER_PHONE` / `PHONE_REGISTERED`. |
| GW-1206 | REQUEST_GW_INFO handling | **RETIRE** | No challenge–response. |
| GW-1207 | Registration window enforcement | UNAFFECTED | Window still gates `REGISTER_PHONE`. |
| GW-1208 | Registration window activation | UNAFFECTED | Admin-activated. |
| GW-1209 | REGISTER_PHONE processing | **MODIFY (major)** | Gateway receives `phone_psk` from phone (phone-generated). No ECDH, no HKDF, no AES-GCM encryption of response. Gateway stores PSK + label + timestamp. |
| GW-1210 | Phone PSK storage and revocation | **MODIFY** | PSK comes from phone (not gateway-generated). Storage and revocation semantics unchanged. |
| GW-1211 | PEER_REQUEST key-hint bypass | **MODIFY** | `key_hint` now identifies phone PSK (not node PSK). Gateway looks up phone PSK candidates. |
| GW-1212 | PEER_REQUEST decryption | **MODIFY (major)** | Outer frame: AES-256-GCM-Open with `phone_psk` (identified by `key_hint`). Inner payload: AES-256-GCM-Open with `phone_psk` (AAD=`"sonde-pairing-v2"`). Extract PairingRequest CBOR. |
| GW-1213 | Phone HMAC verification | **RETIRE** | AEAD decryption provides authentication. |
| GW-1214 | Frame HMAC verification (node PSK) | **RETIRE** | Frame-level HMAC replaced by AES-256-GCM on all frames. For PEER_REQUEST, the frame uses `phone_psk` (not `node_psk`). |
| GW-1215 | PairingRequest timestamp validation | UNAFFECTED | ±86 400 s check after decryption. |
| GW-1216 | Node ID duplicate handling | UNAFFECTED | |
| GW-1217 | Key hint consistency check | MODIFY | Frame header `key_hint` is now phone's key_hint. Node's `key_hint` is inside PairingRequest CBOR. Check still applies but inputs differ. |
| GW-1218 | Node registration from PEER_REQUEST | UNAFFECTED | Same registration fields. |
| GW-1219 | PEER_ACK generation | **MODIFY (major)** | Remove `registration_proof` HMAC. PEER_ACK frame encrypted with `node_psk` via AES-256-GCM. Encryption with `node_psk` proves gateway holds the key (replaces `registration_proof`). CBOR payload: `{1: 0}` (status only). |
| GW-1220 | Silent-discard error model | **MODIFY** | Pipeline changes. New pipeline: `key_hint` → phone PSK lookup → AES-GCM-Open outer frame → parse CBOR → AES-GCM-Open inner payload → parse PairingRequest → timestamp → node_id → register → PEER_ACK. |
| GW-1221 | Random nonces for PEER_REQUEST/PEER_ACK | UNAFFECTED | Random nonces still used (not sequence numbers). |
| GW-1222 | Admin API — BLE pairing | UNAFFECTED | |
| GW-1223 | Admin API — phone listing | UNAFFECTED | |
| GW-1224 | Admin API — phone revocation | UNAFFECTED | Revoked PSKs excluded from decryption candidate set. |

### 2.6  `node-requirements.md`

| REQ-ID | Title | Action | Rationale |
|--------|-------|--------|-----------|
| ND-0102 | Frame format compliance | **MODIFY (major)** | Frame = `header ‖ AES-256-GCM(payload) ‖ tag[16]`. Was `header ‖ payload ‖ hmac[32]`. |
| ND-0103 | Frame size constraint | **MODIFY** | Usable payload = 250 − 11 (header) − 16 (GCM tag) = 223B (was 207B). |
| ND-0300 | HMAC-SHA256 authentication | **MODIFY (major)** | Replace with AES-256-GCM. Node encrypts + authenticates every outbound frame. |
| ND-0301 | Inbound HMAC verification | **MODIFY (major)** | Replace with AES-256-GCM-Open. Decryption failure → discard. |
| ND-0302 | Response binding verification | UNAFFECTED | Nonce matching unchanged. |
| ND-0303 | Sequence number management | UNAFFECTED | Sequence number semantics unchanged. |
| ND-0304 | Nonce generation | **MODIFY** | WAKE nonce still random 8B. GCM nonce = SHA-256(`node_psk`)[0..3] ‖ msg_type(1B) ‖ frame_nonce(8B). |
| ND-0905 | NODE_PROVISION handling | UNAFFECTED | Node parses `encrypted_payload` as opaque blob. Format change is transparent. |
| ND-0906 | NODE_PROVISION NVS persistence | UNAFFECTED | Same NVS keys stored. |
| ND-0909 | PEER_REQUEST frame construction | **MODIFY (major)** | Frame encrypted with `phone_psk` (not `node_psk`). Node needs `phone_psk` and phone's `key_hint` from provisioning, OR the phone provides the complete pre-built PEER_REQUEST frame as the opaque blob. |
| ND-0910 | PEER_REQUEST retransmission | UNAFFECTED | |
| ND-0911 | PEER_ACK listen timeout | UNAFFECTED | |
| ND-0912 | PEER_ACK verification | **MODIFY (major)** | Replace HMAC + `registration_proof` verification with AES-256-GCM-Open using `node_psk`. Successful decryption = valid PEER_ACK. |
| ND-0913 | Registration completion | UNAFFECTED | Set `reg_complete` flag. |
| ND-0914 | Deferred payload erasure | UNAFFECTED | |
| ND-0915 | Self-healing on WAKE failure | MODIFY | "HMAC verification failure" → "AEAD verification failure." |
| ND-0916 | NVS layout | MODIFY | May need phone's `key_hint` if node builds PEER_REQUEST frame (vs relaying pre-built blob). |
| ND-0918 | Main task stack size | UNAFFECTED | |

### 2.7  Additional Affected Areas

#### `sonde-protocol` Crate

This is the **most heavily affected crate**. The frame codec is the core of `sonde-protocol`.

| Area | Action | Rationale |
|------|--------|-----------|
| `HmacProvider` trait | **RETIRE or MODIFY** | No longer used for frame authentication. May be retained for backward compat or removed entirely. |
| `Sha256Provider` trait | UNAFFECTED | Still needed for `key_hint` derivation. |
| Frame codec (encode/decode) | **MODIFY (major)** | Replace HMAC computation/verification with AES-256-GCM encrypt/decrypt. New trait: `AeadProvider` (or similar). |
| GCM nonce construction | **NEW** | `gcm_nonce` = SHA-256(psk)[0..3] ‖ msg_type(1B) ‖ frame_nonce(8B). Must be computed in codec. |
| Frame overhead constants | **MODIFY** | AUTH_TAG_SIZE: 32 → 16. FRAME_OVERHEAD: 43 → 27. |
| `msg_type` dispatch | MODIFY | PEER_REQUEST (0x05) uses different PSK (`phone_psk`) than other messages. |

#### `sonde-gateway` Crate

| Area | Action | Rationale |
|------|--------|-----------|
| Frame authentication layer | **MODIFY (major)** | All frame encode/decode calls switch to AES-256-GCM. |
| Keypair management module | **RETIRE** | No Ed25519 keypair. |
| `gateway_id` generation/storage | **RETIRE** | No gateway identity. |
| BLE GATT command handling | **MODIFY** | Remove `REQUEST_GW_INFO` / `GW_INFO_RESPONSE`. Simplify `REGISTER_PHONE` / `PHONE_REGISTERED`. |
| PEER_REQUEST processing pipeline | **MODIFY (major)** | Complete rewrite — two-layer AES-GCM decryption, no ECDH/HKDF/HMAC. |
| PEER_ACK construction | **MODIFY** | Remove `registration_proof`. Encrypt frame with `node_psk`. |
| State export/import | **MODIFY** | Remove keypair and `gateway_id` from bundle. |

#### `sonde-node` Crate (ESP32 Firmware)

| Area | Action | Rationale |
|------|--------|-----------|
| Frame encode/decode | **MODIFY (major)** | HMAC → AES-256-GCM for all frames. Needs AES-GCM implementation on ESP32. |
| PEER_REQUEST construction | **MODIFY** | Uses `phone_psk` for frame encryption (or relays pre-built frame). |
| PEER_ACK verification | **MODIFY** | AES-256-GCM-Open with `node_psk` replaces HMAC + `registration_proof`. |
| Crypto dependencies | **MODIFY** | Need AES-256-GCM on embedded target. ESP-IDF provides `mbedtls`. |

#### `sonde-modem` Crate

**Impact: None.** Modem relays BLE/ESP-NOW bytes without inspecting content.

#### `sonde-admin` CLI

**Impact: Minimal.** Admin commands use gRPC to gateway. No direct crypto.

#### `sonde-e2e` Tests

**Impact: Major.** All test helpers that construct/verify frames must switch to AES-256-GCM. All PEER_REQUEST test paths need complete rewrite.

---

## 3  New Requirements

### 3.1  AES-256-GCM Frame Format (All Messages)

**NEW-FRAME-001**: All ESP-NOW frames MUST use AES-256-GCM authenticated encryption:
- **Header (cleartext, used as AAD):** `key_hint`[2B] ‖ `msg_type`[1B] ‖ `nonce`[8B] — 11 bytes total.
- **Body:** AES-256-GCM ciphertext ‖ GCM tag[16B].
- **GCM nonce (12B):** SHA-256(psk)[0..3] ‖ msg_type[1B] ‖ frame_nonce[8B].
- **Key:** PSK identified by `key_hint`.
- **AAD:** Full 11-byte header.

Frame on wire: `header[11] ‖ ciphertext[variable] ‖ tag[16]`.

### 3.2  GCM Nonce Construction

**NEW-FRAME-002**: The 12-byte GCM nonce MUST be constructed as:
```
gcm_nonce = SHA-256(psk)[0..3] ‖ msg_type[1] ‖ frame_nonce[8]
```
Where `frame_nonce` is the 8-byte value from the frame header (random for WAKE/PEER_REQUEST, sequence number for post-WAKE messages) and `msg_type` is the 1-byte message type from the header. Including `msg_type` ensures that request/response pairs sharing the same `frame_nonce` produce distinct GCM nonces. The 3-byte PSK-derived prefix makes cross-key nonce collisions extremely unlikely even if frame nonces collide, but does not remove the requirement for per-PSK nonce uniqueness.

### 3.3  Per-Message PSK Assignment

**NEW-FRAME-003**: The PSK used for frame encryption/decryption MUST be determined by message type:

| `msg_type` | PSK | `key_hint` source |
|------------|-----|-------------------|
| 0x01 WAKE | `node_psk` | `SHA-256(node_psk)[30..32]` |
| 0x81 COMMAND | `node_psk` | Same as WAKE response |
| 0x03 APP_DATA | `node_psk` | Same |
| 0x83 APP_DATA_REPLY | `node_psk` | Same |
| 0x05 PEER_REQUEST | `phone_psk` | `SHA-256(phone_psk)[30..32]` |
| 0x84 PEER_ACK | `node_psk` | `SHA-256(node_psk)[30..32]` |

### 3.4  Simplified Phone Registration

**NEW-REG-001**: The `REGISTER_PHONE` BLE message MUST contain:
- `phone_psk` (32 bytes) — generated by the phone from OS CSPRNG.
- `label_len` (1 byte) + `label` (0–64 bytes UTF-8).

**NEW-REG-002**: The `PHONE_REGISTERED` BLE response is sent **only on successful registration** and MUST contain:
- `status` (1 byte): `0x00` = accepted.
- `rf_channel` (1 byte): WiFi channel 1–13.
- `phone_key_hint` (2 bytes, big-endian): `SHA-256(phone_psk)[30..32]`.

Registration rejections and window-closed conditions are reported using the `ERROR` (0xFF) envelope as defined in `ble-pairing-protocol.md`; `PHONE_REGISTERED` is **not** sent in those cases.

No ECDH, no HKDF, no AES-GCM encryption of the response — BLE LESC provides channel confidentiality and authentication.

### 3.5  Pairing Request Encryption (Inner Payload)

**NEW-PAIR-001**: The phone MUST encrypt the PairingRequest using AES-256-GCM with:
- **Key:** `phone_psk`.
- **Nonce:** 96-bit value from OS CSPRNG.
- **Plaintext:** PairingRequest CBOR bytes (deterministic encoding per RFC 8949 §4.2).
- **AAD:** `"sonde-pairing-v2"` (16 bytes, UTF-8).

The `encrypted_payload` MUST be assembled as:
```
nonce[12] ‖ ciphertext[variable] ‖ GCM_tag[16]
```

### 3.6  PEER_ACK Without `registration_proof`

**NEW-PAIR-002**: The PEER_ACK frame MUST be encrypted with `node_psk` using the standard frame format (NEW-FRAME-001). Successful decryption by the node constitutes proof that the gateway holds `node_psk` (replacing the old `registration_proof` HMAC). CBOR payload: `{1: uint (status)}`. The `registration_proof` field (CBOR key 2) is removed.

### 3.7  `AeadProvider` Trait

**NEW-PROTO-001**: The `sonde-protocol` crate MUST define an `AeadProvider` trait (or extend existing traits) for AES-256-GCM encrypt/decrypt. Platform-specific implementations:
- Gateway/phone: `aes-gcm` crate (pure Rust).
- ESP32 node: `mbedtls` via ESP-IDF.

### 3.8  Updated Frame Overhead

**NEW-PROTO-002**: Frame overhead constants:
```
Current:  11 (header) + 32 (HMAC)    = 43 bytes overhead, 207 bytes payload
Proposed: 11 (header) + 16 (GCM tag) = 27 bytes overhead, 223 bytes payload
```

Each frame gains 16 bytes of usable payload.

---

## 4  Invariant Impact

### 4.1  Invariants Preserved

| Invariant | Status | Notes |
|-----------|--------|-------|
| Silent-discard error model | ✅ Preserved | AEAD failure → discard (same as HMAC failure). |
| Node as opaque relay | ✅ Preserved | Node stores and relays `encrypted_payload` without understanding it. |
| Phone PSK revocation | ✅ Preserved | Revoked PSKs excluded from AEAD candidate set. |
| Timestamp tolerance ±86 400 s | ✅ Preserved | PairingRequest timestamp check after decryption. |
| `key_hint` derivation | ✅ Preserved | `SHA-256(psk)[30..32]` for both phone and node PSKs. |
| Deterministic CBOR encoding | ✅ Preserved | PairingRequest CBOR unchanged. |
| Sequence-number replay protection | ✅ Preserved | Same semantics, now AEAD-protected. |
| BLE LESC requirement | ✅ Preserved | Still required for Phase 1 and Phase 2 BLE links. |

### 4.2  Invariants Modified

| Invariant | Change | Impact |
|-----------|--------|--------|
| Frame authentication is MAC-only (no encryption) | **Eliminated** | All payloads now encrypted. Passive radio observers can no longer read CBOR payloads (WAKE parameters, APP_DATA sensor readings, etc.). This is a **security improvement**. |
| Frame-level HMAC with `node_psk` for all messages | **Changed** | PEER_REQUEST uses `phone_psk`, all others use `node_psk`. Frame auth mechanism changes from HMAC to AES-GCM for all. |
| Gateway has asymmetric identity | **Eliminated** | No Ed25519 keypair, no `gateway_id`. Authority derives purely from possession of node key database. |
| Phone registration requires ECDH + HKDF | **Eliminated** | Phone generates PSK, sends it directly over BLE LESC. Much simpler. |
| `registration_proof` in PEER_ACK | **Eliminated** | AEAD encryption with `node_psk` provides equivalent proof. |
| Two-layer crypto for pairing (ECDH envelope + HMAC) | **Reduced** | Single AES-GCM layer for inner payload. Frame-level AES-GCM provides outer layer. |

### 4.3  Invariants Gained

| Invariant | Description |
|-----------|-------------|
| Payload confidentiality on all frames | All CBOR payloads encrypted. Sensor data, program hashes, battery levels no longer visible to passive observers. |
| Uniform crypto primitive | One algorithm (AES-256-GCM) for everything. Reduces implementation complexity and attack surface. |
| 16 bytes saved per frame | GCM tag (16B) vs HMAC (32B). Meaningful on 250B ESP-NOW frames. |
| Simpler gateway deployment | No keypair to generate, store, back up, or replicate. |
| Simpler phone registration | No challenge–response, no ECDH, no HKDF. |

---

## 5  Ripple Effects

### 5.1  sonde-protocol Crate (MAJOR)

This is the most impacted crate. The frame codec is its core responsibility.

- Frame encode: HMAC-SHA256 → AES-256-GCM encrypt. New GCM nonce construction.
- Frame decode: HMAC verification → AES-256-GCM decrypt. Tag verification built-in.
- `HmacProvider` trait: retire or deprecate. Add `AeadProvider` trait.
- `Sha256Provider` trait: retained for `key_hint` derivation and GCM nonce prefix.
- Constants: `AUTH_TAG_SIZE` 32→16, `FRAME_OVERHEAD` 43→27.
- All crates consuming `sonde-protocol` frames are affected by the trait change.

### 5.2  State Export/Import (GW-0805, GW-1001)

**Impact: Moderate.** Remove Ed25519 seed and `gateway_id` from export bundle. Simplifies state management. Migration needed for existing state files (if any exist).

### 5.3  Phone Pairing Store (PT-0800)

**Impact: Modified.** Remove `gw_public_key` and `gateway_id`. Only `phone_psk`, `phone_key_hint`, `rf_channel`, `phone_label` persisted.

### 5.4  Node Firmware (ND-*)

**Impact: Major.** Every frame encode/decode path changes. AES-256-GCM implementation required on ESP32 (available via `mbedtls` in ESP-IDF). PEER_REQUEST and PEER_ACK handling simplified but changed.

### 5.5  Gateway (GW-*)

**Impact: Major.** Frame codec changes. Keypair/identity code removed. BLE GATT simplified. PEER_REQUEST pipeline rewritten. PEER_ACK construction simplified.

### 5.6  Modem Firmware

**Impact: None.** Relays bytes opaquely.

### 5.7  sonde-admin CLI

**Impact: Minimal.** Pairing commands work via gRPC. No direct crypto changes.

### 5.8  E2E Tests

**Impact: Major.** All test helpers that construct/verify frames must switch to AES-256-GCM. Test vectors must be regenerated. PEER_REQUEST test paths need complete rewrite.

### 5.9  Encrypted Payload Size Budget

**Impact: Improved.**
```
Current inner payload (ECDH model):     New inner payload (PSK model):
  eph_public:      32 bytes               nonce:            12 bytes
  nonce:           12 bytes               GCM tag:          16 bytes
  GCM tag:         16 bytes               ─────────────────────────
  phone_key_hint:   2 bytes               Fixed overhead:   28 bytes
  phone_hmac:      32 bytes
  ─────────────────────────
  Fixed overhead:  94 bytes

ESP-NOW frame payload budget:           ESP-NOW frame payload budget:
  250 − 43 (frame overhead) = 207B        250 − 27 (frame overhead) = 223B
  207 − 94 (inner overhead) = 113B        223 − 28 (inner overhead) = 195B
  Available for CBOR: ~113 bytes          Available for CBOR: ~195 bytes
```

The new model reclaims **~82 bytes** for PairingRequest CBOR — a major improvement.

---

## 6  Open Questions

### OQ-1: PEER_REQUEST Frame Construction — Who Holds `phone_psk`?

The PEER_REQUEST frame is encrypted with `phone_psk`, but the node sends it over ESP-NOW. Two approaches:

**Option A — Node holds `phone_psk`:** Phone sends `phone_psk` to node in NODE_PROVISION (alongside `node_psk`). Node builds PEER_REQUEST frame with `phone_psk` for frame-level encryption, embeds `encrypted_payload` in CBOR.

**Option B — Phone builds complete frame:** Phone constructs the entire PEER_REQUEST frame bytes (header + AES-GCM encrypted CBOR + tag). Node stores the complete frame as an opaque blob and retransmits it verbatim. Node never needs `phone_psk`.

**[RECOMMENDATION]**: Option B preserves the "node as opaque relay" invariant. However, the phone must know the node's `key_hint` (derived from `phone_psk`, which the phone already has) and must construct a valid ESP-NOW frame. The frame nonce would be chosen by the phone and the node would retransmit the same bytes each time (same nonce → same ciphertext).

**[CONCERN]**: Retransmitting the exact same encrypted frame on every boot is safe because AES-GCM with the same key+nonce+plaintext produces the same ciphertext — but a radio observer can tell it's a retransmission. This is acceptable given the current design already retransmits identical PEER_REQUEST frames.

### OQ-2: Existing Implementations

Are there existing implementations of the HMAC-based frame codec or ECDH-based pairing that need migration?

**[STATUS]**: The `sonde-protocol` crate exists with HMAC-based frame codec. The `sonde-pair` crate is planned (see issue #163). Migration of `sonde-protocol` frame codec is the primary implementation task.

---

## 7  Summary Statistics

| Category | Count |
|----------|-------|
| **Documents affected** | **6 of 6** spec docs (protocol.md, ble-pairing-protocol.md, security.md, ble-pairing-tool-requirements.md, gateway-requirements.md, node-requirements.md) |
| REQ-IDs RETIRED | 16 (PT-0301, PT-0302, PT-0404, PT-0405, PT-0902, PT-1101, GW-1200, GW-1201, GW-1202, GW-1203, GW-1206, GW-1213, GW-1214, ND-0300†, ND-0301†, ND-0912†) |
| REQ-IDs MODIFIED (major) | 18 (GW-0600, GW-0603, GW-1209, GW-1211, GW-1212, GW-1219, GW-1220, GW-0805, GW-1000, GW-1001, ND-0102, ND-0103, ND-0304, ND-0909, PT-0303, PT-0800, PT-1100, PT-1102) |
| REQ-IDs MODIFIED (minor) | 10 (GW-0601a, GW-1204, GW-1210, GW-1217, ND-0915, ND-0916, PT-0304, PT-0406, PT-0407, PT-1201–PT-1204) |
| REQ-IDs UNAFFECTED | 40+ |
| New requirements | 8 (NEW-FRAME-001/002/003, NEW-REG-001/002, NEW-PAIR-001/002, NEW-PROTO-001) |
| Open questions | 2 |
| **Crates affected** | **5** (sonde-protocol, sonde-gateway, sonde-node, sonde-pair, sonde-e2e) |

† ND-0300, ND-0301, ND-0912 are functionally replaced (HMAC → AES-GCM) rather than deleted — listed as RETIRED because the HMAC-specific requirement text must be fully rewritten.

---

## 8  Recommended Next Steps

1. **Proceed to Phase 2** (specification changes) — update all 6 spec documents per this manifest.
2. **Implement `AeadProvider` trait** in `sonde-protocol` alongside existing `HmacProvider` for incremental migration.
3. **Update frame codec** in `sonde-protocol` — this is the critical path. All other crate changes depend on it.
4. **Draft test vectors** for AES-256-GCM frame format with PSK-derived GCM nonce prefix.
5. **Verify ESP32 AES-GCM availability** via `mbedtls` in ESP-IDF for node firmware.
6. **Assess `sonde-protocol` migration strategy** — feature flag for gradual rollout or clean break.
