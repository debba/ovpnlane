use std::{
    collections::VecDeque,
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    socket::{dns, tcp},
    time::Instant as SmolInstant,
    wire::{DnsQueryType, HardwareAddress, IpAddress, IpCidr},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf},
    sync::oneshot,
};

use crate::settings::Settings;

const BUFFER_SIZE: usize = 64 * 1024;
const MAX_PACKETS: usize = 1024;

pub trait PacketSink: Send + Sync + 'static {
    fn send(&self, packet: &[u8]) -> io::Result<()>;
}

#[derive(Clone, Debug)]
pub enum Target {
    Ip(IpAddr),
    Domain(String),
}

pub struct Connected {
    pub stream: DuplexStream,
    pub local: SocketAddr,
}

type Reply = oneshot::Sender<io::Result<Connected>>;

enum Command {
    Online {
        generation: u64,
        settings: Settings,
        sink: Arc<dyn PacketSink>,
    },
    Offline(u64),
    Connect {
        target: Target,
        port: u16,
        reply: Reply,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct Tunnel {
    commands: Sender<Command>,
    packets: Sender<(u64, Vec<u8>)>,
}

pub struct StackWorker {
    tunnel: Tunnel,
    worker: Option<thread::JoinHandle<()>>,
}

impl StackWorker {
    pub fn spawn(
        connect_timeout: Duration,
        idle_timeout: Duration,
        max_connections: usize,
    ) -> io::Result<Self> {
        let (commands, command_rx) = unbounded();
        let (packets, packet_rx) = bounded(MAX_PACKETS);
        let (wake_tx, wake_rx) = bounded(1);
        let tunnel = Tunnel { commands, packets };
        let worker = thread::Builder::new()
            .name("vpn-ip-stack".into())
            .spawn(move || {
                run(
                    command_rx,
                    packet_rx,
                    wake_tx,
                    wake_rx,
                    connect_timeout,
                    idle_timeout,
                    max_connections,
                );
            })?;
        Ok(Self {
            tunnel,
            worker: Some(worker),
        })
    }

    pub fn tunnel(&self) -> Tunnel {
        self.tunnel.clone()
    }
}

impl Drop for StackWorker {
    fn drop(&mut self) {
        let _ = self.tunnel.commands.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Tunnel {
    pub fn online(
        &self,
        generation: u64,
        settings: Settings,
        sink: Arc<dyn PacketSink>,
    ) -> io::Result<()> {
        self.commands
            .send(Command::Online {
                generation,
                settings,
                sink,
            })
            .map_err(|_| closed())
    }

    pub fn offline(&self, generation: u64) {
        let _ = self.commands.send(Command::Offline(generation));
    }

    pub fn receive(&self, generation: u64, packet: &[u8]) -> bool {
        packet.len() <= 65535 && self.packets.try_send((generation, packet.to_vec())).is_ok()
    }

    pub async fn connect(&self, target: Target, port: u16) -> io::Result<Connected> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::Connect {
                target,
                port,
                reply,
            })
            .map_err(|_| closed())?;
        response.await.map_err(|_| closed())?
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "VPN is not connected")
}

struct StackWake(Sender<()>);
impl Wake for StackWake {
    fn wake(self: Arc<Self>) {
        let _ = self.0.try_send(());
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.0.try_send(());
    }
}

fn run(
    commands: Receiver<Command>,
    packets: Receiver<(u64, Vec<u8>)>,
    wake_tx: Sender<()>,
    wake_rx: Receiver<()>,
    connect_timeout: Duration,
    idle_timeout: Duration,
    max_connections: usize,
) {
    let clock = Instant::now();
    let waker = Waker::from(Arc::new(StackWake(wake_tx)));
    let mut context = Context::from_waker(&waker);
    let mut engine: Option<(u64, Engine)> = None;
    loop {
        select! {
            recv(commands) -> command => match command {
                Ok(Command::Online { generation, settings, sink }) => {
                    // Dropping the old stack closes every stream and pending DNS query.
                    engine = Some((generation, Engine::new(settings, sink, connect_timeout, idle_timeout, max_connections, clock)));
                }
                Ok(Command::Offline(generation)) => {
                    if engine.as_ref().is_some_and(|(current, _)| *current == generation) { engine = None; }
                }
                Ok(Command::Connect { target, port, reply }) => {
                    if let Some((_, engine)) = engine.as_mut() { engine.connect(target, port, reply); }
                    else { let _ = reply.send(Err(closed())); }
                }
                Ok(Command::Shutdown) | Err(_) => break,
            },
            recv(packets) -> packet => {
                if let (Ok((generation, packet)), Some((current, engine))) = (packet, engine.as_mut())
                    && generation == *current {
                    engine.device.incoming.push_back(packet);
                }
            },
            recv(wake_rx) -> _ => {},
            default(Duration::from_millis(20)) => {},
        }
        if let Some((generation, engine)) = engine.as_mut() {
            for (epoch, packet) in packets.try_iter().take(256) {
                if epoch == *generation {
                    engine.device.incoming.push_back(packet);
                }
            }
            engine.step(&mut context);
        }
        if engine.as_ref().is_some_and(|(_, e)| e.device.failed) {
            engine = None;
        }
    }
}

pub struct PacketDevice {
    pub incoming: VecDeque<Vec<u8>>,
    sink: Arc<dyn PacketSink>,
    mtu: usize,
    failed: bool,
}

impl PacketDevice {
    pub fn new(mtu: usize, sink: Arc<dyn PacketSink>) -> Self {
        Self {
            incoming: VecDeque::new(),
            sink,
            mtu,
            failed: false,
        }
    }
}

pub struct PacketRx(Vec<u8>);
pub struct PacketTx<'a> {
    sink: &'a dyn PacketSink,
    failed: &'a mut bool,
}

impl Device for PacketDevice {
    type RxToken<'a> = PacketRx;
    type TxToken<'a> = PacketTx<'a>;
    fn receive(&mut self, _: SmolInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.incoming.pop_front().map(|packet| {
            (
                PacketRx(packet),
                PacketTx {
                    sink: &*self.sink,
                    failed: &mut self.failed,
                },
            )
        })
    }
    fn transmit(&mut self, _: SmolInstant) -> Option<Self::TxToken<'_>> {
        Some(PacketTx {
            sink: &*self.sink,
            failed: &mut self.failed,
        })
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = self.mtu;
        caps.max_burst_size = Some(64);
        caps
    }
}

