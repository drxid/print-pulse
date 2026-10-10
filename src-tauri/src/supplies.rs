//! Consumables and counters read over SNMP: ink levels, the waste-ink
//! (maintenance) box and the lifetime page counter.
//!
//! The standard Printer-MIB (RFC 3805) is tried first. Many Epsons only report
//! "some ink remaining" there, so their binary "ST2" status (the same reply
//! the Epson utilities parse) fills the gaps. Neither path has been verified
//! against the L8180 yet — unknown values simply stay empty in the UI.

use serde::{Deserialize, Serialize};

use crate::snmp::{parse_oid, Client, Value};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Ink {
    /// Stable colour key the UI localizes: black, cyan, photoBlack, grey, …
    pub key: String,
    /// What the printer called it, shown when the key is `other`.
    pub label: String,
    /// Percent left; `None` when the printer doesn't say.
    pub level: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Supplies {
    /// When this was read (unix seconds).
    pub ts: i64,
    pub inks: Vec<Ink>,
    /// How full the maintenance box is, when reported as a number.
    pub waste_pct: Option<i32>,
    /// Epson's coarse maintenance box state: ok | near | full.
    pub waste_state: Option<String>,
    /// Lifetime page counter.
    pub pages: Option<i64>,
}

impl Supplies {
    fn is_empty(&self) -> bool {
        self.inks.is_empty() && self.waste_pct.is_none() && self.waste_state.is_none() && self.pages.is_none()
    }
}

const SUPPLIES_TABLE: &str = "1.3.6.1.2.1.43.11.1.1";
const LIFE_COUNT: &str = "1.3.6.1.2.1.43.10.2.1.4.1.1";
/// Epson private MIB: the remote "st" command (status, format ST2).
const EPSON_ST: &str = "1.3.6.1.4.1.1248.1.2.2.44.1.1.2.1.115.116.1.0.1";

fn ink_key(text: &str) -> &'static str {
    let t = text.to_lowercase();
    let has = |w: &str| t.contains(w);
    if has("photo black") || has("photoblack") {
        "photoBlack"
    } else if has("matte black") {
        "matteBlack"
    } else if has("light cyan") {
        "lightCyan"
    } else if has("light magenta") {
        "lightMagenta"
    } else if has("black") {
        "black"
    } else if has("cyan") {
        "cyan"
    } else if has("magenta") {
        "magenta"
    } else if has("yellow") {
        "yellow"
    } else if has("gray") || has("grey") {
        "grey"
    } else if has("red") {
        "red"
    } else {
        "other"
    }
}

#[derive(Default)]
struct Row {
    class: Option<i64>,
    kind: Option<i64>,
    desc: String,
    max: Option<i64>,
    level: Option<i64>,
}

/// prtMarkerSuppliesTable → inks and the waste receptacle.
fn from_printer_mib(c: &mut Client, out: &mut Supplies) {
    let base = parse_oid(SUPPLIES_TABLE);
    let mut rows: Vec<(Vec<u32>, Row)> = Vec::new();
    for (oid, val) in c.walk(&base) {
        // base . column . hrDeviceIndex . supplyIndex
        let Some((&col, index)) = oid[base.len()..].split_first() else { continue };
        let pos = match rows.iter().position(|(i, _)| i.as_slice() == index) {
            Some(p) => p,
            None => {
                rows.push((index.to_vec(), Row::default()));
                rows.len() - 1
            }
        };
        let row = &mut rows[pos].1;
        match col {
            4 => row.class = val.as_int(),
            5 => row.kind = val.as_int(),
            6 => row.desc = val.as_text().unwrap_or_default(),
            8 => row.max = val.as_int(),
            9 => row.level = val.as_int(),
            _ => {}
        }
    }

    for (_, r) in rows {
        // Negative levels: -2 unknown, -3 "some remaining".
        let pct = match (r.level, r.max) {
            (Some(l), Some(m)) if l >= 0 && m > 0 => Some((l * 100 / m).clamp(0, 100) as i32),
            _ => None,
        };
        let waste = r.kind == Some(8) || r.class == Some(4) || r.desc.to_lowercase().contains("maintenance");
        if waste {
            // For a receptacle the level is the space left, so invert it.
            if let Some(p) = pct {
                out.waste_pct = Some(100 - p);
            }
        } else if matches!(r.kind, Some(5) | Some(6)) || ink_key(&r.desc) != "other" {
            out.inks.push(Ink { key: ink_key(&r.desc).into(), label: r.desc, level: pct });
        }
    }
}

/// ST2 ink record → colour. The second byte is the colour; both blacks share
/// 0x00 and are told apart by the first (cartridge) byte. Codes checked on an
/// L8180: 0b/00 black, 01/00 photo black, 03/01 cyan, 04/02 magenta,
/// 05/03 yellow, 16/13 grey.
fn st2_colour(cartridge: u8, code: u8) -> (&'static str, &'static str) {
    match code {
        0x00 if cartridge == 0x01 => ("photoBlack", "Photo Black"),
        0x00 => ("black", "Black"),
        0x01 => ("cyan", "Cyan"),
        0x02 => ("magenta", "Magenta"),
        0x03 => ("yellow", "Yellow"),
        0x04 => ("lightCyan", "Light Cyan"),
        0x05 => ("lightMagenta", "Light Magenta"),
        0x07 => ("grey", "Grey"),
        0x08 => ("photoBlack", "Light Black"),
        0x09 => ("red", "Red"),
        0x13 => ("grey", "Grey"),
        _ => ("other", ""),
    }
}

