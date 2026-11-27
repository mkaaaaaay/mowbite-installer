//! The network part of the first start: the mower announces _mowbite._tcp over mDNS (avahi of the base system), the
//! installer asks mowbite-host on its broker to confirm the start (host.confirm_start). The broker has no login, so
//! nothing secret goes over it: the answer proves the mower with an HMAC of a fresh challenge under the install id
//! that only the pack and the sheet hold.

use crate::secret::Secret;
use crate::text::FirstStart;
use anyhow::{Context, Result, bail};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde_json::{Value, json};
use sha2::Sha256;
use std::cell::{Cell, RefCell};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const SERVICE: &str = "_mowbite._tcp.local.";
const BROWSE: Duration = Duration::from_secs(5);
const ANSWER: Duration = Duration::from_secs(10);
/// An address that takes no connection within this is not asked: a search round stays short.
const REACH: Duration = Duration::from_secs(2);
const RESOLVE: Duration = Duration::from_secs(3);
const BROKER: u16 = 1883;
/// The mower on its setup Wi-Fi (shared mode of NetworkManager), tried beside what the search finds.
pub const SETUP_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 42, 0, 1));
const SETUP_AP: (IpAddr, u16) = (SETUP_ADDR, BROKER);

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
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(message);
    crate::store::hex(&mac.finalize().into_bytes())
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
                    if let Ok(answer) = serde_json::from_slice::<Value>(&p.payload)
                        && answer.get("id").and_then(Value::as_str) == Some(id.as_str())
                    {
                        return Ok(answer);
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

/// The Wi-Fi this PC is on, as the system tells it (NetworkManager, on Windows netsh); None on a cable or when it
/// does not tell. The page starts from it, the owner may type another one.
pub fn current_wifi() -> Option<String> {
    let asked = |program: &str, args: &[&str]| {
        let out = std::process::Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        String::from_utf8(out.stdout).ok()
    };
    if cfg!(windows) {
        wifi_of_netsh(&asked("netsh", &["wlan", "show", "interfaces"])?)
    } else {
        wifi_of_nmcli(&asked(
            "nmcli",
            &["-t", "-f", "ACTIVE,SSID", "device", "wifi"],
        )?)
    }
}

/// The active row of "ACTIVE,SSID" in the terse form of nmcli, where a colon in a name comes escaped.
fn wifi_of_nmcli(out: &str) -> Option<String> {
    out.lines()
        .find_map(|line| line.strip_prefix("yes:"))
        .map(|name| name.replace("\\:", ":").replace("\\\\", "\\"))
        .filter(|name| !name.is_empty())
}

/// The SSID row of a connected interface. netsh writes in the code page of the console, so only a plain ASCII name
/// is taken as it stands.
fn wifi_of_netsh(out: &str) -> Option<String> {
    out.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.trim() == "SSID")
        .map(|(_, name)| name.trim().to_owned())
        .filter(|name| !name.is_empty() && name.is_ascii())
}

fn rand_id() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    (t as u64) ^ u64::from(std::process::id()).rotate_left(32)
}

/// IPv4 addresses the resolver of the system has for name: its own mDNS or the name the router keeps for the mower
/// find it where the browse above is blocked (a firewall on this PC).
async fn resolve(name: String) -> Vec<IpAddr> {
    let lookup = tokio::task::spawn_blocking(move || {
        (name.as_str(), BROKER)
            .to_socket_addrs()
            .map(|found| found.map(|a| a.ip()).filter(IpAddr::is_ipv4).collect())
            .unwrap_or_default()
    });
    match tokio::time::timeout(RESOLVE, lookup).await {
        Ok(Ok(found)) => found,
        _ => Vec::new(),
    }
}

/// Whether addr takes a connection at all, so a silent address costs REACH and not ANSWER.
async fn reachable(addr: IpAddr, port: u16) -> bool {
    let probe = tokio::task::spawn_blocking(move || {
        TcpStream::connect_timeout(&SocketAddr::new(addr, port), REACH).is_ok()
    });
    matches!(
        tokio::time::timeout(REACH + Duration::from_secs(1), probe).await,
        Ok(Ok(true))
    )
}

