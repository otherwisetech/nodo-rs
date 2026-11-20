//! Reachability: the system `ping` (ICMP without raw-socket privileges), else a TCP connect to
//! a port the device is known to answer on (a refused connection still proves it is there).
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// Result of one TCP connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tcp {
    Open,
    /// RST: the host is up, the port closed.
    Refused,
    /// Nothing came back.
    Silent,
}

/// What the scanner and the monitor need from the network; faked in tests.
pub trait Prober: Send + Sync {
    /// Round-trip time in ms of one echo, if it came back.
    fn ping(&self, ip: IpAddr, timeout: Duration) -> Option<f64>;
    fn tcp(&self, ip: IpAddr, port: u16, timeout: Duration) -> (Tcp, f64);

    /// Ping, then the given ports: `Some(latency)` when anything answered.
    fn reach(&self, ip: IpAddr, ports: &[u16], timeout: Duration) -> Option<f64> {
        if let Some(ms) = self.ping(ip, timeout) {
            return Some(ms);
        }
        ports.iter().find_map(|&p| match self.tcp(ip, p, timeout) {
            (Tcp::Open | Tcp::Refused, ms) => Some(ms),
            (Tcp::Silent, _) => None,
        })
    }
}

pub struct SystemProber;

impl Prober for SystemProber {
    fn ping(&self, ip: IpAddr, timeout: Duration) -> Option<f64> {
        let ms = timeout.as_millis().max(200) as u64;
        let secs = ms.div_ceil(1000).to_string();
        let ms_s = ms.to_string();
        let ip_s = ip.to_string();
        let args: Vec<&str> = if cfg!(windows) {
            vec!["-n", "1", "-w", &ms_s, &ip_s]
        } else if cfg!(target_os = "macos") {
            // macOS: -W is milliseconds; IPv6 needs ping6 on older releases, plain ping works on 12+
            vec!["-c", "1", "-W", &ms_s, &ip_s]
        } else {
            vec!["-c", "1", "-W", &secs, &ip_s]
        };
        let out = crate::cmd::run_named("ping", &args, timeout + Duration::from_secs(2)).ok()?;
        if !out.ok {
            return None;
        }
        parse_ping_time(&out.stdout)
    }

    fn tcp(&self, ip: IpAddr, port: u16, timeout: Duration) -> (Tcp, f64) {
        let start = Instant::now();
        let r = TcpStream::connect_timeout(&SocketAddr::new(ip, port), timeout);
        let ms = start.elapsed().as_secs_f64() * 1000.;
        match r {
            Ok(_) => (Tcp::Open, ms),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => (Tcp::Refused, ms),
            Err(_) => (Tcp::Silent, ms),
        }
    }
}

/// `time=1.234 ms` (Unix), `time=3ms` / `time<1ms` / `Zeit=3ms` (Windows, localised) → ms.
pub fn parse_ping_time(s: &str) -> Option<f64> {
    // reply lines carry a TTL in every locale; skip the counters before the time
    for line in s.lines().filter(|l| l.to_lowercase().contains("ttl") || l.contains("hlim")) {
        let toks: Vec<&str> = line.split_whitespace().collect();
        for (i, tok) in toks.iter().enumerate() {
            let low = tok.to_lowercase();
            if low.starts_with("ttl") || low.starts_with("bytes") || low.starts_with("icmp_seq") || low.starts_with("hlim") {
                continue;
            }
            let Some(pos) = tok.find(['=', '<']) else { continue };
            let v = &tok[pos + 1..];
            let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',').collect();
            if num.is_empty() {
                continue;
            }
            let unit = &v[num.len()..];
            let ms = unit.eq_ignore_ascii_case("ms") || unit.is_empty() && toks.get(i + 1).is_some_and(|n| n.eq_ignore_ascii_case("ms"));
            if ms {
                let n: f64 = num.replace(',', ".").parse().ok()?;
                return Some(if tok.as_bytes()[pos] == b'<' { n / 2. } else { n });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_output() {
        let mac = "PING 192.168.1.1 (192.168.1.1): 56 data bytes\n64 bytes from 192.168.1.1: icmp_seq=0 ttl=64 time=3.512 ms\n\n--- 192.168.1.1 ping statistics ---\n1 packets transmitted, 1 packets received, 0.0% packet loss\n";
        assert_eq!(parse_ping_time(mac), Some(3.512));
        let linux = "PING 10.0.0.1 (10.0.0.1) 56(84) bytes of data.\n64 bytes from 10.0.0.1: icmp_seq=1 ttl=63 time=0.412 ms\n";
        assert_eq!(parse_ping_time(linux), Some(0.412));
        let win = "Pinging 192.168.1.1 with 32 bytes of data:\r\nReply from 192.168.1.1: bytes=32 time=4ms TTL=64\r\n";
        assert_eq!(parse_ping_time(win), Some(4.0));
        let win_fast = "Reply from 192.168.1.1: bytes=32 time<1ms TTL=64\r\n";
        assert_eq!(parse_ping_time(win_fast), Some(0.5));
        let de = "Antwort von 192.168.1.1: Bytes=32 Zeit=7ms TTL=64\r\n";
        assert_eq!(parse_ping_time(de), Some(7.0));
        assert_eq!(parse_ping_time("Request timeout for icmp_seq 0\n"), None);
    }

    struct Fake;
    impl Prober for Fake {
        fn ping(&self, _: IpAddr, _: Duration) -> Option<f64> {
            None
        }
        fn tcp(&self, _: IpAddr, port: u16, _: Duration) -> (Tcp, f64) {
            if port == 22 { (Tcp::Refused, 2.0) } else { (Tcp::Silent, 0.) }
        }
    }

    #[test]
    fn reach_falls_back_to_tcp() {
        let ip: IpAddr = "10.0.0.2".parse().unwrap();
        assert_eq!(Fake.reach(ip, &[80, 22], Duration::from_millis(10)), Some(2.0));
        assert_eq!(Fake.reach(ip, &[80], Duration::from_millis(10)), None);
    }
}
