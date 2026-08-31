//! Tuya local ("SmartLife") protocol: framing, crypto and session keys.
//!
//! Tuya devices speak a framed protocol on TCP port 6668. Three wire
//! versions are in the wild on modern devices:
//!
//! - 3.3: `55AA` frames, payload AES-128-ECB encrypted with the device's
//!   local key, CRC32 integrity. Some commands carry a *plaintext*
//!   `"3.3" + 12 zero bytes` header before the ciphertext.
//! - 3.4: `55AA` frames, HMAC-SHA256 integrity, payloads encrypted with a
//!   session key negotiated per connection (the version header, where
//!   present, is encrypted along with the payload).
//! - 3.5: `6699` frames, AES-128-GCM with the same session-key negotiation.
//!
//! The framing and crypto here are validated byte-for-byte against
//! tinytuya (the reference Python implementation) in the tests below.

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use aes_gcm::aead::AeadInPlace;
use aes_gcm::{Aes128Gcm, Nonce, Tag};
use hmac::{Hmac, Mac};
use sha2::Sha256;

// Command types (lan_protocol.h names).
pub const SESS_KEY_NEG_START: u32 = 3;
pub const SESS_KEY_NEG_RESP: u32 = 4;
pub const SESS_KEY_NEG_FINISH: u32 = 5;
pub const CONTROL: u32 = 7;
#[cfg(test)]
pub const STATUS: u32 = 8;
pub const HEART_BEAT: u32 = 9;
pub const DP_QUERY: u32 = 0x0a;
pub const CONTROL_NEW: u32 = 0x0d;
pub const DP_QUERY_NEW: u32 = 0x10;
pub const UPDATEDPS: u32 = 0x12;
pub const LAN_EXT_STREAM: u32 = 0x40;

const PREFIX_55AA: u32 = 0x0000_55AA;
const SUFFIX_55AA: u32 = 0x0000_AA55;
const PREFIX_6699: u32 = 0x0000_6699;
const SUFFIX_6699: u32 = 0x0000_9966;
const MAX_PAYLOAD: usize = 4096;

/// MD5 of b"yGAdlopoPVldABfn": the fixed key Tuya devices use to encrypt
/// UDP discovery broadcasts on port 6667.
pub const UDP_KEY: [u8; 16] = [
    0x6c, 0x1e, 0xc8, 0xe2, 0xbb, 0x9b, 0xb5, 0x9a, 0xb5, 0x0b, 0x0d, 0xaf, 0x64, 0x9b, 0x41,
    0x0a,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V33,
    V34,
    V35,
}

impl Version {
    fn header(self) -> [u8; 15] {
        let mut h = [0u8; 15];
        h[..3].copy_from_slice(match self {
            Version::V33 => b"3.3",
            Version::V34 => b"3.4",
            Version::V35 => b"3.5",
        });
        h
    }

    pub fn parse(s: &str) -> Option<Version> {
        match s {
            "3.3" => Some(Version::V33),
            "3.4" => Some(Version::V34),
            "3.5" => Some(Version::V35),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Version::V33 => "3.3",
            Version::V34 => "3.4",
            Version::V35 => "3.5",
        }
    }
}

/// Commands whose payload is sent without the "3.x" version header.
fn no_version_header(cmd: u32) -> bool {
    matches!(
        cmd,
        DP_QUERY
            | DP_QUERY_NEW
            | UPDATEDPS
            | HEART_BEAT
            | SESS_KEY_NEG_START
            | SESS_KEY_NEG_RESP
            | SESS_KEY_NEG_FINISH
            | LAN_EXT_STREAM
    )
}

/// A received message, decrypted down to its (usually JSON) payload.
#[derive(Debug, Clone)]
pub struct Msg {
    #[allow(dead_code)]
    pub seqno: u32,
    pub cmd: u32,
    pub retcode: u32,
    pub payload: Vec<u8>,
}

