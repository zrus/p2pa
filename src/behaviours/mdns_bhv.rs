mod config;
mod iface;
mod socket;
mod timer;

pub use self::config::Config;
use self::iface::InterfaceState;
use futures::Stream;
use if_watch::IfEvent;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::identity::PeerId;
use libp2p::swarm::behaviour::FromSwarm;
use libp2p::swarm::{
  dummy, ConnectionDenied, ConnectionId, ListenAddresses, NetworkBehaviour, PollParameters,
  THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use smallvec::SmallVec;
use std::collections::hash_map::{Entry, HashMap};
use std::{cmp, io, net::IpAddr, pin::Pin, task::Context, task::Poll, time::Instant};

use self::timer::Timer;
use if_watch::tokio::IfWatcher;

/// A `NetworkBehaviour` for mDNS. Automatically discovers peers on the local network and adds
/// them to the topology.
#[derive(Debug)]
pub struct Behaviour {
  /// InterfaceState config.
  config: Config,

  /// Iface watcher.
  if_watch: IfWatcher,

  /// Mdns interface states.
  iface_states: HashMap<IpAddr, InterfaceState>,

  /// List of nodes that we have discovered, the address, and when their TTL expires.
  ///
  /// Each combination of `PeerId` and `Multiaddr` can only appear once, but the same `PeerId`
  /// can appear multiple times.
  discovered_nodes: SmallVec<[(PeerId, Multiaddr, Instant); 16]>,

  /// Future that fires when the TTL of at least one node in `discovered_nodes` expires.
  ///
  /// `None` if `discovered_nodes` is empty.
  closest_expiration: Option<Timer>,

  listen_addresses: ListenAddresses,

  local_peer_id: PeerId,
}

impl Behaviour {
  /// Builds a new `Mdns` behaviour.
  pub fn new(config: Config, local_peer_id: PeerId) -> io::Result<Self> {
    Ok(Self {
      config,
      if_watch: IfWatcher::new()?,
      iface_states: Default::default(),
      discovered_nodes: Default::default(),
      closest_expiration: Default::default(),
      listen_addresses: Default::default(),
      local_peer_id,
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

  fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) {
    self.listen_addresses.on_swarm_event(&event);

    match event {
      FromSwarm::NewListener(_) => {
        trace!("waking interface state because listening address changed");
        for iface in self.iface_states.values_mut() {
          iface.fire_timer();
        }
      }
      FromSwarm::ConnectionClosed(_)
      | FromSwarm::ConnectionEstablished(_)
      | FromSwarm::DialFailure(_)
      | FromSwarm::AddressChange(_)
      | FromSwarm::ListenFailure(_)
      | FromSwarm::NewListenAddr(_)
      | FromSwarm::ExpiredListenAddr(_)
      | FromSwarm::ListenerError(_)
      | FromSwarm::ListenerClosed(_)
      | FromSwarm::NewExternalAddrCandidate(_)
      | FromSwarm::ExternalAddrExpired(_)
      | FromSwarm::ExternalAddrConfirmed(_) => {}
    }
  }

  fn poll(
    &mut self,
    cx: &mut Context<'_>,
    _: &mut impl PollParameters,
  ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
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
          if let Entry::Vacant(e) = self.iface_states.entry(addr) {
            match InterfaceState::new(addr, self.config.clone(), self.local_peer_id) {
              Ok(iface_state) => {
                e.insert(iface_state);
              }
              Err(err) => error!("failed to create `InterfaceState`: {}", err),
            }
          }
        }
        Ok(IfEvent::Down(inet)) => {
          if self.iface_states.contains_key(&inet.addr()) {
            info!("dropping instance {}", inet.addr());
            self.iface_states.remove(&inet.addr());
          }
        }
        Err(err) => error!("if watch returned an error: {}", err),
      }
    }
    // Emit discovered event.
    let mut discovered = Vec::new();
    for iface_state in self.iface_states.values_mut() {
      while let Poll::Ready((peer, addr, expiration)) = iface_state.poll(cx, &self.listen_addresses)
      {
        if let Some((_, _, cur_expires)) = self
          .discovered_nodes
          .iter_mut()
          .find(|(p, a, _)| *p == peer && *a == addr)
        {
          *cur_expires = cmp::max(*cur_expires, expiration);
        } else {
          debug!("discovered: {} {}", peer, addr);
          self.discovered_nodes.push((peer, addr.clone(), expiration));
          discovered.push((peer, addr));
        }
      }
    }
    if !discovered.is_empty() {
      let event = Event::Discovered(discovered);
      return Poll::Ready(ToSwarm::GenerateEvent(event));
    }
    // Emit expired event.
    let now = Instant::now();
    let mut closest_expiration = None;
    let mut expired = Vec::new();
    self.discovered_nodes.retain(|(peer, addr, expiration)| {
      if *expiration <= now {
        debug!("expired: {} {}", peer, addr);
        expired.push((*peer, addr.clone()));
        return false;
      }
      closest_expiration = Some(closest_expiration.unwrap_or(*expiration).min(*expiration));
      true
    });
    if !expired.is_empty() {
      let event = Event::Expired(expired);
      return Poll::Ready(ToSwarm::GenerateEvent(event));
    }
    if let Some(closest_expiration) = closest_expiration {
      let mut timer = Timer::at(closest_expiration);
      let _ = Pin::new(&mut timer).poll_next(cx);

      self.closest_expiration = Some(timer);
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
