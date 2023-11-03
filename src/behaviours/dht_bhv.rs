use std::{
  collections::{hash_map::Entry, HashMap, HashSet, VecDeque},
  num::NonZeroUsize,
  task::Poll,
  time::Duration,
};

use futures::channel::oneshot::Sender;
use libp2p::{
  kad::{
    self, store::MemoryStore, BootstrapError, BootstrapOk, GetClosestPeersOk, GetRecordOk,
    GetRecordResult, ProgressStep, PutRecordResult, QueryId, QueryResult, Quorum, Record,
  },
  swarm::{NetworkBehaviour, PollParameters, ToSwarm},
  Multiaddr, PeerId,
};

use crate::utils::ExponentialBackoff;

pub struct Behaviour {
  begin_bootstrap: bool,
  bootnodes: HashMap<PeerId, HashSet<Multiaddr>>,
  event_queue: Vec<Event>,
  in_progress_get_closest_peers: HashMap<QueryId, Sender<()>>,
  in_progress_put_record_queries: HashMap<QueryId, KadPutQuery>,
  in_progress_get_record_queries: HashMap<QueryId, KadGetQuery>,
  queued_put_record_queries: VecDeque<KadPutQuery>,
  queued_get_record_queries: VecDeque<KadGetQuery>,
  kad_dht: kad::Behaviour<MemoryStore>,
  bootstrap: Bootstrap,
  random_walk: RandomWalk,
  replication_factor: NonZeroUsize,
}

#[derive(Debug)]
pub enum Event {
  IsBootstrapped,
}

pub struct Bootstrap {
  state: State,
  backoff: ExponentialBackoff,
}

pub struct RandomWalk {
  state: State,
  backoff: ExponentialBackoff,
}

pub enum State {
  NotStarted,
  Started,
  Finished,
}

#[derive(Debug)]
pub struct KadGetQuery {
  pub backoff: ExponentialBackoff,
  pub key: Vec<u8>,
  pub retry_count: u8,
  pub records: HashMap<Vec<u8>, usize>,
}

#[derive(Debug)]
pub struct KadPutQuery {
  pub backoff: ExponentialBackoff,
  pub key: Vec<u8>,
  pub value: Vec<u8>,
}

impl Behaviour {
  pub fn new(kad_dht: kad::Behaviour<MemoryStore>, replication_factor: NonZeroUsize) -> Self {
    Self {
      begin_bootstrap: false,
      bootnodes: Default::default(),
      event_queue: Default::default(),
      in_progress_get_closest_peers: Default::default(),
      in_progress_put_record_queries: Default::default(),
      in_progress_get_record_queries: Default::default(),
      queued_put_record_queries: Default::default(),
      queued_get_record_queries: Default::default(),
      kad_dht,
      bootstrap: Bootstrap {
        state: State::NotStarted,
        backoff: ExponentialBackoff::new(2, Duration::from_secs(1)),
      },
      random_walk: RandomWalk {
        state: State::NotStarted,
        backoff: ExponentialBackoff::new(2, Duration::from_secs(1)),
      },
      replication_factor,
    }
  }

  pub fn begin_bootstrap(&mut self) {
    self.begin_bootstrap = true;
  }

  #[allow(unused)]
  pub fn lookup_peer(&mut self, peer_id: Option<PeerId>, chan: Sender<()>) {
    let query_id = self
      .kad_dht
      .get_closest_peers(peer_id.unwrap_or(PeerId::random()));
    self.in_progress_get_closest_peers.insert(query_id, chan);
  }

  pub fn add_address(&mut self, peer_id: PeerId, address: Multiaddr) {
    self.kad_dht.add_address(&peer_id, address);
  }

  pub fn add_bootnodes(&mut self, peer_id: PeerId, address: Multiaddr) {
    self
      .bootnodes
      .entry(peer_id)
      .and_modify(|val| {
        val.insert(address.clone());
      })
      .or_insert([address].into());
  }

  pub fn put_record(&mut self, mut query: KadPutQuery) {
    let record = Record::new(query.key.clone(), query.value.clone());

    if !self.begin_bootstrap {
      match self
        .kad_dht
        .put_record(record, Quorum::N(self.replication_factor))
      {
        Ok(query_id) => {
          info!("success published {query_id:?} to DHT");
          self.in_progress_put_record_queries.insert(query_id, query);
        }
        Err(e) => {
          error!("error publishing to DHT: {e:?}");
          query.backoff.start_next(false);
          self.queued_put_record_queries.push_back(query);
        }
      }
    } else {
      self.queued_put_record_queries.push_back(query);
    }
  }