pub fn ecb_encrypt(key: &[u8; 16], data: &[u8], pad: bool) -> Vec<u8> {
    let cipher = Aes128::new(key.into());
    let mut buf = data.to_vec();
    if pad {
        let padlen = 16 - buf.len() % 16;
        buf.resize(buf.len() + padlen, padlen as u8);
    }
    debug_assert!(buf.len().is_multiple_of(16));
    for block in buf.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(block.into());
    }
    buf
}

pub fn ecb_decrypt(key: &[u8; 16], data: &[u8], unpad: bool) -> Option<Vec<u8>> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return None;
    }
    let cipher = Aes128::new(key.into());
    let mut buf = data.to_vec();
    for block in buf.as_chunks_mut::<16>().0 {
        cipher.decrypt_block(block.into());
    }
    if unpad {
        let padlen = *buf.last()? as usize;
        if !(1..=16).contains(&padlen) || padlen > buf.len() {
            return None;
        }
        buf.truncate(buf.len() - padlen);
    }
    Some(buf)
}

pub fn hmac_sha256(key: &[u8; 16], data: &[u8]) -> [u8; 32] {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn gcm_encrypt(key: &[u8; 16], nonce: &[u8; 12], aad: &[u8], plain: &[u8]) -> (Vec<u8>, [u8; 16]) {
    let cipher = Aes128Gcm::new(key.into());
    let mut buf = plain.to_vec();
    let tag = cipher
        .encrypt_in_place_detached(Nonce::from_slice(nonce), aad, &mut buf)
        .expect("gcm encrypt cannot fail");
    (buf, tag.into())
}

fn gcm_decrypt(
    key: &[u8; 16],
    nonce: &[u8; 12],
    aad: &[u8],
    ct: &[u8],
    tag: &[u8; 16],
) -> Option<Vec<u8>> {
    let cipher = Aes128Gcm::new(key.into());
    let mut buf = ct.to_vec();
    cipher
        .decrypt_in_place_detached(Nonce::from_slice(nonce), aad, &mut buf, Tag::from_slice(tag))
        .ok()?;
    Some(buf)
}

fn random_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).expect("os rng");
    nonce
}

pub fn random_local_nonce() -> [u8; 16] {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).expect("os rng");
    nonce
}

fn pack_55aa(seqno: u32, cmd: u32, payload: &[u8], hmac_key: Option<&[u8; 16]>) -> Vec<u8> {
    let end_len = if hmac_key.is_some() { 36 } else { 8 };
    let mut out = Vec::with_capacity(16 + payload.len() + end_len);
    out.extend_from_slice(&PREFIX_55AA.to_be_bytes());
    out.extend_from_slice(&seqno.to_be_bytes());
    out.extend_from_slice(&cmd.to_be_bytes());
    out.extend_from_slice(&((payload.len() + end_len) as u32).to_be_bytes());
    out.extend_from_slice(payload);
    match hmac_key {
        Some(key) => out.extend_from_slice(&hmac_sha256(key, &out)),
        None => out.extend_from_slice(&crc32fast::hash(&out).to_be_bytes()),
    }
    out.extend_from_slice(&SUFFIX_55AA.to_be_bytes());
    out
}

fn pack_6699(seqno: u32, cmd: u32, payload: &[u8], key: &[u8; 16], nonce: &[u8; 12]) -> Vec<u8> {
    let mut out = Vec::with_capacity(18 + 12 + payload.len() + 16 + 4);
    out.extend_from_slice(&PREFIX_6699.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&seqno.to_be_bytes());
    out.extend_from_slice(&cmd.to_be_bytes());
    out.extend_from_slice(&((12 + payload.len() + 16) as u32).to_be_bytes());
    let aad = out[4..18].to_vec();
    let (ct, tag) = gcm_encrypt(key, nonce, &aad, payload);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ct);
    out.extend_from_slice(&tag);
    out.extend_from_slice(&SUFFIX_6699.to_be_bytes());
    out
}

/// Encoder/decoder for one connection. `key` is the device's local key for
/// 3.3 (and during 3.4/3.5 session negotiation), the session key afterwards.
pub struct Codec {
    pub version: Version,
    pub key: [u8; 16],
}

