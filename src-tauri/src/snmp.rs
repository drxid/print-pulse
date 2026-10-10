//! Minimal SNMPv1 client (GET / GETNEXT walk over UDP) — just enough to read
//! ink levels and counters from a network printer without extra dependencies.

use std::net::UdpSocket;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    Oid(#[allow(dead_code)] Vec<u32>),
    Null,
    /// noSuchObject / noSuchInstance / endOfMibView
    Missing,
}

impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_text(&self) -> Option<String> {
        match self {
            Value::Bytes(b) => Some(String::from_utf8_lossy(b).trim_end_matches('\0').trim().to_string()),
            _ => None,
        }
    }
}

pub fn parse_oid(s: &str) -> Vec<u32> {
    s.trim_start_matches('.').split('.').filter_map(|p| p.parse().ok()).collect()
}

// ---- BER encoding -----------------------------------------------------------

fn enc_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes: Vec<u8> = (len as u32).to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
}

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    enc_len(body.len(), &mut out);
    out.extend_from_slice(body);
    out
}

fn enc_int(v: i64) -> Vec<u8> {
    let mut bytes = v.to_be_bytes().to_vec();
    while bytes.len() > 1
        && ((bytes[0] == 0 && bytes[1] & 0x80 == 0) || (bytes[0] == 0xff && bytes[1] & 0x80 != 0))
    {
        bytes.remove(0);
    }
    tlv(0x02, &bytes)
}

fn enc_oid(oid: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    if oid.len() >= 2 {
        body.push((oid[0] * 40 + oid[1]) as u8);
    }
    for &sub in oid.iter().skip(2) {
        let mut chunk = vec![(sub & 0x7f) as u8];
        let mut v = sub >> 7;
        while v > 0 {
            chunk.push(0x80 | (v & 0x7f) as u8);
            v >>= 7;
        }
        chunk.reverse();
        body.extend(chunk);
    }
    tlv(0x06, &body)
}

fn request(pdu_tag: u8, community: &str, req_id: i64, oid: &[u32]) -> Vec<u8> {
    let varbind = tlv(0x30, &[enc_oid(oid), vec![0x05, 0x00]].concat());
    let pdu = tlv(
        pdu_tag,
        &[enc_int(req_id), enc_int(0), enc_int(0), tlv(0x30, &varbind)].concat(),
    );
    tlv(0x30, &[enc_int(0), tlv(0x04, community.as_bytes()), pdu].concat())
}

/// A standalone GET for `public`, for fire-and-forget use (waking a device).
pub fn get_request(oid: &[u32]) -> Vec<u8> {
    request(0xa0, "public", 1, oid)
}

// ---- BER decoding -----------------------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn read(&mut self) -> Option<(u8, &'a [u8])> {
        let tag = *self.buf.get(self.pos)?;
        let mut p = self.pos + 1;
        let first = *self.buf.get(p)?;
        p += 1;
        let len = if first & 0x80 == 0 {
            first as usize
        } else {
            let n = (first & 0x7f) as usize;
            let mut l = 0usize;
            for _ in 0..n {
                l = (l << 8) | *self.buf.get(p)? as usize;
                p += 1;
            }
            l
        };
        let body = self.buf.get(p..p + len)?;
        self.pos = p + len;
        Some((tag, body))
    }
}

fn dec_int(b: &[u8]) -> i64 {
    let mut v: i64 = if b.first().is_some_and(|x| x & 0x80 != 0) { -1 } else { 0 };
    for &x in b {
        v = (v << 8) | x as i64;
    }
    v
}

fn dec_uint(b: &[u8]) -> i64 {
    b.iter().fold(0i64, |v, &x| (v << 8) | x as i64)
}

fn dec_oid(b: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    if let Some(&first) = b.first() {
        out.push(first as u32 / 40);
        out.push(first as u32 % 40);
    }
    let mut acc = 0u32;
    for &x in b.iter().skip(1) {
        acc = (acc << 7) | (x & 0x7f) as u32;
        if x & 0x80 == 0 {
            out.push(acc);
            acc = 0;
        }
    }
    out
}

