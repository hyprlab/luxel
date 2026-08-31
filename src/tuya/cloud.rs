//! Tuya Cloud OpenAPI client: fetches the account's device list, including
//! each device's local key, so the setup wizard can configure devices
//! without tinytuya or manual file juggling.
//!
//! Only two read-only endpoints are used, with Tuya's HMAC-SHA256 request
//! signing (the same calls tinytuya's wizard makes):
//! `/v1.0/token?grant_type=1` and the device list
//! (`/v1.0/iot-01/associated-users/devices`, plus a per-user pass through
//! `/v1.3/iot-03/devices` to fill in local keys some account types omit).

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::config::TuyaDevice;

#[derive(Debug, Clone)]
pub struct CloudAccount {
    pub client_id: String,
    pub secret: String,
    pub region: String,
}

/// Data centers, in the order shown in the wizard's dropdown.
pub const REGIONS: &[(&str, &str)] = &[
    ("us", "Western America"),
    ("us-e", "Eastern America"),
    ("eu", "Central Europe"),
    ("eu-w", "Western Europe"),
    ("cn", "China"),
    ("in", "India"),
    ("sg", "Singapore"),
];

fn region_base(region: &str) -> String {
    let host = match region {
        "us" => "openapi.tuyaus.com",
        "us-e" => "openapi-ueaz.tuyaus.com",
        "eu" => "openapi.tuyaeu.com",
        "eu-w" => "openapi-weaz.tuyaeu.com",
        "cn" => "openapi.tuyacn.com",
        "in" => "openapi.tuyain.com",
        "sg" => "openapi-sg.iotbing.com",
        _ => "openapi.tuyaus.com",
    };
    format!("https://{host}")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Tuya's request signature (post-2021 algorithm) for a GET with no body
/// and no signed headers.
fn build_sign(client_id: &str, secret: &str, token: Option<&str>, t: &str, path_query: &str) -> String {
    let empty_body_sha = hex(&Sha256::digest(b""));
    let string_to_sign = format!("GET\n{empty_body_sha}\n\n{path_query}");
    let payload = match token {
        None => format!("{client_id}{t}{string_to_sign}"),
        Some(tok) => format!("{client_id}{tok}{t}{string_to_sign}"),
    };
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    hex(&mac.finalize().into_bytes()).to_uppercase()
}

/// Translate Tuya's error codes into something actionable.
fn friendly_error(code: i64, msg: &str) -> String {
    match code {
        1001 | 1005 => "Tuya Cloud rejected the Access ID — check it against the \
                        project's Authorization Key section."
            .to_string(),
        1004 => "Tuya Cloud rejected the request signature — the Access Secret is \
                 probably wrong."
            .to_string(),
        1010 | 1011 => "The Tuya Cloud session expired — try again.".to_string(),
        1106 | 1114 => "Tuya Cloud denied access. Usually this means the data center \
                        doesn't match your SmartLife account region, or the SmartLife \
                        account isn't linked to the project (Devices → Link Tuya App \
                        Account)."
            .to_string(),
        28841101 | 28841002 => "The project's cloud services aren't authorized or the \
                                trial expired — open the project on iot.tuya.com and \
                                (re)authorize IoT Core under Service API."
            .to_string(),
        _ if msg.contains("subscribed") || msg.contains("expire") => {
            format!(
                "Tuya Cloud error {code}: {msg} — open the project on iot.tuya.com \
                 and (re)authorize IoT Core under Service API."
            )
        }
        _ => format!("Tuya Cloud error {code}: {msg}"),
    }
}

/// Signed GET returning the response's `result` value.
fn signed_get(
    agent: &ureq::Agent,
    base: &str,
    acct: &CloudAccount,
    token: Option<&str>,
    path_query: &str,
) -> Result<serde_json::Value, String> {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_default();
    let sign = build_sign(&acct.client_id, &acct.secret, token, &t, path_query);
    let mut req = agent
        .get(format!("{base}{path_query}"))
        .header("client_id", &acct.client_id)
        .header("sign", &sign)
        .header("t", &t)
        .header("sign_method", "HMAC-SHA256")
        .header("mode", "cors");
    req = match token {
        // tinytuya also sends the secret on the token request; match it.
        None => req.header("secret", &acct.secret),
        Some(tok) => req.header("access_token", tok),
    };
    let mut resp = req
        .call()
        .map_err(|e| format!("Could not reach Tuya Cloud: {e}"))?;
    let body: serde_json::Value = resp
        .body_mut()
        .read_json()
        .map_err(|e| format!("Invalid response from Tuya Cloud: {e}"))?;
    if !body.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
        let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("unknown error");
        return Err(friendly_error(code, msg));
    }
    body.get("result")
        .cloned()
        .ok_or_else(|| "Tuya Cloud response had no result".to_string())
}