impl Codec {
    /// Frame, encrypt and version-tag one outgoing command.
    pub fn encode(&self, seqno: u32, cmd: u32, payload: &[u8]) -> Vec<u8> {
        match self.version {
            Version::V33 => {
                let mut body = Vec::new();
                if !no_version_header(cmd) {
                    body.extend_from_slice(&self.version.header());
                }
                body.extend_from_slice(&ecb_encrypt(&self.key, payload, true));
                pack_55aa(seqno, cmd, &body, None)
            }
            Version::V34 => {
                let mut plain = Vec::new();
                if !no_version_header(cmd) {
                    plain.extend_from_slice(&self.version.header());
                }
                plain.extend_from_slice(payload);
                let body = ecb_encrypt(&self.key, &plain, true);
                pack_55aa(seqno, cmd, &body, Some(&self.key))
            }
            Version::V35 => {
                let mut plain = Vec::new();
                if !no_version_header(cmd) {
                    plain.extend_from_slice(&self.version.header());
                }
                plain.extend_from_slice(payload);
                pack_6699(seqno, cmd, &plain, &self.key, &random_nonce())
            }
        }
    }

    /// Pull the next complete frame out of `buf` (removing its bytes) and
    /// decrypt it. `Ok(None)` means more data is needed; `Err` means the
    /// stream is corrupt or keyed wrongly and the connection should drop.
    pub fn decode_next(&self, buf: &mut Vec<u8>) -> Result<Option<Msg>, String> {
        // Resynchronize on a frame prefix.
        let pos = buf
            .windows(4)
            .position(|w| w == PREFIX_55AA.to_be_bytes() || w == PREFIX_6699.to_be_bytes());
        match pos {
            Some(0) => {}
            Some(p) => {
                buf.drain(..p);
            }
            None => {
                let keep = buf.len().min(3);
                buf.drain(..buf.len() - keep);
                return Ok(None);
            }
        }
        if buf.len() < 4 {
            return Ok(None);
        }
        if buf[..4] == PREFIX_6699.to_be_bytes() {
            self.decode_6699(buf)
        } else {
            self.decode_55aa(buf)
        }
    }

    fn decode_55aa(&self, buf: &mut Vec<u8>) -> Result<Option<Msg>, String> {
        if buf.len() < 16 {
            return Ok(None);
        }
        let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        let seqno = be(&buf[4..8]);
        let cmd = be(&buf[8..12]);
        let length = be(&buf[12..16]) as usize;
        if length > MAX_PAYLOAD {
            return Err(format!("oversized 55AA frame ({length} bytes)"));
        }
        let total = 16 + length;
        if buf.len() < total {
            return Ok(None);
        }
        let frame: Vec<u8> = buf.drain(..total).collect();
        let end_len = if self.version == Version::V34 { 36 } else { 8 };
        if length < 4 + end_len {
            return Err("truncated 55AA frame".into());
        }
        let retcode = be(&frame[16..20]);
        let body = &frame[20..total - end_len];
        let signed = &frame[..total - end_len];
        match self.version {
            Version::V34 => {
                let want = hmac_sha256(&self.key, signed);
                if frame[total - end_len..total - 4] != want {
                    return Err("HMAC mismatch (wrong key or protocol version?)".into());
                }
            }
            _ => {
                let want = crc32fast::hash(signed);
                if be(&frame[total - end_len..total - 4]) != want {
                    return Err("CRC mismatch".into());
                }
            }
        }
        let payload = match self.version {
            Version::V34 => {
                let mut plain = ecb_decrypt(&self.key, body, true)
                    .ok_or("payload decrypt failed (wrong key?)")?;
                if plain.starts_with(b"3.") && plain.len() >= 15 {
                    plain.drain(..15);
                }
                plain
            }
            _ => {
                let mut body = body;
                if body.starts_with(b"3.") && body.len() >= 15 {
                    body = &body[15..];
                }
                if body.is_empty() {
                    Vec::new()
                } else if body.starts_with(b"{") {
                    // Some responses (errors, heartbeats) are plaintext JSON.
                    body.to_vec()
                } else {
                    ecb_decrypt(&self.key, body, true)
                        .ok_or("payload decrypt failed (wrong key?)")?
                }
            }
        };
        Ok(Some(Msg {
            seqno,
            cmd,
            retcode,
            payload,
        }))
    }

