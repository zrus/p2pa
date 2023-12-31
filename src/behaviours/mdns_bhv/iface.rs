mod dns;
mod query;

use self::dns::{build_query, build_query_response, build_service_discovery_response};
use self::query::MdnsPacket;
use super::socket::{AsyncSocket, TokioUdpSocket};
use super::timer::Timer;
use super::Config;
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use libp2p::core::Multiaddr;
use libp2p::identity::PeerId;
use libp2p::mdns::{IPV4_MDNS_MULTICAST_ADDRESS, IPV6_MDNS_MULTICAST_ADDRESS};
use libp2p::swarm::ListenAddresses;
use socket2::{Domain, Socket, Type};
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::{
  collections::VecDeque,
  io,
  net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
  pin::Pin,
  task::{Context, Poll},
  time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub enum ProbeState {
  Probing(Duration),
  Finished(Duration),
}

impl ProbeState {
  pub fn interval(&self) -> &Duration {
    match self {
      ProbeState::Probing(query_interval) => query_interval,
      ProbeState::Finished(query_interval) => query_interval,
    }
  }
}

#[derive(Debug)]
pub(crate) struct InterfaceState {
  /// Address this instance is bound to.
  addr: IpAddr,
  /// Receive socket.
  recv_socket: TokioUdpSocket,
  /// Send socket.
  send_socket: TokioUdpSocket,

  listen_addresses: Arc<RwLock<ListenAddresses>>,

  query_response_sender: mpsc::Sender<(PeerId, Multiaddr, Instant)>,

  /// Buffer used for receiving data from the main socket.
  /// RFC6762 discourages packets larger than the interface MTU, but allows sizes of up to 9000
  /// bytes, if it can be ensured that all participating devices can handle such large packets.
  /// For computers with several interfaces and IP addresses responses can easily reach sizes in
  /// the range of 3000 bytes, so 4096 seems sensible for now. For more information see
  /// [rfc6762](https://tools.ietf.org/html/rfc6762#page-46).
  recv_buffer: [u8; 4096],
  /// Buffers pending to send on the main socket.
  send_buffer: VecDeque<Vec<u8>>,
  /// Discovery interval.
  query_interval: Duration,
  /// Discovery timer.
  timeout: Timer,
  /// Multicast address.
  multicast_addr: IpAddr,
  /// Discovered addresses.
  discovered: VecDeque<(PeerId, Multiaddr, Instant)>,
  /// TTL
  ttl: Duration,
  probe_state: ProbeState,
  local_peer_id: PeerId,
  config: Config,
}

impl InterfaceState {
  /// Builds a new [`InterfaceState`].
  pub(crate) fn new(
    addr: IpAddr,
    config: Config,
    local_peer_id: PeerId,
    listen_addresses: Arc<RwLock<ListenAddresses>>,
    query_response_sender: mpsc::Sender<(PeerId, Multiaddr, Instant)>,
  ) -> io::Result<Self> {
    info!("creating instance on iface address {addr}");
    let recv_socket = match addr {
      IpAddr::V4(addr) => {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(socket2::Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;
        socket.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 5353).into())?;
        socket.set_multicast_loop_v4(true)?;
        socket.set_multicast_ttl_v4(255)?;
        socket.join_multicast_v4(&IPV4_MDNS_MULTICAST_ADDRESS, &addr)?;
        <TokioUdpSocket as AsyncSocket>::from_std(UdpSocket::from(socket))?
      }
      IpAddr::V6(_) => {
        let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(socket2::Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;
        socket.bind(&SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 5353).into())?;
        socket.set_multicast_loop_v6(true)?;
        // TODO: find interface matching addr.
        socket.join_multicast_v6(&IPV6_MDNS_MULTICAST_ADDRESS, 0)?;
        <TokioUdpSocket as AsyncSocket>::from_std(UdpSocket::from(socket))?
      }
    };
    let bind_addr = match addr {
      IpAddr::V4(_) => SocketAddr::new(addr, 0),
      IpAddr::V6(_addr) => {
        // TODO: if-watch should return the scope_id of an address
        // as a workaround we bind to unspecified, which means that
        // this probably won't work when using multiple interfaces.
        // SocketAddr::V6(SocketAddrV6::new(addr, 0, 0, scope_id))
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
      }
    };
    let send_socket = <TokioUdpSocket as AsyncSocket>::from_std(UdpSocket::bind(bind_addr)?)?;

    // randomize timer to prevent all converging and firing at the same time.
    let query_interval = {
      use rand::Rng;
      let mut rng = rand::thread_rng();
      let jitter = rng.gen_range(0..100);
      config.query_interval + Duration::from_millis(jitter)
    };
    let multicast_addr = match addr {
      IpAddr::V4(_) => IpAddr::V4(IPV4_MDNS_MULTICAST_ADDRESS),
      IpAddr::V6(_) => IpAddr::V6(IPV6_MDNS_MULTICAST_ADDRESS),
    };
    Ok(Self {
      addr,
      recv_socket,
      send_socket,
      listen_addresses,
      query_response_sender,
      recv_buffer: [0; 4096],
      send_buffer: Default::default(),
      discovered: Default::default(),
      query_interval,
      timeout: Timer::interval_at(Instant::now(), config.probe_interval),
      multicast_addr,
      ttl: config.ttl,
      probe_state: ProbeState::Probing(config.probe_interval),
      local_peer_id,
      config,
    })
  }

  pub(crate) fn reset_timer(&mut self) {
    trace!("reset timer at {} with {:?}", self.addr, self.probe_state);
    let interval = *self.probe_state.interval();
    self.timeout = Timer::interval(interval);
  }

  fn mdns_socket(&self) -> SocketAddr {
    SocketAddr::new(self.multicast_addr, 5353)
  }
}