impl RxToken for PacketRx {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

impl TxToken for PacketTx<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        if self.sink.send(&packet).is_err() {
            *self.failed = true;
        }
        result
    }
}

struct Flow {
    socket: SocketHandle,
    bridge: DuplexStream,
    client: Option<DuplexStream>,
    reply: Option<Reply>,
    local: SocketAddr,
    deadline: Instant,
    activity: Instant,
    read_closed: bool,
    write_closed: bool,
}

struct Lookup {
    query: dns::QueryHandle,
    name: String,
    fallback: Option<DnsQueryType>,
    port: u16,
    reply: Reply,
    deadline: Instant,
}

struct Engine {
    settings: Settings,
    device: PacketDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    dns: SocketHandle,
    flows: Vec<Flow>,
    lookups: Vec<Lookup>,
    next_port: u16,
    connect_timeout: Duration,
    idle_timeout: Duration,
    max_connections: usize,
    clock: Instant,
}

impl Engine {
    fn new(
        settings: Settings,
        sink: Arc<dyn PacketSink>,
        connect_timeout: Duration,
        idle_timeout: Duration,
        max_connections: usize,
        clock: Instant,
    ) -> Self {
        let mut device = PacketDevice::new(settings.mtu, sink);
        let mut config = Config::new(HardwareAddress::Ip);
        config.random_seed = rand::random();
        let mut iface = Interface::new(
            config,
            &mut device,
            SmolInstant::from_millis(clock.elapsed().as_millis() as i64),
        );
        iface.update_ip_addrs(|addresses| {
            if let Some(ip) = settings.ipv4 {
                addresses
                    .push(IpCidr::new(ip.into(), 32))
                    .expect("IPv4 slot");
            }
            if let Some(ip) = settings.ipv6 {
                addresses
                    .push(IpCidr::new(ip.into(), 128))
                    .expect("IPv6 slot");
            }
        });
        // Medium::Ip has no ARP/NDP. Every destination uses the packet tunnel.
        if let Some(ip) = settings.ipv4 {
            iface
                .routes_mut()
                .add_default_ipv4_route(ip)
                .expect("IPv4 route slot");
        }
        if let Some(ip) = settings.ipv6 {
            iface
                .routes_mut()
                .add_default_ipv6_route(ip)
                .expect("IPv6 route slot");
        }
        let mut sockets = SocketSet::new(vec![]);
        let servers: Vec<IpAddress> = settings.dns.iter().copied().map(Into::into).collect();
        let dns = sockets.add(dns::Socket::new(&servers, vec![]));
        Self {
            settings,
            device,
            iface,
            sockets,
            dns,
            flows: vec![],
            lookups: vec![],
            next_port: rand::random_range(49152..65535),
            connect_timeout,
            idle_timeout,
            max_connections,
            clock,
        }
    }

