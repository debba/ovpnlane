use crate::netstack::{Target, Tunnel};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{Semaphore, watch},
    task::JoinSet,
    time::timeout,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn serve(
    listener: TcpListener,
    tunnel: Tunnel,
    limit: usize,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let semaphore = Arc::new(Semaphore::new(limit));
    let mut tasks = JoinSet::new();
    let result = loop {
        if *shutdown.borrow() {
            break Ok(());
        }
        tokio::select! {
            _ = shutdown.changed() => break Ok(()),
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result { tracing::warn!(%error, "SOCKS task failed"); }
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted { Ok(pair) => pair, Err(error) => break Err(error) };
                let Ok(permit) = semaphore.clone().try_acquire_owned() else { continue; };
                let tunnel = tunnel.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = handle(stream, tunnel).await { tracing::debug!(%error, "SOCKS connection ended"); }
                });
            }
        }
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result
}

pub async fn handle<S>(mut stream: S, tunnel: Tunnel) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (target, port) = timeout(HANDSHAKE_TIMEOUT, handshake(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SOCKS handshake timeout"))??;
    let mut connection = match tunnel.connect(target, port).await {
        Ok(connection) => connection,
        Err(error) => {
            let code = match error.kind() {
                io::ErrorKind::NetworkUnreachable | io::ErrorKind::NotConnected => 3,
                io::ErrorKind::NotFound
                | io::ErrorKind::HostUnreachable
                | io::ErrorKind::TimedOut => 4,
                io::ErrorKind::ConnectionRefused => 5,
                _ => 1,
            };
            reply(&mut stream, code, None).await?;
            return Err(error);
        }
    };
    reply(&mut stream, 0, Some(connection.local)).await?;
    tokio::io::copy_bidirectional(&mut stream, &mut connection.stream).await?;
    Ok(())
}

async fn handshake<S>(stream: &mut S) -> io::Result<(Target, u16)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let version = stream.read_u8().await?;
    let count = stream.read_u8().await?;
    if version != 5 || count == 0 {
        return Err(invalid("invalid SOCKS greeting"));
    }
    let mut methods = vec![0; usize::from(count)];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        stream.write_all(&[5, 255]).await?;
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SOCKS requires no-auth method",
        ));
    }
    stream.write_all(&[5, 0]).await?;
    let mut request = [0; 4];
    stream.read_exact(&mut request).await?;
    if request[0] != 5 || request[2] != 0 {
        reply(stream, 1, None).await?;
        return Err(invalid("invalid SOCKS request"));
    }
    if request[1] != 1 {
        reply(stream, 7, None).await?;
        return Err(invalid("only SOCKS CONNECT is supported"));
    }
    let target = match request[3] {
        1 => {
            let mut octets = [0; 4];
            stream.read_exact(&mut octets).await?;
            Target::Ip(IpAddr::V4(Ipv4Addr::from(octets)))
        }
        4 => {
            let mut octets = [0; 16];
            stream.read_exact(&mut octets).await?;
            Target::Ip(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        3 => {
            let length = stream.read_u8().await?;
            let mut name = vec![0; usize::from(length)];
            stream.read_exact(&mut name).await?;
            if name.is_empty()
                || !name
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(b))
            {
                reply(stream, 4, None).await?;
                return Err(invalid("invalid SOCKS hostname"));
            }
            Target::Domain(String::from_utf8(name).map_err(|_| invalid("invalid hostname"))?)
        }
        _ => {
            reply(stream, 8, None).await?;
            return Err(invalid("unsupported address type"));
        }
    };
    let port = stream.read_u16().await?;
    if port == 0 {
        reply(stream, 1, None).await?;
        return Err(invalid("destination port is zero"));
    }
    Ok((target, port))
}

async fn reply<S: AsyncWrite + Unpin>(
    stream: &mut S,
    code: u8,
    bound: Option<SocketAddr>,
) -> io::Result<()> {
    let bound = bound.unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0)));
    let mut response = vec![5, code, 0];
    match bound.ip() {
        IpAddr::V4(ip) => {
            response.push(1);
            response.extend(ip.octets());
        }
        IpAddr::V6(ip) => {
            response.push(4);
            response.extend(ip.octets());
        }
    }
    response.extend(bound.port().to_be_bytes());
    stream.write_all(&response).await
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netstack::StackWorker;
    #[tokio::test]
    async fn rejects_auth_without_reading_a_request() {
        let (mut client, mut server) = tokio::io::duplex(64);
        let task = tokio::spawn(async move { handshake(&mut server).await });
        client.write_all(&[5, 1, 2]).await.unwrap();
        let mut reply = [0; 2];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [5, 255]);
        assert!(task.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn rejects_udp_associate() {
        let (mut client, mut server) = tokio::io::duplex(64);
        let task = tokio::spawn(async move { handshake(&mut server).await });
        client.write_all(&[5, 1, 0, 5, 3, 0, 1]).await.unwrap();
        let mut response = [0; 12];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(response[3], 7);
        assert!(task.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn disconnected_vpn_fails_closed() {
        let worker = StackWorker::spawn(Duration::from_secs(1), Duration::ZERO, 4).unwrap();
        let (mut client, server) = tokio::io::duplex(64);
        let task = tokio::spawn(handle(server, worker.tunnel()));
        client
            .write_all(&[5, 1, 0, 5, 1, 0, 1, 127, 0, 0, 1, 0, 22])
            .await
            .unwrap();
        let mut response = [0; 12];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(response[3], 3);
        assert!(task.await.unwrap().is_err());
    }
}
