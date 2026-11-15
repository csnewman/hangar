//! The server as the authoritative DNS server for its own zone: the public
//! URL's host and every name under it.
//!
//! The zone is delegated to it: the parent zone's NS records name the
//! server's nameservers, with glue giving their addresses. Every name in
//! the zone resolves to Hangar (Hangar itself, and each environment's
//! hosts), so the zone needs no records made per environment. What it does
//! need is TXT records made on demand: ACME's DNS-01 challenge, the only
//! way to be issued the wildcard certificate environments' names need.
//! Those are in the store (`dns_records`), so every server sharing it
//! serves the same answers.
//!
//! It answers for the zone and only the zone: it does not recurse, and
//! refuses questions about any other name.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use hickory_proto::op::{Edns, Message, MessageType, Metadata, OpCode, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA, NS, SOA, TXT};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::Dns;
use crate::store::Store;
use crate::store::rows::DnsRecord;

/// The lifetime of the records the configuration implies.
const TTL: u32 = 300;

/// The largest answer a UDP query without EDNS is given.
const MIN_UDP: usize = 512;

/// A name as the zone keeps it: lower case, without a trailing dot.
pub fn canonical(name: &str) -> String {
    name.trim_end_matches('.').to_lowercase()
}

pub struct Zone {
    zone: String,
    apex: Name,
    cfg: Dns,
    store: Store,
}

impl Zone {
    pub fn new(zone: &str, cfg: Dns, store: Store) -> Result<Zone, String> {
        let zone = canonical(zone);
        let apex = Name::from_ascii(format!("{zone}."))
            .map_err(|e| format!("the DNS zone {zone:?}: {e}"))?;
        Ok(Zone {
            zone,
            apex,
            cfg,
            store,
        })
    }

    /// Answers on the configured address over UDP and TCP.
    pub async fn serve(self: Arc<Self>) -> Result<(), String> {
        let addr = self.cfg.listen.clone();
        let udp = crate::listen::udp(&addr)
            .await
            .map_err(|e| format!("listening for DNS on {addr} (UDP): {e}"))?;
        let tcp = crate::listen::tcp(&addr)
            .await
            .map_err(|e| format!("listening for DNS on {addr} (TCP): {e}"))?;
        eprintln!(
            "hangar-server: serving DNS for {} on {addr}, nameservers {}",
            self.zone,
            self.cfg.nameservers.join(", ")
        );
        let udp = Arc::new(udp);
        let z = self.clone();
        let datagrams = tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                let (n, from) = match udp.recv_from(&mut buf).await {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("hangar-server: DNS over UDP: {e}");
                        continue;
                    }
                };
                let query = buf[..n].to_vec();
                let (z, udp) = (z.clone(), udp.clone());
                tokio::spawn(async move {
                    if let Some(reply) = z.answer(&query, false).await {
                        let _ = udp.send_to(&reply, from).await;
                    }
                });
            }
        });
        loop {
            let (conn, _) = match tcp.accept().await {
                Ok(c) => c,
                Err(e) => {
                    if datagrams.is_finished() {
                        return Err(format!("DNS over TCP: {e}"));
                    }
                    continue;
                }
            };
            tokio::spawn(self.clone().stream(conn));
        }
    }

    /// Answers the queries on one TCP connection, each prefixed by its
    /// length, until it goes quiet.
    async fn stream(self: Arc<Self>, mut conn: tokio::net::TcpStream) {
        loop {
            let read = async {
                let n = conn.read_u16().await? as usize;
                let mut query = vec![0u8; n];
                conn.read_exact(&mut query).await?;
                Ok::<_, std::io::Error>(query)
            };
            let Ok(Ok(query)) = tokio::time::timeout(Duration::from_secs(10), read).await else {
                return;
            };
            let Some(reply) = self.answer(&query, true).await else {
                return;
            };
            let Ok(len) = u16::try_from(reply.len()) else {
                return;
            };
            if conn.write_u16(len).await.is_err() || conn.write_all(&reply).await.is_err() {
                return;
            }
        }
    }

    /// The reply to one query, if it is one.
    async fn answer(&self, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
        let req = match Message::from_vec(query) {
            Ok(m) => m,
            Err(_) if query.len() >= 2 => {
                let id = u16::from_be_bytes([query[0], query[1]]);
                return Message::error_msg(id, OpCode::Query, ResponseCode::FormErr)
                    .to_vec()
                    .ok();
            }
            Err(_) => return None,
        };
        if req.metadata.message_type != MessageType::Query {
            return None;
        }
        let mut m = Message::response(req.metadata.id, req.metadata.op_code);
        m.metadata = Metadata::response_from_request(&req.metadata);
        m.metadata.authoritative = true;
        m.queries = req.queries.iter().take(1).cloned().collect();
        self.fill(&req, &mut m).await;

        let mut size = MIN_UDP;
        if let Some(o) = &req.edns {
            size = size.max(o.max_payload() as usize);
            let mut edns = Edns::new();
            edns.set_max_payload(o.max_payload().max(MIN_UDP as u16));
            m.set_edns(edns);
        }
        let reply = m.to_vec().ok()?;
        if !tcp && reply.len() > size {
            return m.truncate().to_vec().ok();
        }
        Some(reply)
    }

    async fn fill(&self, req: &Message, m: &mut Message) {
        if req.metadata.op_code != OpCode::Query || req.queries.len() != 1 {
            m.metadata.response_code = ResponseCode::NotImp;
            return;
        }
        let q = &req.queries[0];
        let name = canonical(&q.name().to_ascii());
        if name != self.zone && !name.ends_with(&format!(".{}", self.zone)) {
            m.metadata.authoritative = false;
            m.metadata.response_code = ResponseCode::Refused;
            return;
        }
        let asked = q.name().clone();
        let qtype = q.query_type();
        match qtype {
            RecordType::A | RecordType::AAAA | RecordType::ANY => {
                for a in &self.cfg.addresses {
                    let data = match a {
                        IpAddr::V4(v4) if qtype != RecordType::AAAA => RData::A(A(*v4)),
                        IpAddr::V6(v6)
                            if qtype != RecordType::A && v6.to_ipv4_mapped().is_none() =>
                        {
                            RData::AAAA(AAAA(*v6))
                        }
                        _ => continue,
                    };
                    m.answers.push(Record::from_rdata(asked.clone(), TTL, data));
                }
            }
            RecordType::NS if name == self.zone => {
                for ns in &self.cfg.nameservers {
                    if let Ok(n) = Name::from_ascii(format!("{ns}.")) {
                        m.answers
                            .push(Record::from_rdata(asked.clone(), TTL, RData::NS(NS(n))));
                    }
                }
            }
            RecordType::SOA if name == self.zone => {
                if let Some(soa) = self.soa() {
                    m.answers.push(soa);
                }
            }
            RecordType::TXT => match self.txt(&name).await {
                Ok(recs) => {
                    for r in recs {
                        m.answers.push(Record::from_rdata(
                            asked.clone(),
                            r.ttl.max(0) as u32,
                            RData::TXT(TXT::new(split_txt(&r.value))),
                        ));
                    }
                }
                Err(e) => {
                    eprintln!("hangar-server: looking up TXT records of {name}: {e}");
                    m.metadata.response_code = ResponseCode::ServFail;
                    return;
                }
            },
            _ => {}
        }
        // Every name in the zone exists, so a question with no answer is
        // NODATA, which carries the SOA for negative caching.
        if m.answers.is_empty()
            && let Some(soa) = self.soa()
        {
            m.authorities.push(soa);
        }
    }

    async fn txt(&self, name: &str) -> Result<Vec<DnsRecord>, String> {
        let look = async {
            let mut tx = self.store.begin().await?;
            let recs = tx.dns_records(name, "TXT").await?;
            tx.commit().await?;
            Ok::<_, crate::store::Error>(recs)
        };
        match tokio::time::timeout(Duration::from_secs(2), look).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err("timed out".into()),
        }
    }

    fn soa(&self) -> Option<Record> {
        let ns = Name::from_ascii(format!("{}.", self.cfg.nameservers.first()?)).ok()?;
        let mbox =
            Name::from_ascii(format!("{}.", self.cfg.hostmaster.replacen('@', ".", 1))).ok()?;
        // Negative answers are cached for a minute: a challenge's record is
        // looked for soon after it is made.
        let soa = SOA::new(ns, mbox, 1, 3600, 600, 604800, 60);
        Some(Record::from_rdata(self.apex.clone(), TTL, RData::SOA(soa)))
    }
}

