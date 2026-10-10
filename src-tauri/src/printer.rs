//! Everything that talks to the printer: enumerating queues, watching the job
//! queue, checking that a network printer is reachable, and the two kinds of
//! maintenance output — Epson "remote mode" commands sent as RAW data, and a
//! small colour test page drawn through GDI (so any driver can print it).

use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

use serde::Serialize;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PrinterInfo {
    pub name: String,
    pub port: String,
    pub is_default: bool,
    /// Network address parsed from the port name, if the printer is on the LAN.
    pub host: Option<String>,
}

const EXIT_PACKET_MODE: &[u8] = b"\x00\x00\x00\x1b\x01@EJL 1284.4\n@EJL     \n";
const ENTER_REMOTE_MODE: &[u8] = b"\x1b@\x1b@\x1b(R\x08\x00\x00REMOTE1";
const EXIT_REMOTE_MODE: &[u8] = b"\x1b\x00\x00\x00";

/// One remote-mode command: two letters, little-endian argument length, arguments.
fn remote_cmd(cmd: &[u8; 2], args: &[u8]) -> Vec<u8> {
    let mut v = cmd.to_vec();
    v.extend_from_slice(&(args.len() as u16).to_le_bytes());
    v.extend_from_slice(args);
    v
}

/// "TI": sync the printer clock, as Epson's own utility does before maintenance.
fn set_timer(now: chrono::NaiveDateTime) -> Vec<u8> {
    use chrono::{Datelike, Timelike};
    let mut args = vec![0x00];
    args.extend_from_slice(&(now.year() as u16).to_be_bytes());
    args.extend_from_slice(&[now.month() as u8, now.day() as u8, now.hour() as u8, now.minute() as u8, now.second() as u8]);
    remote_cmd(b"TI", &args)
}

/// Printer's built-in nozzle check pattern — the cheapest way to fire every nozzle.
///
/// Byte-for-byte what the L8180 driver's own "Nozzle Check" utility spools
/// (captured from the queue). The form feed after the pattern is what ejects
/// the sheet: without it the printer finishes the pattern and then waits with
/// the paper inside, the head shuttling, until it is switched off.
fn nozzle_check_job(now: chrono::NaiveDateTime) -> Vec<u8> {
    [
        EXIT_PACKET_MODE,
        ENTER_REMOTE_MODE,
        &set_timer(now),
        &remote_cmd(b"JS", &[0x00, 0x00, 0x00, 0x00]),
        // Paper path for the pattern sheet, as the driver sends it.
        &remote_cmd(b"PP", &[0x00, 0x01, 0xfe]),
        &remote_cmd(b"NC", &[0x00, 0x00]),
        EXIT_REMOTE_MODE,
        b"\r\n\r\n",
        b"\x1b(R\x08\x00\x00REMOTE1",
        &remote_cmd(b"VI", &[0x00, 0x00]),
        &remote_cmd(b"LD", &[]),
        EXIT_REMOTE_MODE,
        b"\x0c",
        ENTER_REMOTE_MODE,
        &remote_cmd(b"JE", &[0x00]),
        EXIT_REMOTE_MODE,
    ]
    .concat()
}

pub fn nozzle_check_bytes() -> Vec<u8> {
    nozzle_check_job(chrono::Local::now().naive_local())
}

/// A paperless maintenance job (head cleaning), framed the way Epson's utility
/// sends it (see Ircama/epson_escp2 `clean_nozzles`), ending with "JE".
fn maintenance_job(cmd: &[u8; 2], args: &[u8]) -> Vec<u8> {
    let now = chrono::Local::now().naive_local();
    [
        EXIT_PACKET_MODE,
        ENTER_REMOTE_MODE,
        &set_timer(now),
        &remote_cmd(cmd, args),
        EXIT_REMOTE_MODE,
        ENTER_REMOTE_MODE,
        &remote_cmd(b"JE", &[0x00]),
        EXIT_REMOTE_MODE,
    ]
    .concat()
}

/// Built-in head cleaning cycle for all colours (uses noticeably more ink than a nozzle check).
pub fn head_cleaning_bytes() -> Vec<u8> {
    maintenance_job(b"CH", &[0x00, 0x00])
}

/// Pull an IPv4 address out of a port name such as `EP_192.168.68.121` or
/// `IP_10.0.0.5`. WSD / USB ports have none, so reachability can't be checked.
pub fn host_from_port(port: &str) -> Option<String> {
    port.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|tok| tok.parse::<std::net::Ipv4Addr>().is_ok())
        .map(str::to_string)
}

