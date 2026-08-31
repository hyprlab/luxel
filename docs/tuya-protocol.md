# Tuya LAN protocol notes

`src/tuya/` implements the Tuya local protocol used by SmartLife devices:
UDP discovery broadcasts on ports 6666/6667 and control over TCP port 6668.
The reference for all of it is [tinytuya](https://github.com/jasonacox/tinytuya)
(`tinytuya/core/`), which this implementation follows.

## Wire versions

| | 3.3 | 3.4 | 3.5 |
|---|---|---|---|
| Frame | `55AA` | `55AA` | `6699` |
| Integrity | CRC32 | HMAC-SHA256 | AES-GCM tag |
| Payload crypto | AES-128-ECB, local key | AES-128-ECB, session key | AES-128-GCM, session key |
| Version header | plaintext, outside ciphertext | inside ciphertext | inside ciphertext |
| Set dps | `CONTROL` (0x07) | `CONTROL_NEW` (0x0d), protocol-5 JSON | same as 3.4 |
| Query dps | `DP_QUERY` (0x0a) | `DP_QUERY_NEW` (0x10), `{}` | same as 3.4 |

The version header is `"3.x" + 12 zero bytes` and is skipped for
`DP_QUERY`, `DP_QUERY_NEW`, `UPDATEDPS`, `HEART_BEAT`, the three session
negotiation commands and `LAN_EXT_STREAM`.

Frames received from a device carry a 4-byte return code before the
payload (inside the GCM plaintext for 3.5).

## Session key negotiation (3.4/3.5)

1. Client sends `SESS_KEY_NEG_START` (0x03) with a random 16-byte nonce
   (ECB-encrypted with the local key on 3.4; GCM on 3.5).
2. Device answers `SESS_KEY_NEG_RESP` (0x04): its own 16-byte nonce
   followed by `HMAC-SHA256(local_key, client_nonce)`.
3. Client verifies and sends `SESS_KEY_NEG_FINISH` (0x05) containing
   `HMAC-SHA256(local_key, device_nonce)`.
4. Both sides derive `xored = client_nonce XOR device_nonce`;
   the session key is `AES-ECB(local_key, xored)` on 3.4, and on 3.5 the
   first 16 ciphertext bytes of `AES-GCM(local_key, iv=client_nonce[..12],
   xored)`.

Until negotiation completes, frames are keyed with the local key; after,
with the session key.

## UDP discovery

Devices broadcast a JSON blob (`gwId`, `ip`, `version`, …) every ~5 s:
plaintext on port 6666 (3.1 firmware), AES-ECB on port 6667 with the fixed
key `md5("yGAdlopoPVldABfn")`, and GCM-framed `6699` on 6667 for 3.5.
Broadcasts don't cross subnets, so Luxel only uses them opportunistically
to refresh the IP/version of already-configured devices.

## Test vectors

The unit tests in `src/tuya/protocol.rs` pin every encode/decode path to
byte-exact golden vectors generated with tinytuya 1.20 (fixed key, nonces,
timestamp and GCM IV) by [`gen-tuya-vectors.py`](gen-tuya-vectors.py)
(`pip install tinytuya` and run it to regenerate). Local keys are always 16 ASCII characters; the
Tuya cloud is the only source for them (`tinytuya wizard` fetches them for
all devices on a SmartLife account).