/// Finds the mower by its name (from the pack) and confirms its start; remembers the address that answered and
/// looks again once it does not. Whatever answers has to prove the install id, so a wrong address does no harm.
pub struct Confirm {
    name: RefCell<String>,
    install_id: RefCell<Secret<String>>,
    at: RefCell<Vec<(IpAddr, u16)>>,
    given: Cell<Option<IpAddr>>,
    reached: Cell<Option<IpAddr>>,
}

impl Confirm {
    pub fn new(name: &str, install_id: Secret<String>) -> Self {
        Confirm {
            name: RefCell::new(name.to_owned()),
            install_id: RefCell::new(install_id),
            at: RefCell::new(Vec::new()),
            given: Cell::new(None),
            reached: Cell::new(None),
        }
    }

    /// Another mower to look for: a check that goes on to install learns name and id only then.
    pub fn aim(&self, name: &str, install_id: Secret<String>) {
        *self.name.borrow_mut() = name.to_owned();
        *self.install_id.borrow_mut() = install_id;
        self.at.borrow_mut().clear();
    }

    async fn ask(&self, addr: IpAddr, port: u16) -> Result<Option<String>> {
        let challenge = crate::secret::random_hex(32)?;
        let proof = hmac_sha256(
            self.install_id.borrow().expose().as_bytes(),
            challenge.as_bytes(),
        );
        let answer = call(
            addr,
            port,
            "host.confirm_start",
            json!({"challenge": challenge}),
        )
        .await?;
        judge(&answer, &proof)
    }

    /// Where the mower may be: announced under its name, known to the resolver of the system, given by the owner,
    /// and on its setup Wi-Fi.
    async fn candidates(&self) -> Result<Vec<(IpAddr, u16)>> {
        let name = self.name.borrow().clone();
        let mut out: Vec<(IpAddr, u16)> = find(BROWSE)
            .await?
            .into_iter()
            .filter(|(host, _, _)| {
                host.strip_suffix(".local")
                    .unwrap_or(host)
                    .eq_ignore_ascii_case(&name)
            })
            .map(|(_, addr, port)| (addr, port))
            .collect();
        for name in [format!("{name}.local"), name] {
            out.extend(resolve(name).await.into_iter().map(|a| (a, BROKER)));
        }
        out.extend(self.given.get().map(|a| (a, BROKER)));
        out.push(SETUP_AP);
        let mut seen = Vec::new();
        out.retain(|c| !seen.contains(c) && (seen.push(*c), true).1);
        Ok(out)
    }
}

impl FirstStart for Confirm {
    async fn confirm(&self) -> Result<Option<String>> {
        if self.at.borrow().is_empty() {
            let found = self.candidates().await?;
            *self.at.borrow_mut() = found;
        }
        let candidates = self.at.borrow().clone();
        let mut last = format!("{} not seen on the network yet", self.name.borrow());
        for (addr, port) in candidates {
            if !reachable(addr, port).await {
                continue;
            }
            match self.ask(addr, port).await {
                Ok(answer) => {
                    *self.at.borrow_mut() = vec![(addr, port)];
                    self.reached.set(Some(addr));
                    return Ok(answer);
                }
                Err(e) => last = format!("{addr}: {e:#}"),
            }
        }
        self.at.borrow_mut().clear();
        self.reached.set(None);
        Ok(Some(last))
    }

    fn reached(&self) -> Option<IpAddr> {
        self.reached.get()
    }

    fn also_at(&self, addr: IpAddr) {
        self.given.set(Some(addr));
        self.at.borrow_mut().clear();
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
    fn the_wifi_of_this_pc() {
        assert_eq!(
            wifi_of_nmcli("no:Nachbar\nyes:Heim\\: oben\nno:Gast\n").as_deref(),
            Some("Heim: oben")
        );
        assert_eq!(wifi_of_nmcli("no:Nachbar\n"), None);
        assert_eq!(wifi_of_nmcli(""), None);
        let netsh = "\r\n    Name                   : WLAN\r\n    Status                 : Verbunden\r\n    \
                     SSID                   : Heim 5\r\n    BSSID                  : 12:34:56:78:9a:bc\r\n";
        assert_eq!(wifi_of_netsh(netsh).as_deref(), Some("Heim 5"));
        assert_eq!(wifi_of_netsh("    Status : getrennt\r\n"), None);
        assert_eq!(wifi_of_netsh("    SSID : Caf\u{e9}\r\n"), None);
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
