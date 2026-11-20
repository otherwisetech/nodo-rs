//! MAC addresses and the offline vendor lookup (IEEE MA-L registry, embedded gzip; refresh it
//! with `script/update-oui.py`).
use std::io::Read as _;
use std::sync::OnceLock;

/// `0:1a:2B:3:4:5`, `00-1A-2B-03-04-05`, `001a.2b03.0405`, `001A2B030405` → `00:1a:2b:03:04:05`.
pub fn normalize(s: &str) -> Option<String> {
    let s = s.trim();
    let parts: Vec<&str> = if s.contains(':') {
        s.split(':').collect()
    } else if s.contains('-') {
        s.split('-').collect()
    } else if s.contains('.') {
        let hex: String = s.split('.').collect();
        return from_hex(&hex);
    } else {
        return from_hex(s);
    };
    if parts.len() != 6 || parts.iter().any(|p| p.is_empty() || p.len() > 2 || !p.chars().all(|c| c.is_ascii_hexdigit())) {
        return None;
    }
    Some(parts.iter().map(|p| format!("{:0>2}", p.to_lowercase())).collect::<Vec<_>>().join(":"))
}

fn from_hex(h: &str) -> Option<String> {
    if h.len() != 12 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let h = h.to_lowercase();
    Some((0..6).map(|i| &h[i * 2..i * 2 + 2]).collect::<Vec<_>>().join(":"))
}

/// Raw bytes of a normalised MAC.
pub fn bytes(mac: &str) -> Option<[u8; 6]> {
    let n = normalize(mac)?;
    let mut out = [0u8; 6];
    for (i, p) in n.split(':').enumerate() {
        out[i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(out)
}

/// Broadcast, multicast and all-zero addresses are not devices.
pub fn is_device(mac: &str) -> bool {
    match bytes(mac) {
        Some(b) => b != [0xff; 6] && b != [0; 6] && b[0] & 1 == 0,
        None => false,
    }
}

/// Locally administered (randomised "private Wi-Fi address" on phones): no vendor to look up,
/// and it may change.
pub fn is_random(mac: &str) -> bool {
    bytes(mac).is_some_and(|b| b[0] & 2 != 0)
}

static OUI_GZ: &[u8] = include_bytes!("../data/oui.tsv.gz");

fn table() -> &'static Vec<([u8; 3], &'static str)> {
    static T: OnceLock<Vec<([u8; 3], &'static str)>> = OnceLock::new();
    T.get_or_init(|| {
        let mut text = String::new();
        if flate2::read::GzDecoder::new(OUI_GZ).read_to_string(&mut text).is_err() {
            return Vec::new();
        }
        let text: &'static str = Box::leak(text.into_boxed_str());
        let mut v: Vec<([u8; 3], &'static str)> = text
            .lines()
            .filter_map(|l| {
                let (k, name) = l.split_once('\t')?;
                let n = u32::from_str_radix(k, 16).ok()?;
                Some(([(n >> 16) as u8, (n >> 8) as u8, n as u8], name))
            })
            .collect();
        v.sort_by_key(|(k, _)| *k);
        v
    })
}

/// Number of registry entries (0 if the embedded table failed to load).
pub fn oui_entries() -> usize {
    table().len()
}

/// Organisation that registered the MAC's prefix, e.g. "Apple, Inc.".
pub fn vendor(mac: &str) -> Option<&'static str> {
    let b = bytes(mac)?;
    if b[0] & 2 != 0 {
        return None;
    }
    let t = table();
    let key = [b[0], b[1], b[2]];
    t.binary_search_by_key(&key, |(k, _)| *k).ok().map(|i| t[i].1)
}

/// "Apple, Inc." → "Apple"; "HUAWEI TECHNOLOGIES CO.,LTD" → "Huawei".
pub fn short_vendor(v: &str) -> String {
    let mut s = v.trim().to_string();
    for sep in [",", " co.", " co ", " corporation", " corp", " inc", " ltd", " llc", " gmbh", " technologies", " technology", " electronics", " international", " ind.", " semiconductor"] {
        if let Some(i) = s.to_lowercase().find(sep)
            && i > 2
        {
            s.truncate(i);
        }
    }
    let s = s.trim().trim_end_matches('.').trim().to_string();
    // SHOUTING registry names read better in title case
    if s.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase()) && s.len() > 4 {
        s.split(' ').map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_string() + &c.as_str().to_lowercase()).unwrap_or_default()
        }).collect::<Vec<_>>().join(" ")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("0:1a:2B:3:4:5").as_deref(), Some("00:1a:2b:03:04:05"));
        assert_eq!(normalize("00-1A-2B-03-04-05").as_deref(), Some("00:1a:2b:03:04:05"));
        assert_eq!(normalize("001a.2b03.0405").as_deref(), Some("00:1a:2b:03:04:05"));
        assert_eq!(normalize("001A2B030405").as_deref(), Some("00:1a:2b:03:04:05"));
        assert_eq!(normalize("(incomplete)"), None);
        assert_eq!(normalize("00:11:22:33:44"), None);
        assert!(is_device("00:11:22:33:44:55"));
        assert!(!is_device("ff:ff:ff:ff:ff:ff"));
        assert!(!is_device("01:00:5e:00:00:fb"));
        assert!(is_random("da:a1:19:00:00:01"));
        assert!(!is_random("00:11:22:33:44:55"));
    }

    #[test]
    fn vendor_lookup() {
        assert!(oui_entries() > 30_000, "embedded OUI table loads");
        assert_eq!(vendor("00:03:93:12:34:56"), Some("Apple, Inc."));
        assert_eq!(vendor("B8-27-EB-00-00-01"), Some("Raspberry Pi Foundation"));
        assert_eq!(vendor("00:00:0C:01:02:03"), Some("Cisco Systems, Inc"));
        // locally administered: no vendor
        assert_eq!(vendor("da:a1:19:00:00:01"), None);
        assert_eq!(vendor("garbage"), None);
        assert_eq!(short_vendor("Apple, Inc."), "Apple");
        assert_eq!(short_vendor("Hon Hai Precision Ind. Co.,Ltd."), "Hon Hai Precision");
        assert_eq!(short_vendor("Raspberry Pi Foundation"), "Raspberry Pi Foundation");
        assert_eq!(short_vendor("HUAWEI TECHNOLOGIES CO.,LTD"), "Huawei");
        assert_eq!(short_vendor("REALTEK SEMICONDUCTOR CORP."), "Realtek");
        assert_eq!(short_vendor("Samsung Electronics Co.,Ltd"), "Samsung");
        assert_eq!(short_vendor("TP-LINK TECHNOLOGIES CO.,LTD."), "Tp-link");
        assert_eq!(short_vendor("ASUSTek COMPUTER INC."), "ASUSTek COMPUTER");
    }
}
