//! The network part of the first start: the mower announces _mowbite._tcp over mDNS (avahi of the base system), the
//! installer asks mowbite-host on its broker to confirm the start (host.confirm_start). The broker has no login, so
//! nothing secret goes over it.

use crate::text::FirstStart;
use anyhow::{Context, Result, bail};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::net::IpAddr;
use std::time::Duration;

pub const SERVICE: &str = "_mowbite._tcp.local.";
const BROWSE: Duration = Duration::from_secs(5);
const ANSWER: Duration = Duration::from_secs(10);

/// Mowers announced on the local network: (host name, address, port).
pub async fn find(wait: Duration) -> Result<Vec<(String, IpAddr, u16)>> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    let events = daemon.browse(SERVICE)?;
    let mut found = Vec::new();
    let until = tokio::time::Instant::now() + wait;
    while let Ok(Ok(event)) = tokio::time::timeout_at(until, events.recv_async()).await {
        if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
            if let Some(addr) = info.get_addresses_v4().into_iter().next() {
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

/// Ok(None) for {"trial": false}; Ok(Some(why)) for a refusal the owner can wait out; Err for a malformed answer.
pub fn judge(answer: &Value) -> Result<Option<String>> {
    if let Some(result) = answer.get("result") {
        return match result.get("trial") {
            Some(Value::Bool(false)) => Ok(None),
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
pub async fn call(addr: IpAddr, port: u16, method: &str) -> Result<Value> {
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
                    let request = json!({"jsonrpc": "2.0", "id": id, "method": method});
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

/// Finds the mower by its name (from the pack) and confirms its start; remembers the address once found.
pub struct Confirm {
    pub name: String,
    at: RefCell<Option<(IpAddr, u16)>>,
}

impl Confirm {
    pub fn new(name: &str) -> Self {
        Confirm {
            name: name.to_owned(),
            at: RefCell::new(None),
        }
    }
}

impl FirstStart for Confirm {
    async fn confirm(&self) -> Result<Option<String>> {
        let at = *self.at.borrow();
        let (addr, port) = match at {
            Some(at) => at,
            None => {
                let mine = find(BROWSE).await?.into_iter().find(|(host, _, _)| {
                    host.strip_suffix(".local")
                        .unwrap_or(host)
                        .eq_ignore_ascii_case(&self.name)
                });
                let Some((_, addr, port)) = mine else {
                    return Ok(Some(format!("{} not seen on the network yet", self.name)));
                };
                *self.at.borrow_mut() = Some((addr, port));
                (addr, port)
            }
        };
        judge(&call(addr, port, "host.confirm_start").await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_of_host_confirm_start() {
        assert_eq!(
            judge(&json!({"id": "1", "result": {"trial": false}})).unwrap(),
            None
        );
        let refused = json!({"error": {"code": -32600, "message": "x",
            "data": {"event": "UPDATE_REFUSED", "reason": "START_NOT_OK", "message": "no WLAN connection"}}});
        assert_eq!(
            judge(&refused).unwrap().unwrap(),
            "START_NOT_OK: no WLAN connection"
        );
        assert!(judge(&json!({"result": {"trial": true}})).is_err());
        assert!(judge(&json!({"id": "1"})).is_err());
    }
}
