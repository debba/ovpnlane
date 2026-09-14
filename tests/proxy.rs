use crossbeam_channel::{Sender, bounded};
use ovpnlane::{
    netstack::{PacketDevice, PacketSink, StackWorker, Target, Tunnel},
    settings::Settings,
    socks,
};
use smoltcp::{
    iface::{Config, Interface, SocketSet},
    socket::{tcp, udp},
    time::Instant as SmolInstant,
    wire::{HardwareAddress, IpAddress, IpCidr},
};
use std::{
    io,
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct QueueSink(Sender<Vec<u8>>);
impl PacketSink for QueueSink {
    fn send(&self, packet: &[u8]) -> io::Result<()> {
        self.0.send(packet.to_vec()).map_err(io::Error::other)
    }
}
struct ReturnSink(Tunnel);
impl PacketSink for ReturnSink {
    fn send(&self, packet: &[u8]) -> io::Result<()> {
        if self.0.receive(1, packet) {
            Ok(())
        } else {
            Err(io::Error::other("packet queue full"))
        }
    }
}

/// A second real TCP/IP stack is the remote network. No kernel TUN, VPN credentials,
/// privileged operations, or external network connections are used by these tests.
struct Peer {
    worker: StackWorker,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    queries: Arc<AtomicUsize>,
}
impl Peer {
    fn start() -> Self {
        let worker = StackWorker::spawn(Duration::from_secs(2), Duration::from_secs(5), 8).unwrap();
        let tunnel = worker.tunnel();
        let (packets, incoming) = bounded(2048);
        tunnel.online(1, Settings::parse("ifconfig 10.8.0.2 10.8.0.1\nifconfig-ipv6 fd00::2/64 fd00::1\ndhcp-option DNS 10.8.0.1", 1500, &[]).unwrap(), Arc::new(QueueSink(packets))).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let queries = Arc::new(AtomicUsize::new(0));
        let query_count = queries.clone();
        let thread = thread::spawn(move || {
            let clock = Instant::now();
            let mut device = PacketDevice::new(1500, Arc::new(ReturnSink(tunnel)));
            let mut config = Config::new(HardwareAddress::Ip);
            config.random_seed = 42;
            let mut iface = Interface::new(config, &mut device, SmolInstant::ZERO);
            iface.update_ip_addrs(|ips| {
                ips.push(IpCidr::new(IpAddress::v4(10, 8, 0, 1), 24))
                    .unwrap();
                ips.push("fd00::1/64".parse().unwrap()).unwrap();
            });
            let mut sockets = SocketSet::new(vec![]);
            let mut socket = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 65536]),
                tcp::SocketBuffer::new(vec![0; 65536]),
            );
            socket.listen(22).unwrap();
            let tcp_handle = sockets.add(socket);
            let mut socket = udp::Socket::new(
                udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 8192]),
                udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 8192]),
            );
            socket.bind(53).unwrap();
            let dns_handle = sockets.add(socket);
            while !stopping.load(Ordering::Relaxed) {
                if let Ok(packet) = incoming.recv_timeout(Duration::from_millis(1)) {
                    device.incoming.push_back(packet);
                }
                for packet in incoming.try_iter().take(256) {
                    device.incoming.push_back(packet);
                }
                let now = SmolInstant::from_millis(clock.elapsed().as_millis() as i64);
                iface.poll(now, &mut device, &mut sockets);
                let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
                if socket.state() == tcp::State::Closed {
                    socket.listen(22).unwrap();
                }
                if socket.can_recv() && socket.can_send() {
                    let capacity = socket.send_capacity() - socket.send_queue();
                    let bytes = socket
                        .recv(|data| {
                            let n = data.len().min(capacity);
                            (n, data[..n].to_vec())
                        })
                        .unwrap();
                    socket.send_slice(&bytes).unwrap();
                }
                if !socket.may_recv() && socket.state() == tcp::State::CloseWait {
                    socket.close();
                }
                let socket = sockets.get_mut::<udp::Socket>(dns_handle);
                if socket.can_recv() && socket.can_send() {
                    let (request, meta) = socket.recv().unwrap();
                    let request = request.to_vec();
                    query_count.fetch_add(1, Ordering::Relaxed);
                    // Preserve the transaction/question and add an authoritative A answer.
                    let mut answer = request.clone();
                    answer[2..4].copy_from_slice(&[0x81, 0x80]);
                    answer[6..8].copy_from_slice(&[0, 1]);
                    answer.extend_from_slice(&[
                        0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 10, 8, 0, 1,
                    ]);
                    socket.send_slice(&answer, meta.endpoint).unwrap();
                }
                iface.poll(now, &mut device, &mut sockets);
            }
        });
        Self {
            worker,
            stop,
            thread: Some(thread),
            queries,
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

async fn socks_connection(peer: &Peer, target: &[u8]) -> tokio::io::DuplexStream {
    let (mut client, server) = tokio::io::duplex(65536);
    tokio::spawn(socks::handle(server, peer.worker.tunnel()));
    // Fragment the greeting/request into single bytes to exercise framing.
    for byte in [5, 1, 0, 5, 1, 0].iter().chain(target).chain(&[0, 22]) {
        client.write_all(&[*byte]).await.unwrap();
    }
    let mut greeting = [0; 2];
    client.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting, [5, 0]);
    let mut head = [0; 4];
    client.read_exact(&mut head).await.unwrap();
    assert_eq!(head[1], 0, "SOCKS connect failed: {head:?}");
    let mut rest = vec![0; if head[3] == 4 { 18 } else { 6 }];
    client.read_exact(&mut rest).await.unwrap();
    client
}

