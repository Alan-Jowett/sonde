<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Why Aren’t We Using Standard IETF IoT Protocols?

This project deliberately **does not** use several well‑known IETF IoT security protocols. This is not because we are unaware of them. It is because **they do not solve the actual problem we have**.

This document answers the most common “why didn’t you just use X?” questions.

---

## Why not **EDHOC** (RFC 9528 / LAKE WG)?

**Short answer:** EDHOC is a *key exchange*, not a *device onboarding or pairing* protocol.

**What EDHOC does well**

*   Compact, formally analyzed authenticated key exchange
*   Excellent for constrained devices
*   Works well *once identities and credentials already exist*

**Why it doesn’t fit here**

*   EDHOC assumes you already know *who you are talking to*
*   It does **not** define:
    *   Device discovery
    *   Human intent (“am I pairing *this* device?”)
    *   Delegation via a phone acting as a pairing agent
*   It does not address BLE identity ambiguity or headless devices
*   It assumes an IP‑like transport (CoAP/UDP), not ESP‑NOW or raw BLE GATT

**Bottom line:** EDHOC could replace *some cryptographic plumbing*, but it does **not** solve the hard part of this system: **secure human‑mediated onboarding of anonymous devices**.

---

## Why not **OSCORE** (RFC 8613)?

**Short answer:** OSCORE secures messages *after* pairing. We need to secure pairing itself.

**What OSCORE does well**

*   End‑to‑end object security for CoAP
*   Replay protection
*   Proxy‑friendly

**Why it doesn’t fit here**

*   OSCORE explicitly does **not** define key establishment
*   It assumes keys already exist (from EDHOC, DTLS, OOB provisioning, etc.)
*   It does not help with:
    *   First contact
    *   Device identity
    *   Trust bootstrap