    fn decode_6699(&self, buf: &mut Vec<u8>) -> Result<Option<Msg>, String> {
        if buf.len() < 18 {
            return Ok(None);
        }
        let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        let seqno = be(&buf[6..10]);
        let cmd = be(&buf[10..14]);
        let length = be(&buf[14..18]) as usize;
        if length > MAX_PAYLOAD {
            return Err(format!("oversized 6699 frame ({length} bytes)"));
        }
        let total = 18 + length + 4;
        if buf.len() < total {
            return Ok(None);
        }
        let frame: Vec<u8> = buf.drain(..total).collect();
        if length < 12 + 16 {
            return Err("truncated 6699 frame".into());
        }
        let aad = &frame[4..18];
        let nonce: [u8; 12] = frame[18..30].try_into().unwrap();
        let ct = &frame[30..18 + length - 16];
        let tag: [u8; 16] = frame[18 + length - 16..18 + length].try_into().unwrap();
        let mut plain = gcm_decrypt(&self.key, &nonce, aad, ct, &tag)
            .ok_or("GCM decrypt failed (wrong key or protocol version?)")?;
        // Device->client payloads carry a 4-byte return code first.
        let retcode = if plain.len() >= 4 && !plain.starts_with(b"{") {
            let rc = be(&plain[..4]);
            plain.drain(..4);
            rc
        } else {
            0
        };
        if plain.starts_with(b"3.") && plain.len() >= 15 {
            plain.drain(..15);
        }
        Ok(Some(Msg {
            seqno,
            cmd,
            retcode,
            payload: plain,
        }))
    }
}

/// Check a SESS_KEY_NEG_RESP payload (remote nonce + HMAC of our nonce) and
/// return the device's nonce.
pub fn verify_neg_resp(
    local_key: &[u8; 16],
    local_nonce: &[u8; 16],
    payload: &[u8],
) -> Option<[u8; 16]> {
    if payload.len() < 48 {
        return None;
    }
    let remote_nonce: [u8; 16] = payload[..16].try_into().ok()?;
    if hmac_sha256(local_key, local_nonce)[..] != payload[16..48] {
        return None;
    }
    Some(remote_nonce)
}

/// Derive the session key from the two negotiation nonces.
pub fn session_key(
    version: Version,
    local_key: &[u8; 16],
    local_nonce: &[u8; 16],
    remote_nonce: &[u8; 16],
) -> [u8; 16] {
    let mut xored = [0u8; 16];
    for i in 0..16 {
        xored[i] = local_nonce[i] ^ remote_nonce[i];
    }
    match version {
        Version::V35 => {
            let nonce: [u8; 12] = local_nonce[..12].try_into().unwrap();
            let (ct, _) = gcm_encrypt(local_key, &nonce, &[], &xored);
            ct.try_into().unwrap()
        }
        _ => ecb_encrypt(local_key, &xored, false).try_into().unwrap(),
    }
}