impl Future for InterfaceState {
  type Output = ();

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let config = self.config.clone();
    let this = self.get_mut();

    loop {
      // 1st priority: Low latency: Create packet ASAP after timeout.
      if this.timeout.poll_next_unpin(cx).is_ready() {
        trace!("sending query on iface {}", this.addr);
        this
          .send_buffer
          .push_back(build_query(&config.service_name));
        trace!("tick at {} with {:?}", this.addr, this.probe_state);

        // Stop to probe when the initial interval reach the query interval
        if let ProbeState::Probing(interval) = this.probe_state {
          this.probe_state = if interval * 2 >= this.query_interval {
            ProbeState::Finished(this.query_interval)
          } else {
            ProbeState::Probing(interval)
          };
        }

        this.reset_timer();
      }

      // 2nd priority: Keep local buffers small: Send packets to remote.
      if let Some(packet) = this.send_buffer.pop_front() {
        match this.send_socket.poll_write(cx, &packet, this.mdns_socket()) {
          Poll::Ready(Ok(_)) => {
            trace!("sent packet on iface address {}", this.addr);
            continue;
          }
          Poll::Ready(Err(err)) => {
            error!(
              "error sending packet on iface address {} with {}",
              this.addr, err
            );
            continue;
          }
          Poll::Pending => {
            this.send_buffer.push_front(packet);
          }
        }
      }

      // 3rd priority: Keep local buffers small: Return discovered addresses.
      if this.query_response_sender.poll_ready_unpin(cx).is_ready() {
        if let Some(discovered) = this.discovered.pop_front() {
          match this.query_response_sender.try_send(discovered) {
            Ok(()) => {}
            Err(e) if e.is_disconnected() => {
              return Poll::Ready(());
            }
            Err(e) => {
              this.discovered.push_front(e.into_inner());
            }
          }

          continue;
        }
      }

      // 4th priority: Remote work: Answer incoming requests.
      match this
        .recv_socket
        .poll_read(cx, &mut this.recv_buffer)
        .map_ok(|(len, from)| {
          MdnsPacket::new_from_bytes(
            &this.recv_buffer[..len],
            from,
            &config.service_name_fqdn,
            &config.meta_query_service_fqdn,
          )
        }) {
        Poll::Ready(Ok(Ok(Some(MdnsPacket::Query(query))))) => {
          trace!(
            "received query from remote address {} on address {}",
            query.remote_addr(),
            this.addr
          );

          this.send_buffer.extend(build_query_response(
            query.query_id(),
            this.local_peer_id,
            this
              .listen_addresses
              .read()
              .unwrap_or_else(|e| e.into_inner())
              .iter(),
            this.ttl,
            &config.service_name,
          ));
          continue;
        }
        Poll::Ready(Ok(Ok(Some(MdnsPacket::Response(response))))) => {
          trace!(
            "received response from remote address {} on address {}",
            response.remote_addr(),
            this.addr,
          );

          this
            .discovered
            .extend(response.extract_discovered(Instant::now(), this.local_peer_id));

          // Stop probing when we have a valid response
          if !this.discovered.is_empty() {
            this.probe_state = ProbeState::Finished(this.query_interval);
            this.reset_timer();
          }
          continue;
        }
        Poll::Ready(Ok(Ok(Some(MdnsPacket::ServiceDiscovery(disc))))) => {
          trace!(
            "received service discovery from remote address {} on address {}",
            disc.remote_addr(),
            this.addr,
          );

          this.send_buffer.push_back(build_service_discovery_response(
            disc.query_id(),
            this.ttl,
            &config.service_name,
            &config.meta_query_service,
          ));
          continue;
        }
        Poll::Ready(Err(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {
          // No more bytes available on the socket to read
        }
        Poll::Ready(Err(err)) => {
          error!("failed reading datagram: {}", err);
        }
        Poll::Ready(Ok(Err(err))) => {
          debug!("Parsing mdns packet failed: {:?}", err);
        }
        Poll::Ready(Ok(Ok(None))) | Poll::Pending => {}
      }

      return Poll::Pending;
    }
  }
}