fn parse_response(buf: &[u8], req_id: i64) -> Option<(Vec<u32>, Value)> {
    let (_, msg) = Reader { buf, pos: 0 }.read()?;
    let mut r = Reader { buf: msg, pos: 0 };
    r.read()?; // version
    r.read()?; // community
    let (tag, pdu) = r.read()?;
    if tag != 0xa2 {
        return None;
    }
    let mut p = Reader { buf: pdu, pos: 0 };
    if dec_int(p.read()?.1) != req_id {
        return None;
    }
    let status = dec_int(p.read()?.1);
    p.read()?; // error index
    let (_, list) = p.read()?;
    if status != 0 {
        return Some((Vec::new(), Value::Missing));
    }
    let (_, vb) = Reader { buf: list, pos: 0 }.read()?;
    let mut v = Reader { buf: vb, pos: 0 };
    let (_, oid) = v.read()?;
    let (vtag, val) = v.read()?;
    let value = match vtag {
        0x02 => Value::Int(dec_int(val)),
        0x41 | 0x42 | 0x43 | 0x46 => Value::Int(dec_uint(val)),
        0x04 => Value::Bytes(val.to_vec()),
        0x06 => Value::Oid(dec_oid(val)),
        0x05 => Value::Null,
        _ => Value::Missing,
    };
    Some((dec_oid(oid), value))
}

pub struct Client {
    sock: UdpSocket,
    community: String,
    req_id: i64,
}

impl Client {
    pub fn new(host: &str) -> std::io::Result<Self> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.set_read_timeout(Some(Duration::from_millis(1500)))?;
        sock.connect((host, 161))?;
        Ok(Self { sock, community: "public".into(), req_id: 0x5050 })
    }

    fn call(&mut self, tag: u8, oid: &[u32]) -> Option<(Vec<u32>, Value)> {
        for _ in 0..2 {
            self.req_id += 1;
            let req = request(tag, &self.community, self.req_id, oid);
            if self.sock.send(&req).is_err() {
                return None;
            }
            let mut buf = vec![0u8; 8192];
            // Skip stale replies to earlier (timed-out) requests.
            while let Ok(n) = self.sock.recv(&mut buf) {
                if let Some(r) = parse_response(&buf[..n], self.req_id) {
                    return Some(r);
                }
            }
        }
        None
    }

    pub fn get(&mut self, oid: &[u32]) -> Option<Value> {
        self.call(0xa0, oid).map(|(_, v)| v).filter(|v| !matches!(v, Value::Missing))
    }

    /// All (oid, value) pairs under `base`, in order.
    pub fn walk(&mut self, base: &[u32]) -> Vec<(Vec<u32>, Value)> {
        let mut out = Vec::new();
        let mut cur = base.to_vec();
        for _ in 0..256 {
            match self.call(0xa1, &cur) {
                Some((oid, val)) if oid.starts_with(base) && !matches!(val, Value::Missing) => {
                    cur = oid.clone();
                    out.push((oid, val));
                }
                _ => break,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oid_roundtrip() {
        let oid = parse_oid("1.3.6.1.4.1.1248.1.2.2.44.1.1.2.1.115.116.1.0.1");
        let enc = enc_oid(&oid);
        assert_eq!(dec_oid(&enc[2..]), oid);
    }

    #[test]
    fn int_encoding() {
        assert_eq!(enc_int(0), vec![0x02, 0x01, 0x00]);
        assert_eq!(enc_int(128), vec![0x02, 0x02, 0x00, 0x80]);
        assert_eq!(dec_int(&[0xff]), -1);
        assert_eq!(dec_int(&[0x00, 0x80]), 128);
    }

    #[test]
    fn response_parsing() {
        // Hand-built GetResponse for sysName.0 = "EPSON"
        let oid = parse_oid("1.3.6.1.2.1.1.5.0");
        let vb = tlv(0x30, &[enc_oid(&oid), tlv(0x04, b"EPSON")].concat());
        let pdu = tlv(0xa2, &[enc_int(7), enc_int(0), enc_int(0), tlv(0x30, &vb)].concat());
        let msg = tlv(0x30, &[enc_int(0), tlv(0x04, b"public"), pdu].concat());
        let (got_oid, val) = parse_response(&msg, 7).unwrap();
        assert_eq!(got_oid, oid);
        assert_eq!(val.as_text().as_deref(), Some("EPSON"));
    }
}