/// Cuts a value into the 255-byte strings a TXT record is made of.
fn split_txt(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = v;
    while rest.len() > 255 {
        let mut at = 255;
        while !rest.is_char_boundary(at) {
            at -= 1;
        }
        out.push(rest[..at].to_string());
        rest = &rest[at..];
    }
    out.push(rest.to_string());
    out
}

/// Keeps a challenge's record in the zone while it is answered.
pub async fn add_txt(store: &Store, name: &str, value: &str, ttl: i32) -> crate::store::Result<()> {
    let mut tx = store.begin().await?;
    tx.add_dns_record(&DnsRecord {
        name: canonical(name),
        r#type: "TXT".into(),
        value: value.into(),
        ttl,
        created_at: chrono::Utc::now(),
    })
    .await?;
    tx.commit().await
}

pub async fn remove_txt(store: &Store, name: &str, value: &str) -> crate::store::Result<()> {
    let mut tx = store.begin().await?;
    tx.remove_dns_record(&canonical(name), "TXT", value).await?;
    tx.commit().await
}

/// Removes, every ten minutes, records older than an hour: a challenge's
/// record is removed when the challenge is done, and one left by a server
/// that died midway is removed here.
pub async fn prune(store: Store) {
    let mut tick = tokio::time::interval(Duration::from_secs(600));
    loop {
        tick.tick().await;
        let pruned = async {
            let mut tx = store.begin().await?;
            let n = tx
                .prune_dns_records(chrono::Utc::now() - chrono::Duration::hours(1))
                .await?;
            tx.commit().await?;
            Ok::<_, crate::store::Error>(n)
        };
        match pruned.await {
            Ok(0) => {}
            Ok(n) => eprintln!("hangar-server: pruned {n} stale DNS records"),
            Err(e) => eprintln!("hangar-server: pruning DNS records: {e}"),
        }
    }
}