/// Parsed bits of an Epson "@BDC ST2" reply.
#[derive(Debug, Default, PartialEq)]
struct St2 {
    /// (cartridge, colour, level)
    inks: Vec<(u8, u8, u8)>,
    waste: Option<u8>,
}

fn parse_st2(b: &[u8]) -> Option<St2> {
    const HDR: &[u8] = b"@BDC ST2\r\n";
    let start = b.windows(HDR.len()).position(|w| w == HDR)? + HDR.len();
    let len = u16::from_le_bytes([*b.get(start)?, *b.get(start + 1)?]) as usize;
    let body = b.get(start + 2..)?;
    let body = &body[..len.min(body.len())];

    let mut st = St2::default();
    let mut i = 0;
    while i + 2 <= body.len() {
        let (ftype, flen) = (body[i], body[i + 1] as usize);
        let Some(data) = body.get(i + 2..i + 2 + flen) else { break };
        match ftype {
            0x0f if !data.is_empty() => {
                let step = data[0] as usize;
                if step >= 3 {
                    st.inks = data[1..].chunks_exact(step).map(|r| (r[0], r[1], r[2])).collect();
                }
            }
            // Maintenance box: one status byte per box; 0 ok, 1 near full, 2 full.
            0x37 if data.len() >= 2 => {
                let step = (data[0] as usize).max(1);
                st.waste = data[1..].iter().step_by(step).copied().max();
            }
            _ => {}
        }
        i += 2 + flen;
    }
    Some(st)
}

fn from_epson_st2(c: &mut Client, out: &mut Supplies) {
    let Some(Value::Bytes(reply)) = c.get(&parse_oid(EPSON_ST)) else { return };
    let Some(st) = parse_st2(&reply) else { return };

    // Prefer Epson's numbers when the standard MIB had no real levels.
    if !st.inks.is_empty() && out.inks.iter().all(|i| i.level.is_none()) {
        out.inks = st
            .inks
            .iter()
            .map(|&(cartridge, code, level)| {
                let (key, name) = st2_colour(cartridge, code);
                Ink {
                    key: key.into(),
                    label: if name.is_empty() { format!("#{code:02x}") } else { name.into() },
                    level: Some(level.min(100) as i32),
                }
            })
            .collect();
    }
    out.waste_state = st.waste.map(|w| match w {
        0 => "ok",
        1 => "near",
        _ => "full",
    }.into());
}

/// Read everything we can; `None` when the printer doesn't answer SNMP at all.
pub fn fetch(host: &str, now: i64) -> Option<Supplies> {
    let mut c = Client::new(host).ok()?;
    let mut out = Supplies { ts: now, ..Default::default() };
    from_printer_mib(&mut c, &mut out);
    out.pages = c.get(&parse_oid(LIFE_COUNT)).and_then(|v| v.as_int());
    from_epson_st2(&mut c, &mut out);
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn st2_inks_and_waste() {
        let mut body = vec![0x01, 0x01, 0x04]; // status: idle
        body.extend([0x0f, 7, 3, 0x0b, 0x00, 80, 0x03, 0x01, 15]); // black 80, cyan 15
        body.extend([0x37, 3, 1, 0, 1]); // two boxes: ok, near full
        let mut msg = b"\x00@BDC ST2\r\n".to_vec();
        msg.extend((body.len() as u16).to_le_bytes());
        msg.extend(&body);

        let st = parse_st2(&msg).unwrap();
        assert_eq!(st.inks, vec![(0x0b, 0x00, 80), (0x03, 0x01, 15)]);
        assert_eq!(st.waste, Some(1));
    }

    /// Real reply from an EPSON L8180 (2026-10-10).
    #[test]
    fn st2_l8180() {
        let reply: &[u8] = &[
            0x00, 0x40, 0x42, 0x44, 0x43, 0x20, 0x53, 0x54, 0x32, 0x0d, 0x0a, 0xa0, 0x00, 0x01, 0x01, 0x04,
            0x0f, 0x13, 0x03, 0x0b, 0x00, 0x51, 0x01, 0x00, 0x4d, 0x03, 0x01, 0x4d, 0x05, 0x03, 0x4e, 0x04,
            0x02, 0x4d, 0x16, 0x13, 0x4e, 0x37, 0x03, 0x02, 0x00, 0x00,
        ];
        let st = parse_st2(reply).unwrap();
        let named: Vec<(&str, u8)> = st.inks.iter().map(|&(c, k, l)| (st2_colour(c, k).0, l)).collect();
        assert_eq!(
            named,
            [("black", 81), ("photoBlack", 77), ("cyan", 77), ("yellow", 78), ("magenta", 77), ("grey", 78)]
        );
        assert_eq!(st.waste, Some(0));
    }

    #[test]
    fn ink_names() {
        assert_eq!(ink_key("Photo Black ink"), "photoBlack");
        assert_eq!(ink_key("Light Cyan Ink"), "lightCyan");
        assert_eq!(ink_key("Black ink"), "black");
        assert_eq!(ink_key("Gray Ink"), "grey");
        assert_eq!(ink_key("Maintenance Box"), "other");
    }
}