/// Decode a UDP discovery broadcast (ports 6666/6667) into its JSON payload.
pub fn parse_udp(data: &[u8]) -> Option<serde_json::Value> {
    let json = |bytes: &[u8]| -> Option<serde_json::Value> {
        serde_json::from_slice(bytes.trim_ascii_end()).ok()
    };
    if data.len() >= 4 && data[..4] == PREFIX_6699.to_be_bytes() {
        // 3.5 broadcast: GCM with the fixed UDP key.
        let codec = Codec {
            version: Version::V35,
            key: UDP_KEY,
        };
        let mut buf = data.to_vec();
        let msg = codec.decode_next(&mut buf).ok()??;
        return json(&msg.payload);
    }
    if data.len() >= 4 && data[..4] == PREFIX_55AA.to_be_bytes() {
        // Strip header+retcode and CRC+suffix; plaintext on 6666, ECB with
        // the fixed UDP key on 6667.
        if data.len() < 28 {
            return None;
        }
        let body = &data[20..data.len() - 8];
        if body.starts_with(b"{") {
            return json(body);
        }
        return json(&ecb_decrypt(&UDP_KEY, body, true)?);
    }
    if data.starts_with(b"{") {
        return json(data);
    }
    json(&ecb_decrypt(&UDP_KEY, data, true)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Golden vectors generated with tinytuya 1.20 (see docs/tuya-protocol.md):
    // key "0123456789abcdef", device id "vdevo123456789abcdefg", t=1700000000,
    // local nonce "aaaabbbbccccdddd", remote nonce "eeeeffffgggghhhh".
    const KEY: &[u8; 16] = b"0123456789abcdef";
    const LOCAL_NONCE: &[u8; 16] = b"aaaabbbbccccdddd";
    const REMOTE_NONCE: &[u8; 16] = b"eeeeffffgggghhhh";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn v33_encode_matches_tinytuya() {
        let codec = Codec { version: Version::V33, key: *KEY };
        let control = br#"{"devId":"vdevo123456789abcdefg","uid":"vdevo123456789abcdefg","t":"1700000000","dps":{"1":true}}"#;
        assert_eq!(
            codec.encode(1, CONTROL, control),
            hex("000055aa000000010000000700000087332e33000000000000000000000000f3b9cbb02721d2b0d7a136b5fb907f738adc8b6a6cd7513e048abc68c03525caf79274b86f3818211130d7120d867abcdc295e30d4d5f007f9aad3443b6c681077a161492d5232dcc0b97c0f2e2ad0e841fb8ef70a70a27b93a7e74ffe75426303000aeb4ff51d29188f82e4a162a1f6c768c8e90000aa55"),
        );
        let query = br#"{"gwId":"vdevo123456789abcdefg","devId":"vdevo123456789abcdefg","uid":"vdevo123456789abcdefg","t":"1700000000"}"#;
        assert_eq!(
            codec.encode(2, DP_QUERY, query),
            hex("000055aa000000020000000a000000780fd4350c17b993bc42d781f30be4f49089c607163a6b19db966eb6816a6ed975069852fe991dbad975609bcb8237597c89c607163a6b19db966eb6816a6ed975345de95f787fb8709c4df989277fd82a41eabae29f80fcd03d81886580e7ceea6e595054990a5d2a0779fa43f44ecd47df81ec540000aa55"),
        );
    }

    #[test]
    fn v33_decode_response_and_push() {
        let codec = Codec { version: Version::V33, key: *KEY };
        let mut buf = hex("000055aa000000020000000a0000002c00000000c27ee03f8be481e63320de7dc6eb4296eaa232ae24f922bc7c13d00b5180beaaa9c1a59c0000aa55");
        let msg = codec.decode_next(&mut buf).unwrap().unwrap();
        assert_eq!(msg.cmd, DP_QUERY);
        assert_eq!(msg.retcode, 0);
        assert_eq!(msg.payload, br#"{"dps":{"1":true,"9":0}}"#);
        assert!(buf.is_empty());

        // Async status push carries a plaintext version header.
        let mut buf = hex("000055aa00000003000000080000005b00000000332e33000000000000000000000000f3b9cbb02721d2b0d7a136b5fb907f738adc8b6a6cd7513e048abc68c03525ca12455835ab3bf333b6ae985f9771327531836d84bd28cb4b20619eb84882ca2a27b87e440000aa55");
        let msg = codec.decode_next(&mut buf).unwrap().unwrap();
        assert_eq!(msg.cmd, STATUS);
        assert_eq!(
            msg.payload,
            br#"{"devId":"vdevo123456789abcdefg","dps":{"1":false}}"#
        );
    }

    #[test]
    fn v33_decode_handles_partial_and_garbage() {
        let codec = Codec { version: Version::V33, key: *KEY };
        let frame = hex("000055aa000000020000000a0000002c00000000c27ee03f8be481e63320de7dc6eb4296eaa232ae24f922bc7c13d00b5180beaaa9c1a59c0000aa55");
        let mut buf = b"junk".to_vec();
        buf.extend_from_slice(&frame[..20]);
        assert!(codec.decode_next(&mut buf).unwrap().is_none());
        buf.extend_from_slice(&frame[20..]);
        let msg = codec.decode_next(&mut buf).unwrap().unwrap();
        assert_eq!(msg.payload, br#"{"dps":{"1":true,"9":0}}"#);
    }

    #[test]
    fn v34_session_negotiation_matches_tinytuya() {
        let codec = Codec { version: Version::V34, key: *KEY };
        assert_eq!(
            codec.encode(1, SESS_KEY_NEG_START, LOCAL_NONCE),
            hex("000055aa000000010000000300000044a8827ff1fea095a494f296743c806fe6377222e061a924c591cd9c27ea163ed40b8dc63597699da16eb6580d1f86956d6ae060351fb605143b133fa24645b0190000aa55"),
        );
        let mut buf = hex("000055aa000000010000000400000068000000008cc08950d793b29269c4802207491b3c53397740112515228dcd57760b0820577cbd19c25e7406bd11ac8f55dfed5cdf377222e061a924c591cd9c27ea163ed45945824e444adf1b7fa33055c2291390f315b49c263895bccd125c28896f4c9f0000aa55");
        let msg = codec.decode_next(&mut buf).unwrap().unwrap();
        assert_eq!(msg.cmd, SESS_KEY_NEG_RESP);
        let remote = verify_neg_resp(KEY, LOCAL_NONCE, &msg.payload).unwrap();
        assert_eq!(&remote, REMOTE_NONCE);
        assert_eq!(
            codec.encode(2, SESS_KEY_NEG_FINISH, &hmac_sha256(KEY, &remote)),
            hex("000055aa000000020000000500000054b7b4d9d8ea3000da77eabb91e8517ca9a5e8f2b076dd0b441534ea0c12b7b1dc377222e061a924c591cd9c27ea163ed4d015170f96d462a64c19a13736f4b1bd6743b39df082756d0bcd4a80847d40b40000aa55"),
        );
        assert_eq!(
            session_key(Version::V34, KEY, LOCAL_NONCE, REMOTE_NONCE),
            hex("ddd39fc310f4d13dffb872959f4cf8cb")[..],
        );
    }

    #[test]
    fn v34_session_encode_decode() {
        let key: [u8; 16] = session_key(Version::V34, KEY, LOCAL_NONCE, REMOTE_NONCE);
        let codec = Codec { version: Version::V34, key };
        assert_eq!(
            codec.encode(3, CONTROL_NEW, br#"{"protocol":5,"t":1700000000,"data":{"dps":{"1":true}}}"#),
            hex("000055aa000000030000000d00000074fbc5f6e8f8eb05d1f13f6fc498379f6bee8abf2bb4f9b9810cb54abd842d10c6d70d09cc3e148dc37e31e84f84a59b27d10117785714712ebcf43359b77ae842cd7fdce04c2a06eee80fef495504f128da9c35175f8d871cb6fe29ea8465d845e5c375104fb2d1feb7c831cf49dbcde80000aa55"),
        );
        assert_eq!(
            codec.encode(4, DP_QUERY_NEW, b"{}"),
            hex("000055aa0000000400000010000000348f931d539f2dbe870f4b7e9c0e0cc9a5532b2f6f7555ef17a8177538064e9768e56eb1a1f06a0c5d17092376a94c66290000aa55"),
        );
        let mut buf = hex("000055aa000000040000001000000048000000005507ab037207ae747cbc42eebda754cf96404b5b659eef723370939656891f00e2537dedba7b05547eb109cdd73522c449f73f1c42c8f53e6cf7d6efe70d4e560000aa55");
        let msg = codec.decode_next(&mut buf).unwrap().unwrap();
        assert_eq!(msg.cmd, DP_QUERY_NEW);
        assert_eq!(msg.payload, br#"{"dps":{"1":true}}"#);

        // A frame HMAC'd with a different key must be rejected.
        let mut buf = codec.encode(5, DP_QUERY_NEW, b"{}");
        let other = Codec { version: Version::V34, key: *KEY };
        assert!(other.decode_next(&mut buf).is_err());
    }

    #[test]
    fn v35_session_and_frames_match_tinytuya() {
        assert_eq!(
            session_key(Version::V35, KEY, LOCAL_NONCE, REMOTE_NONCE),
            hex("6b19607dc9dd5dc91f52c8cd1a5c87e8")[..],
        );
        let key: [u8; 16] = session_key(Version::V35, KEY, LOCAL_NONCE, REMOTE_NONCE);
        let codec = Codec { version: Version::V35, key };

        // Decode a device response (fixed IV vector from tinytuya).
        let mut buf = hex("000066990000000000090000001000000032303132333435363738393031de1a39c78f7b265cd5413a029533b299331955e3947b3ab3c6321272d01fcaa5875f2dce7d7300009966");
        let msg = codec.decode_next(&mut buf).unwrap().unwrap();
        assert_eq!(msg.cmd, DP_QUERY_NEW);
        assert_eq!(msg.retcode, 0);
        assert_eq!(msg.payload, br#"{"dps":{"1":true}}"#);

        // Fixed-IV encode must reproduce tinytuya byte-for-byte.
        let plain: Vec<u8> = [
            &Version::V35.header()[..],
            br#"{"protocol":5,"t":1700000000,"data":{"dps":{"1":true}}}"#,
        ]
        .concat();
        assert_eq!(
            pack_6699(5, CONTROL_NEW, &plain, &key, b"012345678901"),
            hex("000066990000000000050000000d00000062303132333435363738393031ed340cc7f459422ca6630079b70290d8651b52e99d6985815332cd96e1871a3a8264eb10e9370e41487ec54cd797a641e5a2197284af406dbf91bb366acd170fb332ea19cf678a823bf217682ecf3697c2b77f495fce00009966"),
        );

        // Negotiation frames travel over 6699 with the *local* key.
        let nego = Codec { version: Version::V35, key: *KEY };
        assert_eq!(
            pack_6699(1, SESS_KEY_NEG_START, LOCAL_NONCE, KEY, b"012345678901"),
            hex("00006699000000000001000000030000002c3031323334353637383930311f977e1392edf1e75e669c07bfdc5c56966fbbca21603d3755359d72233931a000009966"),
        );
        let mut buf = hex("0000669900000000000100000004000000503031323334353637383930317ef61f7295eaf6e05b639902bcdf5f553b4d7ac9224a07e26a502d28f675a2542587377a288418838bb146fa8f0a0d0d34dd448bb0c55df7d9913c957497f96718313e0000009966");
        let msg = nego.decode_next(&mut buf).unwrap().unwrap();
        assert_eq!(msg.cmd, SESS_KEY_NEG_RESP);
        let remote = verify_neg_resp(KEY, LOCAL_NONCE, &msg.payload).unwrap();
        assert_eq!(&remote, REMOTE_NONCE);

        // Round-trip through the random-nonce encoder.
        let frame = codec.encode(7, DP_QUERY_NEW, b"{}");
        let mut buf = frame.clone();
        let msg = codec.decode_next(&mut buf).unwrap().unwrap();
        // Client->device frames carry no retcode, so "{}" survives as-is.
        assert_eq!(msg.payload, b"{}");
    }

    #[test]
    fn udp_broadcast_parses() {
        let frame = hex("000055aa0000000000000013000000ac0000000097e190a5ed90c0924c3e1db529391681dd93d406e784045a64287d343c2101238a28d0879225ec6bb06d7e0792ff2e2d18ca0c1babd743880b08c3cd80e660c65fb9d6387160d3e4636f8ec084aaf888f462949a474596b425bab23babe68074e3c949d359d5c3ea6cd5201d408904bfa453d3c37aa6b8cb137d6a3614eb70e5343378fd637a924ca2f95eab619c9c2e2dd3a795424f2879a4492a51a7d90f07f0b67eba0000aa55");
        let v = parse_udp(&frame).unwrap();
        assert_eq!(v["gwId"], "vdevo123456789abcdefg");
        assert_eq!(v["ip"], "10.7.1.20");
        assert_eq!(v["version"], "3.3");
    }
}
