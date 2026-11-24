//! The network part of the first start: the mower announces _mowbite._tcp over mDNS (avahi of the base system), the
//! installer asks mowbite-host on its broker to confirm the start (host.confirm_start). The broker has no login, so
//! nothing secret goes over it: the answer proves the mower with an HMAC of a fresh challenge under the install id
//! that only the pack and the sheet hold.

use crate::secret::Secret;
use crate::text::FirstStart;
use anyhow::{Context, Result, anyhow, bail};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

pub const SERVICE: &str = "_mowbite._tcp.local.";
const BROWSE: Duration = Duration::from_secs(5);
const ANSWER: Duration = Duration::from_secs(10);
/// The broker on the setup Wi-Fi of the mower (shared mode of NetworkManager), tried when mDNS finds nothing.
const SETUP_AP: (IpAddr, u16) = (IpAddr::V4(Ipv4Addr::new(10, 42, 0, 1)), 1883);

/// Mowers announced on the local network: (host name, address, port), one row per IPv4 address.
pub async fn find(wait: Duration) -> Result<Vec<(String, IpAddr, u16)>> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    let events = daemon.browse(SERVICE)?;
    let mut found = Vec::new();
    let until = tokio::time::Instant::now() + wait;
    while let Ok(Ok(event)) = tokio::time::timeout_at(until, events.recv_async()).await {
        if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
            for addr in info.get_addresses_v4() {
                found.push((
                    info.get_hostname().trim_end_matches('.').to_owned(),
                    IpAddr::V4(*addr),
                    info.get_port(),
                ));
            }
        }
    }
    let _ = daemon.shutdown();
    Ok(found)
}

/// HMAC-SHA256 (RFC 2104) in hex.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    let outer = Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize();
    outer.iter().map(|b| format!("{b:02x}")).collect()
}

/// Ok(None) for {"trial": false} with the expected proof; Ok(Some(why)) for a refusal the owner can wait out or an
/// answer from another device; Err for a malformed answer.
pub fn judge(answer: &Value, proof: &str) -> Result<Option<String>> {
    if let Some(result) = answer.get("result") {
        return match result.get("trial") {
            Some(Value::Bool(false)) if result.get("proof").and_then(Value::as_str) == Some(proof) => Ok(None),
            Some(Value::Bool(false)) => Ok(Some(
                "an answer without the proof of this mower (another device, or a system not written from here)"
                    .into(),
            )),
            _ => bail!("unexpected answer of host.confirm_start: {result}"),
        };
    }
    let Some(error) = answer.get("error") else {
        bail!("an answer without result or error");
    };
    let data = error.get("data").unwrap_or(&Value::Null);
    let reason = data
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    let message = data
        .get("message")
        .or_else(|| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("");
    Ok(Some(format!("{reason}: {message}")))
}

/// One JSON-RPC call on the broker at addr; the host answers on rpc/response with the id of the request.
pub async fn call(addr: IpAddr, port: u16, method: &str, params: Value) -> Result<Value> {
    let id = format!("installer-{:016x}", rand_id());
    let mut options = MqttOptions::new(id.clone(), addr.to_string(), port);
    options.set_keep_alive(Duration::from_secs(15));
    let (client, mut events) = AsyncClient::new(options, 10);
    let work = async {
        let mut sent = false;
        loop {
            match events.poll().await.context("broker of the mower")? {
                Event::Incoming(Packet::ConnAck(_)) => {
                    client.subscribe("rpc/response", QoS::AtLeastOnce).await?
                }
                Event::Incoming(Packet::SubAck(_)) if !sent => {
                    let request =
                        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
                    client
                        .publish("rpc/request", QoS::AtLeastOnce, false, request.to_string())
                        .await?;
                    sent = true;
                }
                Event::Incoming(Packet::Publish(p)) if p.topic == "rpc/response" => {
                    if let Ok(answer) = serde_json::from_slice::<Value>(&p.payload) {
                        if answer.get("id").and_then(Value::as_str) == Some(id.as_str()) {
                            return Ok(answer);
                        }
                    }
                }
                _ => {}
            }
        }
    };
    let answer = tokio::time::timeout(ANSWER, work)
        .await
        .unwrap_or_else(|_| bail!("no answer within 10 s"));
    let _ = client.disconnect().await;
    answer
}

fn rand_id() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    (t as u64) ^ u64::from(std::process::id()).rotate_left(32)
}

fn challenge() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow!("no randomness from the system: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Finds the mower by its name (from the pack) and confirms its start; remembers the addresses that answered and
/// looks again once none does.
pub struct Confirm {
    pub name: String,
    install_id: Secret<String>,
    at: RefCell<Vec<(IpAddr, u16)>>,
}

impl Confirm {
    pub fn new(name: &str, install_id: Secret<String>) -> Self {
        Confirm {
            name: name.to_owned(),
            install_id,
            at: RefCell::new(Vec::new()),
        }
    }

    async fn ask(&self, addr: IpAddr, port: u16) -> Result<Option<String>> {
        let challenge = challenge()?;
        let proof = hmac_sha256(self.install_id.expose().as_bytes(), challenge.as_bytes());
        let answer = call(
            addr,
            port,
            "host.confirm_start",
            json!({"challenge": challenge}),
        )
        .await?;
        judge(&answer, &proof)
    }
}

impl FirstStart for Confirm {
    async fn confirm(&self) -> Result<Option<String>> {
        if self.at.borrow().is_empty() {
            let mut mine: Vec<(IpAddr, u16)> = find(BROWSE)
                .await?
                .into_iter()
                .filter(|(host, _, _)| {
                    host.strip_suffix(".local")
                        .unwrap_or(host)
                        .eq_ignore_ascii_case(&self.name)
                })
                .map(|(_, addr, port)| (addr, port))
                .collect();
            mine.push(SETUP_AP);
            *self.at.borrow_mut() = mine;
        }
        let candidates = self.at.borrow().clone();
        let mut last = format!("{} not seen on the network yet", self.name);
        for (addr, port) in candidates {
            match self.ask(addr, port).await {
                Ok(answer) => {
                    *self.at.borrow_mut() = vec![(addr, port)];
                    return Ok(answer);
                }
                // Reached only from the setup Wi-Fi; elsewhere its failure says nothing.
                Err(_) if (addr, port) == SETUP_AP => {}
                Err(e) => last = format!("{addr}: {e:#}"),
            }
        }
        self.at.borrow_mut().clear();
        Ok(Some(last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_of_host_confirm_start() {
        assert_eq!(
            judge(
                &json!({"id": "1", "result": {"trial": false, "proof": "ab"}}),
                "ab"
            )
            .unwrap(),
            None
        );
        // Another device on the network knows the name, not the install id.
        for result in [
            json!({"trial": false}),
            json!({"trial": false, "proof": "cd"}),
        ] {
            assert!(judge(&json!({"result": result}), "ab").unwrap().is_some());
        }
        let refused = json!({"error": {"code": -32600, "message": "x",
            "data": {"event": "UPDATE_REFUSED", "reason": "START_NOT_OK", "message": "no WLAN connection"}}});
        assert_eq!(
            judge(&refused, "ab").unwrap().unwrap(),
            "START_NOT_OK: no WLAN connection"
        );
        assert!(judge(&json!({"result": {"trial": true}}), "ab").is_err());
        assert!(judge(&json!({"id": "1"}), "ab").is_err());
    }

    #[test]
    fn hmac_of_rfc_4231() {
        assert_eq!(
            hmac_sha256(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hmac_sha256(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