/// True if anything answers at `host`. A refused connection still proves the
/// device is powered and on the network, so it counts as reachable.
pub fn is_reachable(host: &str) -> bool {
    let Ok(ip) = host.parse::<IpAddr>() else {
        return false;
    };
    [515u16, 9100, 80, 631].iter().any(|&port| {
        match TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_millis(1200)) {
            Ok(_) => true,
            Err(e) => e.kind() == std::io::ErrorKind::ConnectionRefused,
        }
    })
}

/// Nudge a printer that may be in network sleep: a UDP SNMP request and a few
/// TCP connection attempts are usually enough to bring the NIC back up.
fn poke(ip: IpAddr) {
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        let req = crate::snmp::get_request(&crate::snmp::parse_oid("1.3.6.1.2.1.1.3.0"));
        let _ = sock.send_to(&req, SocketAddr::new(ip, 161));
    }
}

/// Before giving up on a scheduled print: try to wake the printer and give it
/// time to come back. Returns true as soon as it answers.
pub fn wake(host: &str, attempts: u32, gap: Duration) -> bool {
    let Ok(ip) = host.parse::<IpAddr>() else {
        return false;
    };
    for i in 0..attempts {
        poke(ip);
        if is_reachable(host) {
            return true;
        }
        if i + 1 < attempts {
            std::thread::sleep(gap);
        }
    }
    false
}

