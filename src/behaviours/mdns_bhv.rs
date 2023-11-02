mod config;
mod iface;
mod socket;
mod timer;

pub use self::config::Config;
use self::iface::InterfaceState;
use self::timer::Timer;
use futures::channel::mpsc;
use futures::{Stream, StreamExt};
use if_watch::tokio::IfWatcher;
use if_watch::IfEvent;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::behaviour::FromSwarm;
use libp2p::swarm::{
  dummy, ConnectionDenied, ConnectionId, ListenAddresses, NetworkBehaviour, THandler,
  THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::PeerId;
use smallvec::SmallVec;
use std::collections::hash_map::{Entry, HashMap};
use std::sync::{Arc, RwLock};
use std::{cmp, io, net::IpAddr, pin::Pin, task::Context, task::Poll, time::Instant};
use tokio::task::JoinHandle;

/// A `NetworkBehaviour` for mDNS. Automatically discovers peers on the local network and adds
/// them to the topology.
#[derive(Debug)]
pub struct Behaviour {
  /// InterfaceState config.
  config: Config,

  /// Iface watcher.
  if_watch: IfWatcher,

  /// Handles to tasks running the mDNS queries.
  if_tasks: HashMap<IpAddr, JoinHandle<()>>,

  query_response_receiver: mpsc::Receiver<(PeerId, Multiaddr, Instant)>,
  query_response_sender: mpsc::Sender<(PeerId, Multiaddr, Instant)>,

  /// List of nodes that we have discovered, the address, and when their TTL expires.
  ///
  /// Each combination of `PeerId` and `Multiaddr` can only appear once, but the same `PeerId`
  /// can appear multiple times.
  discovered_nodes: SmallVec<[(PeerId, Multiaddr, Instant); 16]>,

  /// Future that fires when the TTL of at least one node in `discovered_nodes` expires.
  ///
  /// `None` if `discovered_nodes` is empty.
  closest_expiration: Option<Timer>,

  /// The current set of listen addresses.
  ///
  /// This is shared across all interface tasks using an [`RwLock`].
  /// The [`Behaviour`] updates this upon new [`FromSwarm`] events where as [`InterfaceState`]s read from it to answer inbound mDNS queries.
  listen_addresses: Arc<RwLock<ListenAddresses>>,

  local_peer_id: PeerId,

  event_queue: Vec<Event>,
}

impl Behaviour {
  /// Builds a new `Mdns` behaviour.
  pub fn new(config: Config, local_peer_id: PeerId) -> io::Result<Self> {
    let (tx, rx) = mpsc::channel(8); // Chosen arbitrarily.

    Ok(Self {
      config,
      if_watch: IfWatcher::new()?,
      if_tasks: Default::default(),
      query_response_receiver: rx,
      query_response_sender: tx,
      discovered_nodes: Default::default(),
      closest_expiration: Default::default(),
      listen_addresses: Default::default(),
      local_peer_id,
      event_queue: Default::default(),
    })
  }

  #[allow(unused)]
  /// Returns the list of nodes that we have discovered through mDNS and that are not expired.
  pub fn discovered_nodes(&self) -> impl ExactSizeIterator<Item = &PeerId> {
    self.discovered_nodes.iter().map(|(p, _, _)| p)
  }
}

impl NetworkBehaviour for Behaviour {
  type ConnectionHandler = dummy::ConnectionHandler;
  type ToSwarm = Event;

  fn handle_established_inbound_connection(
    &mut self,
    _: ConnectionId,
    _: PeerId,
    _: &Multiaddr,
    _: &Multiaddr,
  ) -> Result<THandler<Self>, ConnectionDenied> {
    Ok(dummy::ConnectionHandler)
  }

  fn handle_pending_outbound_connection(
    &mut self,
    _connection_id: ConnectionId,
    maybe_peer: Option<PeerId>,
    _addresses: &[Multiaddr],
    _effective_role: Endpoint,
  ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
    let peer_id = match maybe_peer {
      None => return Ok(vec![]),
      Some(peer) => peer,
    };

    Ok(
      self
        .discovered_nodes
        .iter()
        .filter(|(peer, _, _)| peer == &peer_id)
        .map(|(_, addr, _)| addr.clone())
        .collect(),
    )
  }

  fn handle_established_outbound_connection(
    &mut self,
    _: ConnectionId,
    _: PeerId,
    _: &Multiaddr,
    _: Endpoint,
  ) -> Result<THandler<Self>, ConnectionDenied> {
    Ok(dummy::ConnectionHandler)
  }

  fn on_connection_handler_event(
    &mut self,
    _: PeerId,
    _: ConnectionId,
    ev: THandlerOutEvent<Self>,
  ) {
    void::unreachable(ev)
  }

  fn on_swarm_event(&mut self, event: FromSwarm) {
    self
      .listen_addresses
      .write()
      .unwrap_or_else(|e| e.into_inner())
      .on_swarm_event(&event);
  }

  fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
    // Poll ifwatch.
    while let Poll::Ready(Some(event)) = Pin::new(&mut self.if_watch).poll_next(cx) {
      match event {
        Ok(IfEvent::Up(inet)) => {
          let addr = inet.addr();
          if addr.is_loopback() {
            continue;
          }
          if addr.is_ipv4() && self.config.ipv6 || addr.is_ipv6() && !self.config.ipv6 {
            continue;
          }
          if let Entry::Vacant(e) = self.if_tasks.entry(addr) {
            match InterfaceState::new(
              addr,
              self.config.clone(),
              self.local_peer_id,
              self.listen_addresses.clone(),
              self.query_response_sender.clone(),
            ) {
              Ok(iface_state) => {
                e.insert(tokio::spawn(iface_state));
              }
              Err(err) => error!("failed to create `InterfaceState`: {}", err),
            }
          }
        }
        Ok(IfEvent::Down(inet)) => {
          if let Some(handle) = self.if_tasks.remove(&inet.addr()) {
            info!("dropping instance {}", inet.addr());

            handle.abort();
          }
        }
        Err(err) => error!("if watch returned an error: {}", err),
      }
    }
    // Emit discovered event.
    let mut discovered = Vec::new();

    while let Poll::Ready(Some((peer, addr, expiration))) =
      self.query_response_receiver.poll_next_unpin(cx)
    {
      if let Some((_, _, cur_expires)) = self
        .discovered_nodes
        .iter_mut()
        .find(|(p, a, _)| *p == peer && *a == addr)
      {
        *cur_expires = cmp::max(*cur_expires, expiration);
      } else {
        info!("discovered: {} {}", peer, addr);
        self.discovered_nodes.push((peer, addr.clone(), expiration));
        discovered.push((peer, addr));
      }
    }

    if !discovered.is_empty() {
      let event = Event::Discovered(discovered);
      self.event_queue.push(event);
    }
    // Emit expired event.
    let now = Instant::now();
    let mut closest_expiration = None;
    let mut expired = Vec::new();
    self.discovered_nodes.retain(|(peer, addr, expiration)| {
      if *expiration <= now {
        info!("expired: {} {}", peer, addr);
        expired.push((*peer, addr.clone()));
        return false;
      }
      closest_expiration = Some(closest_expiration.unwrap_or(*expiration).min(*expiration));
      true
    });
    if !expired.is_empty() {
      let event = Event::Expired(expired);
      self.event_queue.push(event);
    }
    if let Some(closest_expiration) = closest_expiration {
      let mut timer = Timer::at(closest_expiration);
      let _ = Pin::new(&mut timer).poll_next(cx);

      self.closest_expiration = Some(timer);
    }
    if !self.event_queue.is_empty() {
      return Poll::Ready(ToSwarm::GenerateEvent(self.event_queue.remove(0)));
    }
    Poll::Pending
  }
}

/// Event that can be produced by the `Mdns` behaviour.
#[derive(Debug, Clone)]
pub enum Event {
  /// Discovered nodes through mDNS.
  Discovered(Vec<(PeerId, Multiaddr)>),

  /// The given combinations of `PeerId` and `Multiaddr` have expired.
  ///
  /// Each discovered record has a time-to-live. When this TTL expires and the address hasn't
  /// been refreshed, we remove it from the list and emit it as an `Expired` event.
  Expired(Vec<(PeerId, Multiaddr)>),
}
