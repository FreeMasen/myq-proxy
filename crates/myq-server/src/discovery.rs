use std::net::Ipv4Addr;

use serde::{Serialize, Deserialize};
use tokio::net::UdpSocket;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryMessage {
    target: String,
    source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryReply {
    ip: Ipv4Addr,
    port: u16,
}

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const MULTICAST_PORT: u16 = 1919;

pub async fn discovery_task(ip: Ipv4Addr, port: u16) {
    let udp = get_sock().await;
    loop {
        let mut buf = vec![0u8;65535];
        let (ct, who) = udp.recv_from(&mut buf).await.unwrap();
        log::debug!("req({ct}, {who}): {:?}", String::from_utf8_lossy(&buf[..ct]));
        if let Ok(msg) = serde_json::from_slice::<QueryMessage>(&buf[..ct]).map_err(|e| log::debug!("bad query: {e}")) {
            if msg.target == "myq-proxy" {
                log::info!("handling disovery request from {}", msg.source);
                udp.send_to(&serde_json::to_vec(&QueryReply {
                    ip, port
                }).unwrap(), who).await.unwrap();
            }
        }
    }
}

async fn get_sock() -> UdpSocket {
    use std::net::{SocketAddr, SocketAddrV4};
    use socket2::{Socket, Domain, Type, SockAddr};
    let udp = Socket::new(Domain::IPV4, Type::DGRAM, None).unwrap();
    udp.set_reuse_port(true).unwrap();
    udp.set_reuse_address(true).unwrap();
    udp.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::new(0, 0, 0, 0)).unwrap();
    udp.set_multicast_loop_v4(false).unwrap();
    let sock_addr = SocketAddrV4::new(MULTICAST_ADDR, MULTICAST_PORT);
    let sock_addr = SocketAddr::V4(sock_addr);
    let sock_addr = SockAddr::from(sock_addr);
    udp.bind(&sock_addr).unwrap();
    udp.set_nonblocking(true).unwrap();
    UdpSocket::from_std(udp.into()).unwrap()
}

#[cfg(test)]
mod test {
    use super::*;
    
    #[tokio::test]
    async fn replies() {
        let target_addr = Ipv4Addr::new(192,168,1,200);
        let target_port = 9999u16;
        tokio::task::spawn(discovery_task(target_addr, target_port));
        tokio::task::yield_now().await;
        let addr = Ipv4Addr::new(0,0,0,0);
        let udp = UdpSocket::bind((addr, 0)).await.unwrap();
        udp.join_multicast_v4(MULTICAST_ADDR, addr).unwrap();
        let query = QueryMessage {
            source: "test-source".to_string(),
            target: "myq-proxy".to_string(),
        };
        let query_bytes = serde_json::to_vec(&query).unwrap();
        let send_ct = udp.send_to(&query_bytes, (MULTICAST_ADDR, MULTICAST_PORT)).await.unwrap();
        assert_eq!(send_ct, query_bytes.len());
        let mut buf = vec![0u8;4096];
        let (ct, _who) = udp.recv_from(&mut buf).await.unwrap();
        let resp: QueryReply = serde_json::from_slice(&buf[..ct]).unwrap();
        assert_eq!(target_addr, resp.ip);
        assert_eq!(target_port, resp.port);
    }
}
