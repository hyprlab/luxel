#!/usr/bin/env python3
"""Generate golden Tuya-protocol test vectors from tinytuya (the reference
implementation) with fixed key/nonces/time, for validating the Rust port."""
import json
import hmac as hmac_mod
from hashlib import sha256, md5

from tinytuya.core.message_helper import TuyaMessage, pack_message, unpack_message
from tinytuya.core import command_types as CT
from tinytuya.core import header as H
from tinytuya.core.crypto_helper import AESCipher

KEY = b"0123456789abcdef"
DEVID = "vdevo123456789abcdefg"
T = 1700000000
LOCAL_NONCE = b"aaaabbbbccccdddd"
REMOTE_NONCE = b"eeeeffffgggghhhh"

out = {}

cipher = AESCipher(KEY)

# ---- v3.3 ----
# CONTROL: json (no spaces), ECB-encrypt, then plaintext "3.3"+12x00 header.
control_json = json.dumps(
    {"devId": DEVID, "uid": DEVID, "t": str(T), "dps": {"1": True}},
    separators=(",", ":"),
).encode()
payload = H.PROTOCOL_33_HEADER + cipher.encrypt(control_json, use_base64=False)
msg = TuyaMessage(1, CT.CONTROL, 0, payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["v33_control_json"] = control_json.decode()
out["v33_control_frame"] = pack_message(msg).hex()

# DP_QUERY: no version header.
query_json = json.dumps(
    {"gwId": DEVID, "devId": DEVID, "uid": DEVID, "t": str(T)},
    separators=(",", ":"),
).encode()
payload = cipher.encrypt(query_json, use_base64=False)
msg = TuyaMessage(2, CT.DP_QUERY, 0, payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["v33_query_json"] = query_json.decode()
out["v33_query_frame"] = pack_message(msg).hex()

# Simulated device response to DP_QUERY: retcode 0 + ECB(json), CRC32.
resp_json = b'{"dps":{"1":true,"9":0}}'
resp_payload = b"\x00\x00\x00\x00" + cipher.encrypt(resp_json, use_base64=False)
msg = TuyaMessage(2, CT.DP_QUERY, 0, resp_payload, 0, True, H.PREFIX_55AA_VALUE, False)
frame = pack_message(msg)
out["v33_response_frame"] = frame.hex()
out["v33_response_json"] = resp_json.decode()
u = unpack_message(frame)
assert u.crc_good and cipher.decrypt(u.payload, use_base64=False, decode_text=False) == resp_json

# Simulated device STATUS push (has plaintext version header).
push_json = b'{"devId":"%s","dps":{"1":false}}' % DEVID.encode()
push_payload = b"\x00\x00\x00\x00" + H.PROTOCOL_33_HEADER + cipher.encrypt(push_json, use_base64=False)
msg = TuyaMessage(3, CT.STATUS, 0, push_payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["v33_push_frame"] = pack_message(msg).hex()
out["v33_push_json"] = push_json.decode()

# ---- v3.4 ----
# Session key derivation.
xored = bytes(a ^ b for a, b in zip(LOCAL_NONCE, REMOTE_NONCE))
session_key_34 = cipher.encrypt(xored, use_base64=False, pad=False)
out["v34_session_key"] = session_key_34.hex()

# NEG_START frame: payload = local_nonce, ECB(local key), HMAC(local key).
payload = cipher.encrypt(LOCAL_NONCE, use_base64=False)
msg = TuyaMessage(1, CT.SESS_KEY_NEG_START, 0, payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["v34_neg_start_frame"] = pack_message(msg, hmac_key=KEY).hex()

# Simulated device NEG_RESP: retcode + ECB(remote_nonce + hmac(local_nonce)).
resp = REMOTE_NONCE + hmac_mod.new(KEY, LOCAL_NONCE, sha256).digest()
payload = b"\x00\x00\x00\x00" + cipher.encrypt(resp, use_base64=False)
msg = TuyaMessage(1, CT.SESS_KEY_NEG_RESP, 0, payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["v34_neg_resp_frame"] = pack_message(msg, hmac_key=KEY).hex()

# NEG_FINISH frame: payload = hmac(remote_nonce), ECB, HMAC.
payload = cipher.encrypt(hmac_mod.new(KEY, REMOTE_NONCE, sha256).digest(), use_base64=False)
msg = TuyaMessage(2, CT.SESS_KEY_NEG_FINISH, 0, payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["v34_neg_finish_frame"] = pack_message(msg, hmac_key=KEY).hex()

# CONTROL_NEW with session key: "3.4" header + json, all ECB'd together.
scipher = AESCipher(session_key_34)
control34_json = json.dumps(
    {"protocol": 5, "t": T, "data": {"dps": {"1": True}}}, separators=(",", ":")
).encode()
payload = scipher.encrypt(H.PROTOCOL_34_HEADER + control34_json, use_base64=False)
msg = TuyaMessage(3, CT.CONTROL_NEW, 0, payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["v34_control_json"] = control34_json.decode()
out["v34_control_frame"] = pack_message(msg, hmac_key=session_key_34).hex()

# DP_QUERY_NEW: "{}", no version header.
payload = scipher.encrypt(b"{}", use_base64=False)
msg = TuyaMessage(4, CT.DP_QUERY_NEW, 0, payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["v34_query_frame"] = pack_message(msg, hmac_key=session_key_34).hex()

# Simulated device response to DP_QUERY_NEW: retcode + ECB(json), HMAC.
resp34 = b'{"dps":{"1":true}}'
payload = b"\x00\x00\x00\x00" + scipher.encrypt(resp34, use_base64=False)
msg = TuyaMessage(4, CT.DP_QUERY_NEW, 0, payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["v34_response_frame"] = pack_message(msg, hmac_key=session_key_34).hex()
out["v34_response_json"] = resp34.decode()

# ---- v3.5 ----
# Session key: GCM-encrypt xored nonces with iv=local_nonce[:12], ct[12:28].
session_key_35 = cipher.encrypt(
    xored, use_base64=False, pad=False, iv=LOCAL_NONCE[:12]
)[12:28]
out["v35_session_key"] = session_key_35.hex()

IV = b"012345678901"
# CONTROL_NEW frame ("3.5" header inside plaintext), fixed IV.
control35_json = control34_json
msg = TuyaMessage(
    5, CT.CONTROL_NEW, None, H.PROTOCOL_35_HEADER + control35_json, 0, True,
    H.PREFIX_6699_VALUE, IV,
)
out["v35_control_frame"] = pack_message(msg, hmac_key=session_key_35).hex()
out["v35_control_json"] = control35_json.decode()

# Simulated device response (retcode inside GCM plaintext).
msg = TuyaMessage(9, CT.DP_QUERY_NEW, 0, resp34, 0, True, H.PREFIX_6699_VALUE, IV)
frame = pack_message(msg, hmac_key=session_key_35)
out["v35_response_frame"] = frame.hex()
out["v35_response_json"] = resp34.decode()
u = unpack_message(frame, hmac_key=session_key_35)
assert u.crc_good and u.payload == resp34 and u.retcode == 0, u

# NEG_START over 6699 (uses local key + fixed IV).
msg = TuyaMessage(1, CT.SESS_KEY_NEG_START, None, LOCAL_NONCE, 0, True, H.PREFIX_6699_VALUE, IV)
out["v35_neg_start_frame"] = pack_message(msg, hmac_key=KEY).hex()
# Device NEG_RESP over 6699.
msg = TuyaMessage(1, CT.SESS_KEY_NEG_RESP, 0, resp, 0, True, H.PREFIX_6699_VALUE, IV)
out["v35_neg_resp_frame"] = pack_message(msg, hmac_key=KEY).hex()

# ---- UDP discovery ----
udpkey = md5(b"yGAdlopoPVldABfn").digest()
bcast_json = json.dumps(
    {"ip": "10.7.1.20", "gwId": DEVID, "active": 2, "ability": 0,
     "mode": 0, "encrypt": True, "productKey": "keydeadbeef00000", "version": "3.3"},
    separators=(",", ":"),
).encode()
ucipher = AESCipher(udpkey)
payload = b"\x00\x00\x00\x00" + ucipher.encrypt(bcast_json, use_base64=False)
msg = TuyaMessage(0, 0x13, 0, payload, 0, True, H.PREFIX_55AA_VALUE, False)
out["udp_6667_frame"] = pack_message(msg).hex()
out["udp_json"] = bcast_json.decode()

out["udpkey"] = udpkey.hex()
out["key"] = KEY.decode()
out["devid"] = DEVID
out["t"] = T
out["local_nonce"] = LOCAL_NONCE.decode()
out["remote_nonce"] = REMOTE_NONCE.decode()

print(json.dumps(out, indent=1))