    fn connect(&mut self, target: Target, port: u16, reply: Reply) {
        if reply.is_closed() {
            return;
        }
        if self.flows.len() + self.lookups.len() >= self.max_connections {
            let _ = reply.send(Err(io::Error::other("connection limit reached")));
            return;
        }
        let deadline = Instant::now() + self.connect_timeout;
        match target {
            Target::Ip(ip) => self.connect_ip(ip, port, reply, deadline),
            Target::Domain(name) => {
                if self.settings.dns.is_empty() {
                    let _ = reply.send(Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "VPN supplied no DNS; use --dns or an IP address",
                    )));
                    return;
                }
                let query_type = if self.settings.ipv4.is_some() {
                    DnsQueryType::A
                } else {
                    DnsQueryType::Aaaa
                };
                let fallback = (self.settings.ipv4.is_some() && self.settings.ipv6.is_some())
                    .then_some(DnsQueryType::Aaaa);
                match self.sockets.get_mut::<dns::Socket>(self.dns).start_query(
                    self.iface.context(),
                    &name,
                    query_type,
                ) {
                    Ok(query) => self.lookups.push(Lookup {
                        query,
                        name,
                        fallback,
                        port,
                        reply,
                        deadline,
                    }),
                    Err(error) => {
                        let _ = reply.send(Err(io::Error::new(io::ErrorKind::InvalidInput, error)));
                    }
                }
            }
        }
    }

    fn connect_ip(&mut self, ip: IpAddr, port: u16, reply: Reply, deadline: Instant) {
        if !self.settings.supports_family(ip) {
            let _ = reply.send(Err(io::Error::new(
                io::ErrorKind::NetworkUnreachable,
                "address family not assigned by VPN",
            )));
            return;
        }
        if port == 0 || ip.is_unspecified() || ip.is_multicast() {
            let _ = reply.send(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid destination",
            )));
            return;
        }
        let local_port = loop {
            let port = self.next_port;
            self.next_port = if port == 65535 { 49152 } else { port + 1 };
            if !self.flows.iter().any(|f| f.local.port() == port) {
                break port;
            }
        };
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; BUFFER_SIZE]),
            tcp::SocketBuffer::new(vec![0; BUFFER_SIZE]),
        );
        socket.set_nagle_enabled(false);
        socket.set_keep_alive(Some(smoltcp::time::Duration::from_secs(30)));
        if let Err(error) = socket.connect(
            self.iface.context(),
            (IpAddress::from(ip), port),
            local_port,
        ) {
            let _ = reply.send(Err(io::Error::other(error)));
            return;
        }
        let local_ip = match ip {
            IpAddr::V4(_) => IpAddr::V4(self.settings.ipv4.unwrap()),
            IpAddr::V6(_) => IpAddr::V6(self.settings.ipv6.unwrap()),
        };
        let handle = self.sockets.add(socket);
        let (bridge, client) = tokio::io::duplex(BUFFER_SIZE);
        self.flows.push(Flow {
            socket: handle,
            bridge,
            client: Some(client),
            reply: Some(reply),
            local: SocketAddr::new(local_ip, local_port),
            deadline,
            activity: Instant::now(),
            read_closed: false,
            write_closed: false,
        });
    }

    fn step(&mut self, context: &mut Context<'_>) {
        let timestamp = SmolInstant::from_millis(self.clock.elapsed().as_millis() as i64);
        self.iface
            .poll(timestamp, &mut self.device, &mut self.sockets);
        let now = Instant::now();
        let mut i = 0;
        while i < self.lookups.len() {
            let lookup = &self.lookups[i];
            if lookup.reply.is_closed() || now >= lookup.deadline {
                let lookup = self.lookups.swap_remove(i);
                self.sockets
                    .get_mut::<dns::Socket>(self.dns)
                    .cancel_query(lookup.query);
                let _ = lookup.reply.send(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "VPN DNS timeout",
                )));
                continue;
            }
            match self
                .sockets
                .get_mut::<dns::Socket>(self.dns)
                .get_query_result(lookup.query)
            {
                Err(dns::GetQueryResultError::Pending) => {
                    i += 1;
                }
                result => {
                    let mut lookup = self.lookups.swap_remove(i);
                    let address = result.ok().and_then(|ips| ips.first().copied());
                    if let Some(ip) = address {
                        self.connect_ip(ip.into(), lookup.port, lookup.reply, lookup.deadline);
                    } else if let Some(kind) = lookup.fallback.take() {
                        match self.sockets.get_mut::<dns::Socket>(self.dns).start_query(
                            self.iface.context(),
                            &lookup.name,
                            kind,
                        ) {
                            Ok(query) => {
                                lookup.query = query;
                                self.lookups.push(lookup);
                            }
                            Err(_) => {
                                let _ = lookup.reply.send(Err(io::Error::new(
                                    io::ErrorKind::NotFound,
                                    "VPN DNS query failed",
                                )));
                            }
                        }
                    } else {
                        let _ = lookup.reply.send(Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            "VPN DNS query failed",
                        )));
                    }
                }
            }
        }
        let mut i = 0;
        while i < self.flows.len() {
            let flow = &mut self.flows[i];
            let socket = self.sockets.get_mut::<tcp::Socket>(flow.socket);
            if !flow.poll(socket, context, now, self.idle_timeout) {
                let flow = self.flows.swap_remove(i);
                self.sockets.remove(flow.socket);
            } else {
                i += 1;
            }
        }
        self.iface
            .poll(timestamp, &mut self.device, &mut self.sockets);
    }
}

