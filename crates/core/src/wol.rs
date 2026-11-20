//! Wake-on-LAN: the magic packet (6 × 0xFF, then the MAC 16 times) as a UDP broadcast.
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

pub fn magic_packet(mac: &str) -> Option<[u8; 102]> {
    let m = crate::mac::bytes(mac)?;
    let mut p = [0xFFu8; 102];
    for i in 0..16 {
        p[6 + i * 6..12 + i * 6].copy_from_slice(&m);
    }
    Some(p)
}

/// Send it to `broadcast` (the subnet's, else 255.255.255.255) on ports 9 and 7.
pub fn wake(mac: &str, broadcast: Option<Ipv4Addr>) -> std::io::Result<()> {
    let p = magic_packet(mac).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "mac"))?;
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    s.set_broadcast(true)?;
    let to = broadcast.unwrap_or(Ipv4Addr::BROADCAST);
    for port in [9, 7] {
        s.send_to(&p, SocketAddr::from((to, port)))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn packet() {
        let p = super::magic_packet("00:11:22:33:44:55").unwrap();
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert_eq!(&p[6..12], &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        assert_eq!(&p[96..102], &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        assert!(super::magic_packet("nope").is_none());
    }
}