#[tokio::test]
async fn socks_ipv4_large_transfer_and_half_close() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let peer = Peer::start();
        let stream = socks_connection(&peer, &[1, 10, 8, 0, 1]).await;
        let (mut reader, mut writer) = tokio::io::split(stream);
        let payload: Vec<u8> = (0..524288).map(|i| (i % 251) as u8).collect();
        let expected = payload.clone();
        let writing = tokio::spawn(async move {
            writer.write_all(&payload).await.unwrap();
            writer.shutdown().await.unwrap();
        });
        let mut received = vec![];
        reader.read_to_end(&mut received).await.unwrap();
        writing.await.unwrap();
        assert_eq!(received, expected);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn socks_ipv6() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let peer = Peer::start();
        let mut target = vec![4];
        target.extend("fd00::1".parse::<std::net::Ipv6Addr>().unwrap().octets());
        let mut stream = socks_connection(&peer, &target).await;
        stream.write_all(b"SSH-2.0-test\r\n").await.unwrap();
        let mut bytes = [0; 14];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"SSH-2.0-test\r\n");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn hostname_is_resolved_inside_the_packet_tunnel() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let peer = Peer::start();
        let mut target = vec![3, 12];
        target.extend(b"host.invalid");
        let mut stream = socks_connection(&peer, &target).await;
        stream.write_all(b"dns").await.unwrap();
        let mut data = [0; 3];
        stream.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"dns");
        assert_eq!(peer.queries.load(Ordering::Relaxed), 1);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn remote_refusal_is_reported() {
    let peer = Peer::start();
    let result = peer
        .worker
        .tunnel()
        .connect(Target::Ip(IpAddr::V4(Ipv4Addr::new(10, 8, 0, 1))), 23)
        .await;
    assert_eq!(
        result.err().unwrap().kind(),
        io::ErrorKind::ConnectionRefused
    );
}

#[tokio::test]
async fn vpn_loss_closes_existing_streams_and_rejects_new_connections() {
    let peer = Peer::start();
    let tunnel = peer.worker.tunnel();
    let mut connection = tunnel
        .connect(Target::Ip("10.8.0.1".parse().unwrap()), 22)
        .await
        .unwrap();
    tunnel.offline(1);
    let mut byte = [0; 1];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), connection.stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let result = tunnel
        .connect(Target::Ip("10.8.0.1".parse().unwrap()), 22)
        .await;
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotConnected);
}

#[tokio::test]
async fn missing_vpn_dns_does_not_fall_back_to_the_system() {
    let worker = StackWorker::spawn(Duration::from_secs(1), Duration::ZERO, 4).unwrap();
    let (tx, _rx) = bounded(16);
    let tunnel = worker.tunnel();
    tunnel
        .online(
            1,
            Settings::parse("ifconfig 10.8.0.2 10.8.0.1", 1500, &[]).unwrap(),
            Arc::new(QueueSink(tx)),
        )
        .unwrap();
    let result = tunnel.connect(Target::Domain("localhost".into()), 22).await;
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotFound);
}