/// A job in the Windows print queue.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: u32,
    pub document: String,
    /// The spooler flagged it: error, offline, out of paper, blocked, paused…
    pub problem: bool,
}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::{host_from_port, PrinterInfo};
    use windows::core::{HSTRING, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{COLORREF, RECT};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Graphics::Printing::*;
    use windows::Win32::Storage::Xps::{EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};

    fn wide_to_string(p: PWSTR) -> String {
        if p.is_null() {
            String::new()
        } else {
            unsafe { p.to_string().unwrap_or_default() }
        }
    }

    /// 8-byte aligned scratch buffer for the variable-size structs winspool returns.
    fn aligned(bytes: u32) -> Vec<u64> {
        vec![0u64; (bytes as usize).div_ceil(8).max(1)]
    }

    fn as_bytes(buf: &mut [u64]) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) }
    }

    pub fn default_printer() -> Option<String> {
        unsafe {
            let mut len = 0u32;
            let _ = GetDefaultPrinterW(None, &mut len);
            if len == 0 {
                return None;
            }
            let mut buf = vec![0u16; len as usize];
            if GetDefaultPrinterW(Some(PWSTR(buf.as_mut_ptr())), &mut len).as_bool() {
                Some(String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]))
            } else {
                None
            }
        }
    }

    pub fn list_printers() -> Vec<PrinterInfo> {
        let default = default_printer();
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        let mut out = Vec::new();
        unsafe {
            let (mut needed, mut count) = (0u32, 0u32);
            let _ = EnumPrintersW(flags, PCWSTR::null(), 2, None, &mut needed, &mut count);
            if needed == 0 {
                return out;
            }
            let mut buf = aligned(needed);
            if EnumPrintersW(flags, PCWSTR::null(), 2, Some(as_bytes(&mut buf)), &mut needed, &mut count).is_err() {
                return out;
            }
            let items = std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_2W, count as usize);
            for p in items {
                let name = wide_to_string(p.pPrinterName);
                let port = wide_to_string(p.pPortName);
                out.push(PrinterInfo {
                    is_default: default.as_deref() == Some(name.as_str()),
                    host: host_from_port(&port),
                    name,
                    port,
                });
            }
        }
        out
    }

    struct Handle(PRINTER_HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = ClosePrinter(self.0);
            }
        }
    }

    fn open(name: &str) -> Result<Handle, String> {
        let mut h = PRINTER_HANDLE::default();
        unsafe { OpenPrinterW(&HSTRING::from(name), &mut h, None) }
            .map_err(|e| format!("OpenPrinter: {}", e.message()))?;
        Ok(Handle(h))
    }

    pub fn list_jobs(name: &str) -> Result<Vec<super::Job>, String> {
        let h = open(name)?;
        let problem_bits = JOB_STATUS_ERROR
            | JOB_STATUS_OFFLINE
            | JOB_STATUS_PAPEROUT
            | JOB_STATUS_BLOCKED_DEVQ
            | JOB_STATUS_USER_INTERVENTION
            | JOB_STATUS_PAUSED;
        // Finished jobs linger when "keep printed documents" is on; they aren't waiting.
        let done_bits = JOB_STATUS_PRINTED | JOB_STATUS_COMPLETE | JOB_STATUS_DELETED | JOB_STATUS_DELETING;
        unsafe {
            let (mut needed, mut count) = (0u32, 0u32);
            let _ = EnumJobsW(h.0, 0, 255, 1, None, &mut needed, &mut count);
            if needed == 0 {
                return Ok(Vec::new());
            }
            let mut buf = aligned(needed);
            EnumJobsW(h.0, 0, 255, 1, Some(as_bytes(&mut buf)), &mut needed, &mut count)
                .map_err(|e| format!("EnumJobs: {}", e.message()))?;
            let items = std::slice::from_raw_parts(buf.as_ptr() as *const JOB_INFO_1W, count as usize);
            Ok(items
                .iter()
                .filter(|j| j.Status & done_bits == 0)
                .map(|j| super::Job {
                    id: j.JobId,
                    document: wide_to_string(j.pDocument),
                    problem: j.Status & problem_bits != 0,
                })
                .collect())
        }
    }

    /// Cancel the given jobs; returns how many were actually removed.
    pub fn delete_jobs(name: &str, ids: &[u32]) -> Result<u32, String> {
        let h = open(name)?;
        let mut removed = 0;
        for &id in ids {
            if unsafe { SetJobW(h.0, id, 0, None, JOB_CONTROL_DELETE) }.as_bool() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Name of a VPN tunnel that is up, if any. A full-tunnel WireGuard VPN
    /// (AmneziaVPN and the like) firewalls off the whole LAN, so the printer
    /// looks offline even though it is on.
    pub fn active_vpn() -> Option<String> {
        use windows::Win32::NetworkManagement::IpHelper::*;
        use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
        use windows::Win32::Networking::WinSock::AF_INET;

        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 16 * 1024u32;
        let mut buf = aligned(size);
        unsafe {
            let mut first = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
            let mut rc = GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(first), &mut size);
            if rc == 111 {
                // ERROR_BUFFER_OVERFLOW: `size` now holds what is needed.
                buf = aligned(size);
                first = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
                rc = GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(first), &mut size);
            }
            if rc != 0 {
                return None;
            }
            let mut cur = first as *const IP_ADAPTER_ADDRESSES_LH;
            while let Some(a) = cur.as_ref() {
                let desc = wide_to_string(a.Description);
                let d = desc.to_lowercase();
                let tunnel = ["wireguard", "wintun", "amnezia", "tap-", "openvpn", "vpn", "tunnel"]
                    .iter()
                    .any(|w| d.contains(w));
                if a.OperStatus == IfOperStatusUp && (tunnel || a.IfType == IF_TYPE_PROP_VIRTUAL) {
                    return Some(desc);
                }
                cur = a.Next;
            }
        }
        None
    }

    /// Spool bytes straight to the device, bypassing the driver's rendering.
    pub fn send_raw(name: &str, doc_name: &str, data: &[u8]) -> Result<(), String> {
        let h = open(name)?;
        let mut doc: Vec<u16> = doc_name.encode_utf16().chain(Some(0)).collect();
        let mut datatype: Vec<u16> = "RAW".encode_utf16().chain(Some(0)).collect();
        let info = DOC_INFO_1W {
            pDocName: PWSTR(doc.as_mut_ptr()),
            pOutputFile: PWSTR::null(),
            pDatatype: PWSTR(datatype.as_mut_ptr()),
        };
        unsafe {
            if StartDocPrinterW(h.0, 1, &info) == 0 {
                return Err(format!("StartDocPrinter: {}", windows::core::Error::from_win32().message()));
            }
            let _ = StartPagePrinter(h.0);
            let mut written = 0u32;
            let ok = WritePrinter(h.0, data.as_ptr() as _, data.len() as u32, &mut written).as_bool();
            let _ = EndPagePrinter(h.0);
            let _ = EndDocPrinter(h.0);
            if !ok || written as usize != data.len() {
                return Err(format!("WritePrinter: {}/{} bytes", written, data.len()));
            }
        }
        Ok(())
    }

    const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
    }

    /// A small strip of solid swatches covering every ink (C, M, Y, K, grey) plus
    /// the RGB mixes, so all channels fire while using very little ink.
    pub fn print_test_page(name: &str, title: &str, subtitle: &str) -> Result<(), String> {
        unsafe {
            let hdc = CreateDCW(&HSTRING::from("WINSPOOL"), &HSTRING::from(name), PCWSTR::null(), None);
            if hdc.is_invalid() {
                return Err("CreateDC failed".into());
            }
            let result = draw_page(hdc, title, subtitle);
            let _ = DeleteDC(hdc);
            result
        }
    }

    unsafe fn draw_page(hdc: HDC, title: &str, subtitle: &str) -> Result<(), String> {
        let doc: Vec<u16> = "Print Pulse".encode_utf16().chain(Some(0)).collect();
        let di = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: PCWSTR(doc.as_ptr()),
            ..Default::default()
        };
        if StartDocW(hdc, &di) <= 0 {
            return Err("StartDoc failed".into());
        }
        if StartPage(hdc) <= 0 {
            let _ = EndDoc(hdc);
            return Err("StartPage failed".into());
        }

        let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSX).max(72);
        let page_w = GetDeviceCaps(Some(hdc), HORZRES);
        let mm = |v: i32| v * dpi * 10 / 254;
        let left = mm(15);
        let mut y = mm(15);

        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, rgb(0, 0, 0));
        let face: Vec<u16> = "Segoe UI".encode_utf16().chain(Some(0)).collect();
        let font = CreateFontW(
            -mm(5), 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
            DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY, 0, PCWSTR(face.as_ptr()),
        );
        let old = SelectObject(hdc, font.into());
        let t: Vec<u16> = title.encode_utf16().collect();
        let _ = TextOutW(hdc, left, y, &t);
        SelectObject(hdc, old);
        let _ = DeleteObject(font.into());

        y += mm(8);
        let small = CreateFontW(
            -mm(3), 0, 0, 0, 400, 0, 0, 0,
            DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY, 0, PCWSTR(face.as_ptr()),
        );
        let old = SelectObject(hdc, small.into());
        let s: Vec<u16> = subtitle.encode_utf16().collect();
        let _ = TextOutW(hdc, left, y, &s);
        SelectObject(hdc, old);
        let _ = DeleteObject(small.into());

        y += mm(8);
        let swatches = [
            rgb(0, 174, 239),  // cyan
            rgb(236, 0, 140),  // magenta
            rgb(255, 230, 0),  // yellow
            rgb(0, 0, 0),      // black
            rgb(128, 128, 128),// grey
            rgb(230, 30, 40),  // red  (M+Y)
            rgb(0, 160, 80),   // green (C+Y)
            rgb(40, 60, 170),  // blue (C+M)
        ];
        let usable = (page_w - 2 * left).max(mm(80));
        let gap = mm(2);
        let w = (usable - gap * (swatches.len() as i32 - 1)) / swatches.len() as i32;
        let h = mm(12);
        for (i, color) in swatches.iter().enumerate() {
            let x = left + i as i32 * (w + gap);
            let brush = CreateSolidBrush(*color);
            let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
            FillRect(hdc, &rect, brush);
            let _ = DeleteObject(brush.into());
        }

        EndPage(hdc);
        if EndDoc(hdc) <= 0 {
            return Err("EndDoc failed".into());
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod other {
    use super::PrinterInfo;
    pub fn list_printers() -> Vec<PrinterInfo> {
        Vec::new()
    }
    pub fn default_printer() -> Option<String> {
        None
    }
    pub fn list_jobs(_: &str) -> Result<Vec<super::Job>, String> {
        Err("unsupported platform".into())
    }
    pub fn active_vpn() -> Option<String> {
        None
    }
    pub fn delete_jobs(_: &str, _: &[u32]) -> Result<u32, String> {
        Err("unsupported platform".into())
    }
    pub fn send_raw(_: &str, _: &str, _: &[u8]) -> Result<(), String> {
        Err("unsupported platform".into())
    }
    pub fn print_test_page(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err("unsupported platform".into())
    }
}
#[cfg(not(windows))]
pub use other::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// What the EPSON L8180 driver spooled for "Nozzle Check" on 2026-10-10 17:34:41.
    const DRIVER_NOZZLE_CHECK: &[u8] = b"\x00\x00\x00\x1b\x01@EJL 1284.4\n@EJL     \n\x1b@\x1b@\x1b(R\x08\x00\x00REMOTE1TI\x08\x00\x00\x07\xea\n\n\x11\")JS\x04\x00\x00\x00\x00\x00PP\x03\x00\x00\x01\xfeNC\x02\x00\x00\x00\x1b\x00\x00\x00\r\n\r\n\x1b(R\x08\x00\x00REMOTE1VI\x02\x00\x00\x00LD\x00\x00\x1b\x00\x00\x00\x0c\x1b@\x1b@\x1b(R\x08\x00\x00REMOTE1JE\x01\x00\x00\x1b\x00\x00\x00";

    #[test]
    fn nozzle_check_matches_driver() {
        let at = chrono::NaiveDate::from_ymd_opt(2026, 10, 10).unwrap().and_hms_opt(17, 34, 41).unwrap();
        assert_eq!(nozzle_check_job(at), DRIVER_NOZZLE_CHECK);
    }

    #[test]
    fn cleaning_ends_the_job() {
        assert!(head_cleaning_bytes().ends_with(b"JE\x01\x00\x00\x1b\x00\x00\x00"));
    }
}