  fn handle_put_query(&mut self, record_results: PutRecordResult, id: QueryId) {
    if let Some(mut query) = self.in_progress_put_record_queries.remove(&id) {
      if let Err(e) = record_results {
        warn!("error performing put: {e}, retrying..");
        query.backoff.start_next(false);
        self.queued_put_record_queries.push_back(query);
      }
    }
  }

  pub fn get_record(
    &mut self,
    key: impl Into<Vec<u8>>,
    backoff: ExponentialBackoff,
    retry_count: u8,
  ) {
    if retry_count == 0 {
      return;
    }

    let key = key.into();

    let query_id = self.kad_dht.get_record(key.clone().into());
    let query = KadGetQuery {
      backoff,
      key,
      retry_count: retry_count - 1,
      records: HashMap::default(),
    };

    self.in_progress_get_record_queries.insert(query_id, query);
  }

  fn handle_get_query(&mut self, record_results: GetRecordResult, id: QueryId, last: bool) {
    if let Some(query) = self.in_progress_get_record_queries.get_mut(&id) {
      if let Ok(GetRecordOk::FoundRecord(record)) = record_results {
        match query.records.entry(record.record.value) {
          Entry::Occupied(mut o) => {
            let num_entries = o.get_mut();
            *num_entries += 1;
          }
          Entry::Vacant(v) => {
            v.insert(1);
          }
        }
      }
    } else {
      return;
    }

    if last {
      if let Some(KadGetQuery {
        backoff,
        key,
        retry_count,
        ..
      }) = self.in_progress_get_record_queries.remove(&id)
      {
        self.get_record(key, backoff, retry_count);
      }
    }
  }

  fn handle_event(&mut self, event: libp2p::kad::Event) {
    use libp2p::kad::Event::*;
    match event {
      OutboundQueryProgressed {
        id,
        result: QueryResult::PutRecord(record_results),
        step: ProgressStep { last: true, .. },
        ..
      } => self.handle_put_query(record_results, id),
      OutboundQueryProgressed {
        id,
        result: QueryResult::GetClosestPeers(r),
        stats,
        step: ProgressStep { last: true, .. },
      } => match r {
        Ok(GetClosestPeersOk { key, peers }) => {
          if let Some(chan) = self.in_progress_get_closest_peers.remove(&id) {
            _ = chan.send(());
          } else {
            self.random_walk.state = State::NotStarted;
            self.random_walk.backoff.start_next(true);
          }
          info!("successfully completed get closest peers for {key:?} with peers {peers:?}",);
        }
        Err(e) => {
          if let Some(chan) = self.in_progress_get_closest_peers.remove(&id) {
            _ = chan.send(());
          } else {
            self.random_walk.state = State::NotStarted;
            self.random_walk.backoff.start_next(true);
          }
          warn!("failed to get closest peers with {e:?} and stats {stats:?}",);
        }
      },
      OutboundQueryProgressed {
        result: QueryResult::GetRecord(record_results),
        id,
        step: ProgressStep { last, .. },
        ..
      } => {
        self.handle_get_query(record_results, id, last);
      }
      OutboundQueryProgressed {
        result:
          QueryResult::Bootstrap(Ok(BootstrapOk {
            peer: _,
            num_remaining,
          })),
        step: ProgressStep { last: true, .. },
        ..
      } => {
        if num_remaining == 0 {
          info!("finished bootstrap");
          self.bootstrap.state = State::Finished;
          self.event_queue.push(Event::IsBootstrapped);
          self.begin_bootstrap = false;
        } else {
          warn!("bootstrap in progress: num remaining nodes to ping {num_remaining:?}",);
        }
      }
      OutboundQueryProgressed {
        result: QueryResult::Bootstrap(Err(e)),
        ..
      } => {
        warn!("bootstrap attempt failed, retrying shortly..");
        let BootstrapError::Timeout { num_remaining, .. } = e;
        if num_remaining.is_none() {
          error!("failed bootstrap with error {e:?}, this should not happen and means all bootstrap nodes are down or were evicted from our local DHT, readding bootstrap nodes {:?}", self.bootnodes);
          for (peer, addrs) in self.bootnodes.clone() {
            for addr in addrs {
              self.kad_dht.add_address(&peer, addr);
            }
          }
        }
        self.bootstrap.state = State::NotStarted;
        self.bootstrap.backoff.start_next(true);
      }
      RoutingUpdated { peer, .. } => {
        info!("added peer {peer}");
      }
      e => debug!("kad_dht event: {e:?}"),
    }
  }
}

impl NetworkBehaviour for Behaviour {
  type ConnectionHandler = <kad::Behaviour<MemoryStore> as NetworkBehaviour>::ConnectionHandler;