impl Flow {
    fn poll(
        &mut self,
        socket: &mut tcp::Socket<'_>,
        context: &mut Context<'_>,
        now: Instant,
        idle_timeout: Duration,
    ) -> bool {
        if let Some(reply) = &self.reply {
            if reply.is_closed() {
                socket.abort();
                return false;
            }
            let failure = if now >= self.deadline {
                Some(io::ErrorKind::TimedOut)
            } else if socket.state() == tcp::State::Closed {
                Some(io::ErrorKind::ConnectionRefused)
            } else {
                None
            };
            if let Some(kind) = failure {
                let _ = self
                    .reply
                    .take()
                    .unwrap()
                    .send(Err(io::Error::new(kind, "VPN TCP connection failed")));
                socket.abort();
                return false;
            }
            if socket.state() == tcp::State::Established {
                let connection = Connected {
                    stream: self.client.take().unwrap(),
                    local: self.local,
                };
                if self.reply.take().unwrap().send(Ok(connection)).is_err() {
                    socket.abort();
                    return false;
                }
                self.activity = now;
            } else {
                return true;
            }
        }
        if !idle_timeout.is_zero() && now.duration_since(self.activity) >= idle_timeout {
            socket.abort();
            return false;
        }
        for _ in 0..64 {
            if !socket.can_recv() || self.write_closed {
                break;
            }
            let result =
                socket.recv(
                    |data| match Pin::new(&mut self.bridge).poll_write(context, data) {
                        Poll::Ready(Ok(count)) if count > 0 => (count, Ok(count)),
                        Poll::Ready(Ok(_)) => (0, Err(io::Error::from(io::ErrorKind::WriteZero))),
                        Poll::Ready(Err(error)) => (0, Err(error)),
                        Poll::Pending => (0, Ok(0)),
                    },
                );
            match result {
                Ok(Ok(count)) if count > 0 => self.activity = now,
                Ok(Ok(_)) => break,
                _ => {
                    socket.abort();
                    return false;
                }
            }
        }
        if !socket.may_recv() && !self.write_closed {
            match Pin::new(&mut self.bridge).poll_shutdown(context) {
                Poll::Ready(Ok(())) => self.write_closed = true,
                Poll::Ready(Err(_)) => {
                    socket.abort();
                    return false;
                }
                Poll::Pending => {}
            }
        }
        for _ in 0..64 {
            if !socket.can_send() || self.read_closed {
                break;
            }
            let result = socket.send(|data| {
                let mut buffer = ReadBuf::new(data);
                match Pin::new(&mut self.bridge).poll_read(context, &mut buffer) {
                    Poll::Ready(Ok(())) => {
                        let count = buffer.filled().len();
                        (count, Ok(Some(count)))
                    }
                    Poll::Ready(Err(error)) => (0, Err(error)),
                    Poll::Pending => (0, Ok(None)),
                }
            });
            match result {
                Ok(Ok(Some(0))) => {
                    self.read_closed = true;
                    socket.close();
                    break;
                }
                Ok(Ok(Some(_))) => self.activity = now,
                Ok(Ok(None)) => break,
                _ => {
                    socket.abort();
                    return false;
                }
            }
        }
        socket.state() != tcp::State::Closed
    }
}