**Bottom line:** OSCORE is useful *after* onboarding. Our problem is onboarding. And even post‑onboarding, OSCORE assumes CoAP semantics (request/response with tokens and options) that do not map onto our fire‑and‑forget ESP‑NOW frame model. Our post‑pairing protocol uses AES‑256‑GCM AEAD for both authentication and encryption — see [security.md §1.3](security.md#13--out-of-scope-threats) for the explicit threat model.

---

## Why not **DTLS / TLS**?

**Short answer:** Too heavy, wrong abstraction, wrong transport.

**Why not**

*   Handshake sizes are large relative to ESP‑NOW (250 B max frame) and BLE MTUs
*   Requires maintaining session state on constrained nodes that sleep between every exchange
*   Assumes stable, bidirectional, IP‑style connectivity — ESP‑NOW is a raw Layer 2 broadcast protocol with no IP stack
*   DTLS 1.3 (RFC 9147) with Connection ID (RFC 9146) helps with NAT rebinding and sleep/wake, but still requires a full TLS record layer and handshake machinery that exceeds our frame budget
*   Adds complexity without solving identity or user‑intent problems

**Bottom line:** DTLS/TLS solve transport security, not physical‑world device pairing.

---

## Why not **BLE Security / SMP / LESC**?

**Short answer:** BLE secures links, not *identities*.

**What BLE gives you**

*   Encrypted transport
*   Optional MITM resistance (Numeric Comparison / Passkey / OOB)

**What BLE does *not* give you**

*   A reliable notion of “this is the device the user intends”
*   Any semantic identity for headless devices
*   Any gateway‑anchored trust model
*   Delegated authorization (phone → gateway → node)

**Important note**
Even with LE Secure Connections, BLE pairing only proves:

> “The two endpoints completed a cryptographic handshake.”

It does **not** prove:

> “This device is the one the user thinks it is.”

**Bottom line:** BLE is treated as an *untrusted transport* for identity. Higher‑level protocol logic is still required.

---

## Why not **CoAP over GATT** (draft‑amsuess‑core‑coap‑over‑gatt)?

**Short answer:** It’s a transport mapping, not a security or onboarding solution.

**Why not**

*   Defines how to run CoAP over BLE GATT
*   Does not define:
    *   Pairing semantics
    *   Trust bootstrap
    *   Human interaction model
*   Simply moves the problem up a layer

**Bottom line:** Transport ≠ trust.

---

## Why not **manufacturer certificates / PKI / vouchers**?

**Short answer:** Overkill, fragile, and incompatible with our deployment model.

**Why not**

*   Requires factory‑installed identities
*   Complicates manufacturing and key management
*   Poor fit for low‑cost, battery‑powered, intermittently connected nodes
*   Still does not solve *human intent* during pairing

**Bottom line:** We intentionally use **late binding** at the gateway, not factory trust. See the BRSKI section below for why the IETF's specific protocol for voucher‑based onboarding (RFC 8995) also does not fit.

---

## Why not **BRSKI** (RFC 8995 / ANIMA WG)?

**Short answer:** BRSKI solves onboarding for devices that ship with manufacturer certificates. Ours don't.

**What BRSKI does well**

*   Automated "zero‑touch" secure onboarding
*   Voucher‑based proof of device provenance (MASA → Registrar → Pledge)
*   Strong identity assurance via factory‑installed IDevID certificates

**Why it doesn't fit here**

*   BRSKI's entire trust model is rooted in **manufacturer‑issued X.509 identity certificates (IDevID)**. Our nodes are generic, low‑cost hardware with no factory PKI.
*   Requires a **MASA** (Manufacturer Authorized Signing Authority) — a cloud service operated by the device manufacturer. We have no manufacturer in the loop.
*   Assumes IP connectivity between Pledge, Registrar, and MASA. Our nodes speak ESP‑NOW (Layer 2 broadcast, no IP).
*   The Join Proxy model assumes the new device can reach the Registrar over the network. Our nodes cannot — the phone acts as the relay.
*   BRSKI does not address **human intent** ("am I pairing *this* device?"). It is designed for environments where any device with a valid IDevID should be accepted automatically.

**Bottom line:** BRSKI is excellent for enterprise/industrial deployments with supply‑chain identity. We have anonymous, certificate‑free devices and need explicit human‑mediated pairing. The trust anchors are fundamentally different.

---

## Why not **ACE‑OAuth** (RFC 9200)?

**Short answer:** ACE authorizes *access to resources*. We need to *establish identity from nothing*.

**What ACE does well**

*   Adapts OAuth 2.0 for constrained environments (CBOR/CoAP instead of JSON/HTTP)
*   Token‑based access control with delegated authorization servers
*   Works with DTLS and OSCORE profiles for transport security

**Why it doesn't fit here**

*   ACE assumes an **authorization server** that already knows which clients and resource servers exist. We are bootstrapping that knowledge.
*   Token issuance requires the client to already have credentials (or a pre‑existing trust relationship with the AS).
*   ACE's resource model (CoAP GET/PUT/POST on URIs) does not map onto our frame‑level protocol.
*   The phone‑as‑delegated‑agent pattern in our system is closer to a one‑time provisioning act than an ongoing authorization grant.

**Bottom line:** ACE solves "should this already‑known client access this resource?" We solve "who is this device and should the gateway trust it at all?"

---

## Why not **COSE** (RFC 9052) instead of raw AES‑256‑GCM?

**Short answer:** COSE adds structure we don't need, at a cost we can't afford.

**What COSE does well**

*   Standardized CBOR envelope for signatures, MACs, and encryption
*   Self‑describing: carries algorithm identifiers, key hints, and headers
*   Designed for interoperability across vendors

**Why we use raw AES‑256‑GCM instead**

*   Our frame budget is **250 bytes** (ESP‑NOW). After the 11‑byte header and 16‑byte AEAD tag, we have 223 bytes for payload. A `COSE_Encrypt0` wrapper adds ~10–15 bytes of CBOR structure (protected headers, unprotected headers, payload wrapping) that directly reduce usable payload.
*   We have **exactly one algorithm** (AES‑256‑GCM) and **exactly one key‑hint scheme** (`key_hint` in the binary header). COSE's self‑describing flexibility is overhead with no benefit.
*   The system is **closed** — nodes and gateway are both Sonde software. There is no third‑party interoperability requirement.
*   Every byte matters on a battery‑powered device transmitting at 1 Mbps with a duty‑cycle budget.

**Bottom line:** COSE is the right choice for multi‑vendor ecosystems. For a closed, single‑algorithm, frame‑constrained system, raw AES‑256‑GCM is smaller, simpler, and equally secure.

---

## Why not **SUIT** (RFC 9019 / RFC 9124) for program distribution?

**Short answer:** SUIT secures firmware images. We distribute 200‑byte BPF programs, not firmware.

**What SUIT does well**

*   Manifest‑based integrity and authenticity for firmware updates
*   Rollback protection via monotonic sequence numbers
*   Multi‑image, multi‑component update orchestration

**Why it doesn't fit here**

*   SUIT manifests are designed for **firmware images** (tens of KB to MB). Our BPF programs are typically **100–300 bytes** of CBOR‑encoded bytecode. The SUIT manifest envelope would be larger than the payload.
*   SUIT assumes the device stores and manages multiple firmware slots with install/apply/revert semantics. Our nodes have a single program slot with no rollback — the gateway simply sends a new program.
*   Program integrity is already verified: `program_hash` = SHA‑256 of the CBOR image, checked by the node before execution.
*   SUIT's transport model assumes the device can fetch payloads from a URI. Our nodes receive programs via gateway‑pushed ESP‑NOW chunks — no pull capability.

**Bottom line:** SUIT solves a real problem, but for payloads 100–1000× larger than ours, on devices with richer storage semantics. We use a simpler hash‑verified push model that fits the constraint.

---

## So what *are* we doing instead?

We use a **system‑level onboarding protocol** with explicit trust boundaries:

*   **Gateway is the root of trust**
*   **Phone is a delegated pairing agent**
*   **Nodes are deliberately dumb**
*   **BLE is treated as an untrusted transport**
*   **All real trust decisions happen at the gateway**

This is a **composition** of well‑understood primitives (AEAD, CBOR), not a reinvention of cryptography. The threat model and security assumptions are documented explicitly in [security.md](security.md), including what is *not* protected (physical jamming, gateway compromise). This is not a substitute for formal analysis, but it is an honest and auditable starting point.

---

## Is this ideal?

No.

## Is there an IETF standard that solves this end‑to‑end today?

Also no.

## Is this approach explicit, honest about its threat model, and appropriate for the constraints?

Yes.

---

## TL;DR

If you are looking for:

*   A formally analyzed key exchange → **EDHOC**
*   End‑to‑end message security → **OSCORE**
*   Encrypted BLE links → **LESC**

Those exist.

If you are looking for:

> “A human with a phone securely onboarding a headless, anonymous, battery‑powered device via BLE into a gateway‑anchored trust domain”

That protocol does **not** exist in the IETF.

So we built one — carefully.

---

## When would we adopt IETF standards?

We are not opposed to IETF protocols — we actively use CBOR (RFC 8949) and AES‑256‑GCM (NIST SP 800‑38D). The question is always whether the standard solves *our* problem or an adjacent one.

Realistic adoption paths:

*   **EDHOC** — If a future version supports non‑IP transports and human‑mediated pairing, EDHOC could replace the key agreement inside our BLE pairing flow. We would still need the surrounding orchestration.
*   **OSCORE** — If the post‑pairing protocol ever needs confidentiality (e.g., actuator commands), OSCORE or a COSE‑based AEAD envelope would be a natural fit.
*   **COSE** — If the system opens to third‑party devices or multi‑vendor interoperability, self‑describing security envelopes become worth the overhead.
*   **SUIT** — If program images grow significantly or require rollback/multi‑slot management, SUIT manifests would be appropriate.
*   **BRSKI** — If we ever manufacture devices with factory‑provisioned identities and need automated fleet onboarding, BRSKI would be the natural fit. Our current model (anonymous devices, human‑mediated pairing) is a deliberate design choice, not a cost shortcut.
*   **ACE** — If the gateway exposes a richer resource model (e.g., CoAP endpoints for node management), ACE tokens could replace the current PSK‑only authorization.

---

## References

| Standard | RFC / Draft | Relevant WG |
|---|---|---|
| EDHOC | RFC 9528 | LAKE |
| OSCORE | RFC 8613 | CoRE |
| DTLS 1.3 | RFC 9147 | TLS |
| DTLS 1.3 CID | RFC 9146 | TLS |
| BLE SMP | Bluetooth Core Spec v5.x, Part H | Bluetooth SIG |
| BRSKI | RFC 8995 | ANIMA |
| ACE‑OAuth | RFC 9200 | ACE |
| COSE | RFC 9052, RFC 9053 | COSE |
| SUIT | RFC 9019, RFC 9124 | SUIT |
| CBOR | RFC 8949 | CBOR |
| CoAP over GATT | draft‑amsuess‑core‑coap‑over‑gatt | CoRE |
| AES‑256‑GCM | NIST SP 800‑38D | — |
