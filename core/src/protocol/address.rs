use std::fmt;

use crate::{PhantomError, Result};
use bytes::{BufMut, Bytes, BytesMut};
use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub enum TargetAddr {
    IPv4([u8; 4], u16),
    IPv6([u8; 16], u16),
    Domain(String, u16),
}

impl fmt::Display for TargetAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetAddr::IPv4(ip, port) => {
                write!(f, "{}.{}.{}.{}:{}", ip[0], ip[1], ip[2], ip[3], port)
            }
            TargetAddr::IPv6(ip, port) => write!(
                f,
                "[{:#x}:{:#x}:{:#x}:{:#x}:{:#x}:{:#x}:{:#x}:{:#x}]:{}",
                u16::from_be_bytes([ip[0], ip[1]]),
                u16::from_be_bytes([ip[2], ip[3]]),
                u16::from_be_bytes([ip[4], ip[5]]),
                u16::from_be_bytes([ip[6], ip[7]]),
                u16::from_be_bytes([ip[8], ip[9]]),
                u16::from_be_bytes([ip[10], ip[11]]),
                u16::from_be_bytes([ip[12], ip[13]]),
                u16::from_be_bytes([ip[14], ip[15]]),
                port
            ),
            TargetAddr::Domain(domain, port) => write!(f, "{}:{}", domain, port),
        }
    }
}

impl TargetAddr {
    /// Encode address in SOCKS5-compatible format:
    /// - IPv4: [0x01][4 bytes addr][2 bytes port BE]
    /// - Domain: [0x03][1 byte len][domain bytes][2 bytes port BE]
    /// - IPv6: [0x04][16 bytes addr][2 bytes port BE]
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::new();
        match self {
            TargetAddr::IPv4(ip, port) => {
                buf.put_u8(0x01);
                buf.put_slice(ip);
                buf.put_u16(*port);
            }
            TargetAddr::Domain(domain, port) => {
                buf.put_u8(0x03);
                buf.put_u8(domain.len() as u8);
                buf.put_slice(domain.as_bytes());
                buf.put_u16(*port);
            }
            TargetAddr::IPv6(ip, port) => {
                buf.put_u8(0x04);
                buf.put_slice(ip);
                buf.put_u16(*port);
            }
        }
        buf.freeze()
    }

    pub fn decode(src: &[u8]) -> Result<Self> {
        if src.is_empty() {
            return Err(PhantomError::Protocol("Empty address".to_string()));
        }

        let atyp = src[0];
        match atyp {
            0x01 => {
                if src.len() < 1 + 4 + 2 {
                    return Err(PhantomError::Protocol("IPv4 address too short".to_string()));
                }
                let mut ip = [0u8; 4];
                ip.copy_from_slice(&src[1..5]);
                let port = u16::from_be_bytes([src[5], src[6]]);
                Ok(TargetAddr::IPv4(ip, port))
            }
            0x03 => {
                if src.len() < 2 {
                    return Err(PhantomError::Protocol(
                        "Domain address too short".to_string(),
                    ));
                }
                let domain_len = src[1] as usize;
                if src.len() < 2 + domain_len + 2 {
                    return Err(PhantomError::Protocol(
                        "Domain address truncated".to_string(),
                    ));
                }
                let domain = String::from_utf8(src[2..2 + domain_len].to_vec())
                    .map_err(|e| PhantomError::Protocol(format!("Invalid domain: {}", e)))?;
                let port = u16::from_be_bytes([src[2 + domain_len], src[2 + domain_len + 1]]);
                Ok(TargetAddr::Domain(domain, port))
            }
            0x04 => {
                if src.len() < 1 + 16 + 2 {
                    return Err(PhantomError::Protocol("IPv6 address too short".to_string()));
                }
                let mut ip = [0u8; 16];
                ip.copy_from_slice(&src[1..17]);
                let port = u16::from_be_bytes([src[17], src[18]]);
                Ok(TargetAddr::IPv6(ip, port))
            }
            _ => Err(PhantomError::Protocol(format!(
                "Unknown address type: {}",
                atyp
            ))),
        }
    }

    pub async fn to_socket_addr(&self) -> Result<SocketAddr> {
        match self {
            TargetAddr::IPv4(ip, port) => {
                Ok(SocketAddr::new(std::net::IpAddr::V4((*ip).into()), *port))
            }
            TargetAddr::IPv6(ip, port) => {
                Ok(SocketAddr::new(std::net::IpAddr::V6((*ip).into()), *port))
            }
            TargetAddr::Domain(domain, port) => {
                // Use tokio's async DNS resolution
                let addr = format!("{}:{}", domain, port);
                tokio::net::TcpStream::connect(&addr)
                    .await
                    .map_err(|_e| PhantomError::ServerUnreachable {
                        name: domain.clone(),
                    })?
                    .peer_addr()
                    .map_err(PhantomError::Io)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_roundtrip() {
        let addr = TargetAddr::IPv4([127, 0, 0, 1], 8080);
        let encoded = addr.encode();
        let decoded = TargetAddr::decode(&encoded).unwrap();
        match decoded {
            TargetAddr::IPv4(ip, port) => {
                assert_eq!(ip, [127, 0, 0, 1]);
                assert_eq!(port, 8080);
            }
            _ => panic!("Expected IPv4"),
        }
    }

    #[test]
    fn domain_roundtrip() {
        let addr = TargetAddr::Domain("example.com".to_string(), 443);
        let encoded = addr.encode();
        let decoded = TargetAddr::decode(&encoded).unwrap();
        match decoded {
            TargetAddr::Domain(d, port) => {
                assert_eq!(d, "example.com");
                assert_eq!(port, 443);
            }
            _ => panic!("Expected Domain"),
        }
    }
}