  type ToSwarm = Event;

  fn handle_established_inbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    peer: PeerId,
    local_addr: &Multiaddr,
    remote_addr: &Multiaddr,
  ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
    self
      .kad_dht
      .handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
  }

  fn handle_pending_inbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    local_addr: &Multiaddr,
    remote_addr: &Multiaddr,
  ) -> Result<(), libp2p::swarm::ConnectionDenied> {
    self
      .kad_dht
      .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
  }

  fn handle_established_outbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    peer: PeerId,
    addr: &Multiaddr,
    role_override: libp2p::core::Endpoint,
  ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
    self
      .kad_dht
      .handle_established_outbound_connection(connection_id, peer, addr, role_override)
  }

  fn handle_pending_outbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    maybe_peer: Option<PeerId>,
    addresses: &[Multiaddr],
    effective_role: libp2p::core::Endpoint,
  ) -> Result<Vec<Multiaddr>, libp2p::swarm::ConnectionDenied> {
    self.kad_dht.handle_pending_outbound_connection(
      connection_id,
      maybe_peer,
      addresses,
      effective_role,
    )
  }

  fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm<Self::ConnectionHandler>) {
    self.kad_dht.on_swarm_event(event)
  }

  fn on_connection_handler_event(
    &mut self,
    peer_id: PeerId,
    connection_id: libp2p::swarm::ConnectionId,
    event: libp2p::swarm::THandlerOutEvent<Self>,
  ) {
    self
      .kad_dht
      .on_connection_handler_event(peer_id, connection_id, event)
  }

  fn poll(
    &mut self,
    cx: &mut std::task::Context<'_>,
    params: &mut impl PollParameters,
  ) -> std::task::Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>>
  {
    if matches!(self.bootstrap.state, State::NotStarted)
      && self.bootstrap.backoff.is_expired()
      && self.begin_bootstrap
    {
      match self.kad_dht.bootstrap() {
        Ok(_) => {
          info!("bootstrap started");
          self.bootstrap.state = State::Started;
        }
        Err(_) => {
          error!("bootstrap start failed, no known peers");
          for (peer, addrs) in self.bootnodes.clone() {
            for addr in addrs {
              self.kad_dht.add_address(&peer, addr);
            }
          }
        }
      }
    }

    // if matches!(self.random_walk.state, State::NotStarted)
    //   && self.random_walk.backoff.is_expired()
    //   && matches!(self.bootstrap.state, State::Finished)
    // {
    //   self.kad_dht.get_closest_peers(PeerId::random());
    //   self.random_walk.state = State::Started;
    // }

    while let Some(req) = self.queued_get_record_queries.pop_front() {
      if req.backoff.is_expired() {
        self.get_record(req.key, req.backoff, req.retry_count);
      } else {
        self.queued_get_record_queries.push_back(req);
      }
    }
    while let Some(req) = self.queued_put_record_queries.pop_front() {
      if req.backoff.is_expired() {
        self.put_record(req);
      } else {
        self.queued_put_record_queries.push_back(req);
      }
    }

    while let Poll::Ready(ready) = NetworkBehaviour::poll(&mut self.kad_dht, cx, params) {
      match ready {
        ToSwarm::GenerateEvent(event) => self.handle_event(event),
        ToSwarm::Dial { opts } => {
          return Poll::Ready(ToSwarm::Dial { opts });
        }
        ToSwarm::NotifyHandler {
          peer_id,
          handler,
          event,
        } => {
          return Poll::Ready(ToSwarm::NotifyHandler {
            peer_id,
            handler,
            event,
          });
        }
        ToSwarm::CloseConnection {
          peer_id,
          connection,
        } => {
          return Poll::Ready(ToSwarm::CloseConnection {
            peer_id,
            connection,
          });
        }
        ToSwarm::ListenOn { opts } => {
          return Poll::Ready(ToSwarm::ListenOn { opts });
        }
        ToSwarm::RemoveListener { id } => {
          return Poll::Ready(ToSwarm::RemoveListener { id });
        }
        ToSwarm::NewExternalAddrCandidate(c) => {
          return Poll::Ready(ToSwarm::NewExternalAddrCandidate(c));
        }
        ToSwarm::ExternalAddrConfirmed(c) => {
          return Poll::Ready(ToSwarm::ExternalAddrConfirmed(c));
        }
        ToSwarm::ExternalAddrExpired(c) => {
          return Poll::Ready(ToSwarm::ExternalAddrExpired(c));
        }
      }
      if !self.event_queue.is_empty() {
        return Poll::Ready(ToSwarm::GenerateEvent(self.event_queue.remove(0)));
      }
    }
    Poll::Pending
  }
}