/// Fetch a paged device list; `params` must not include `last_row_key`.
fn paged(
    agent: &ureq::Agent,
    base: &str,
    acct: &CloudAccount,
    token: &str,
    path: &str,
    params: &[(&str, &str)],
) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    let mut last_row_key: Option<String> = None;
    for _ in 0..20 {
        let mut query: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Some(key) = &last_row_key {
            query.push(("last_row_key".to_string(), key.clone()));
        }
        // Tuya signs the query string with keys in alphabetical order.
        query.sort();
        let qs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let result = signed_get(agent, base, acct, Some(token), &format!("{path}?{}", qs.join("&")))?;
        // The by-user list nests devices under "list", the account-wide one
        // under "devices".
        if let Some(arr) = result
            .get("devices")
            .or_else(|| result.get("list"))
            .and_then(|v| v.as_array())
        {
            out.extend(arr.iter().cloned());
        }
        let has_more = result.get("has_more").and_then(|v| v.as_bool()).unwrap_or(false);
        last_row_key = result
            .get("last_row_key")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if !has_more || last_row_key.is_none() {
            break;
        }
    }
    Ok(out)
}

/// Merge `extra` into `list` by device id, filling empty/missing fields.
fn merge_device_lists(list: &mut Vec<serde_json::Value>, extra: Vec<serde_json::Value>) {
    for new in extra {
        let Some(id) = new.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
            continue;
        };
        match list
            .iter_mut()
            .find(|d| d.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        {
            Some(existing) => {
                if let (Some(obj), Some(new_obj)) = (existing.as_object_mut(), new.as_object()) {
                    for (k, v) in new_obj {
                        let missing = obj
                            .get(k)
                            .map(|old| old.is_null() || old.as_str() == Some(""))
                            .unwrap_or(true);
                        if missing {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            None => list.push(new),
        }
    }
}

pub fn fetch_devices(acct: &CloudAccount) -> Result<Vec<TuyaDevice>, String> {
    fetch_devices_at(&region_base(&acct.region), acct)
}

fn fetch_devices_at(base: &str, acct: &CloudAccount) -> Result<Vec<TuyaDevice>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into();

    let token = signed_get(&agent, base, acct, None, "/v1.0/token?grant_type=1")?
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or("Tuya Cloud returned no access token")?;

    let mut raw = paged(
        &agent,
        base,
        acct,
        &token,
        "/v1.0/iot-01/associated-users/devices",
        &[("size", "50")],
    )?;

    // Some account types omit local keys from the account-wide list; the
    // per-user list has them. Ignore failures here — the API isn't always
    // enabled — and fall back to whatever the first list contained.
    let uids: BTreeSet<String> = raw
        .iter()
        .filter_map(|d| d.get("uid").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    for uid in uids {
        if let Ok(extra) = paged(
            &agent,
            base,
            acct,
            &token,
            "/v1.3/iot-03/devices",
            &[
                ("page_size", "75"),
                ("source_id", &uid),
                ("source_type", "tuyaUser"),
            ],
        ) {
            merge_device_lists(&mut raw, extra);
        }
    }

    let text = |v: &serde_json::Value, field: &str| {
        v.get(field)
            .and_then(|f| f.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let mut out = Vec::new();
    for dev in &raw {
        let id = text(dev, "id");
        let key = text(dev, "local_key");
        if id.is_empty() || key.is_empty() {
            continue;
        }
        // Gateway children (zigbee/BLE) have no LAN presence of their own.
        if dev.get("sub").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        out.push(TuyaDevice {
            name: text(dev, "name"),
            // The cloud's "ip" is the WAN address, useless for LAN control;
            // the network locate step finds the real one.
            host: String::new(),
            id,
            key,
            version: "auto".to_string(),
        });
    }
    if out.is_empty() {
        return Err("The Tuya Cloud account has no devices with local keys. Is the \
                    SmartLife account linked to the project (Devices → Link Tuya \
                    App Account)?"
            .to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    // Golden signatures computed with tinytuya's payload construction.
    #[test]
    fn signing_matches_tinytuya() {
        assert_eq!(
            build_sign(
                "test_client_id",
                "test_secret_0123",
                None,
                "1700000000000",
                "/v1.0/token?grant_type=1"
            ),
            "8FF5EC6CD339B7A0BFF520B8F6F02E4721A0E9D7FBCFAF4378E6231090F024AF"
        );
        assert_eq!(
            build_sign(
                "test_client_id",
                "test_secret_0123",
                Some("tok_1234567890"),
                "1700000000000",
                "/v1.0/iot-01/associated-users/devices?size=50"
            ),
            "5925F945C9A4C2F212D280AF3995AE5D74E79CD0BF4FC7EFAC6B212B5184913D"
        );
    }

    /// Minimal canned-response HTTP server standing in for the Tuya Cloud.
    fn mock_cloud() -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut paths = Vec::new();
            // token, account-wide list, one per-user list
            for _ in 0..3 {
                let (mut sock, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let mut req = Vec::new();
                loop {
                    let n = sock.read(&mut buf).unwrap();
                    req.extend_from_slice(&buf[..n]);
                    if n == 0 || req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&req).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                assert!(request.contains("client_id: test_client_id"));
                assert!(request.contains("sign_method: HMAC-SHA256"));
                let body = if path.starts_with("/v1.0/token") {
                    r#"{"success":true,"result":{"access_token":"tok_1234567890","uid":"az1"}}"#
                        .to_string()
                } else if path.starts_with("/v1.0/iot-01") {
                    assert!(request.contains("access_token: tok_1234567890"));
                    // Account-wide list: key missing for one device.
                    r#"{"success":true,"result":{"devices":[
                        {"id":"aaaabbbbccccddddeeee","name":"Plug A","local_key":"0123456789abcdef","uid":"user1","sub":false,"ip":"203.0.113.7"},
                        {"id":"bbbbccccddddeeeeffff","name":"Plug B","local_key":"","uid":"user1"},
                        {"id":"ccccddddeeeeffff0000","name":"Zigbee child","local_key":"ffffffffffffffff","uid":"user1","sub":true}
                    ],"has_more":false}}"#
                        .to_string()
                } else {
                    assert!(path.starts_with("/v1.3/iot-03/devices"));
                    assert!(path.contains("source_id=user1"));
                    // Per-user list fills in Plug B's key.
                    r#"{"success":true,"result":{"list":[
                        {"id":"bbbbccccddddeeeeffff","name":"Plug B","local_key":"fedcba9876543210"}
                    ],"has_more":false}}"#
                        .to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).unwrap();
                paths.push(path);
            }
            paths
        });
        (base, handle)
    }

    #[test]
    fn fetch_devices_via_mock_cloud() {
        let (base, server) = mock_cloud();
        let acct = CloudAccount {
            client_id: "test_client_id".into(),
            secret: "test_secret_0123".into(),
            region: "us".into(),
        };
        let devices = fetch_devices_at(&base, &acct).unwrap();
        let paths = server.join().unwrap();
        assert_eq!(paths.len(), 3);
        // Plug A kept, Plug B completed from the per-user list, zigbee child
        // dropped, WAN ip never used as host.
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "Plug A");
        assert_eq!(devices[0].key, "0123456789abcdef");
        assert_eq!(devices[0].host, "");
        assert_eq!(devices[1].name, "Plug B");
        assert_eq!(devices[1].key, "fedcba9876543210");
        assert_eq!(devices[1].version, "auto");
    }
}
