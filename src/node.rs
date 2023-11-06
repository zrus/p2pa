use std::{
  collections::HashSet,
  num::{NonZeroU8, NonZeroUsize},
  str::FromStr,
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
  },
  time::Duration,
};

use futures::{Future, FutureExt, StreamExt};
use libp2p::{
  build_multiaddr,
  core::ConnectedPoint,
  dcutr, gossipsub, identify,
  identity::Keypair,
  kad::{self, store::MemoryStore},
  multiaddr::Protocol,
  noise, rendezvous,
  swarm::{behaviour::toggle::Toggle, SwarmEvent},
  tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use tokio::{
  sync::{oneshot, Mutex},
  time::sleep,
};

use crate::{
  behaviours::{
    dht_bhv::{self, KadPutQuery},
    gossip_bhv, mdns_bhv, rendezvous_bhv, NodeBehaviour, NodeBehaviourEvent,
  },
  commands::Command,
  config::Config,
  events::Event,
  prelude::*,
  utils::{
    is_loopback,
    unbounded_channel::{unbounded, UnboundedReceiver, UnboundedSender},
    SubscribableMutex,
  },
};

pub struct NodeReceiver {
  receiver_spawned: AtomicBool,
  is_killed: AtomicBool,
  event_chan: Mutex<UnboundedReceiver<Event>>,
  kill_req: Mutex<Option<oneshot::Sender<()>>>,
  kill_resp: Mutex<Option<oneshot::Receiver<()>>>,
}

pub struct Node<State> {
  peer_id: PeerId,
  state: Arc<SubscribableMutex<State>>,
  command_chan: UnboundedSender<Command>,
  receiver: NodeReceiver,
  config: Config,
}

struct NodeInner {
  swarm: Swarm<NodeBehaviour>,
  dialing_relays: HashSet<Multiaddr>,
}

impl<State> Node<State> {
  pub async fn init(state: State, config: Config) -> Ret<Self> {
    let identity = match config.seed() {
      Some(seed) => Keypair::ed25519_from_bytes(seed.to_vec())?,
      None => Keypair::generate_ed25519(),
    };
    let peer_id = PeerId::from_public_key(&identity.public());

    let swarm = build_swarm(&identity, &config).await?;
    let mut inner = NodeInner {
      swarm,
      dialing_relays: Default::default(),
    };

    // Start listening
    inner.start_listen().await?;
    let (req_chan, resp_chan) = Box::pin(inner.spawn_listeners()).await?;
    let (kill_req, kill_resp) = oneshot::channel();

    Ok(Self {
      config,
      peer_id,
      state: Arc::new(SubscribableMutex::new(state)),
      command_chan: req_chan,
      receiver: NodeReceiver {
        receiver_spawned: AtomicBool::new(false),
        is_killed: AtomicBool::new(false),
        event_chan: Mutex::new(resp_chan),
        kill_req: Mutex::new(Some(kill_req)),
        kill_resp: Mutex::new(Some(kill_resp)),
      },
    })
  }

  pub fn execute<F, R>(self: &Arc<Self>, cb: F)
  where
    F: Fn(Arc<Node<State>>, Event) -> R + Sync + Send + 'static,
    R: Future<Output = Ret> + Send + 'static,
    State: Send + 'static,
  {
    assert!(
      !self.receiver.receiver_spawned.swap(true, Ordering::Relaxed),
      "node must be already spawned, need fix"
    );

    let node = Arc::clone(self);
    let handle = tokio::spawn(async move {
      let event_chan = node.receiver.event_chan.lock().await;
      let Some(kill_resp) = node.receiver.kill_resp.lock().await.take() else {
        error!("`execute` has been called on an already down node");
        return;
      };
      let mut next_event = event_chan.recv().boxed();
      let mut kill_resp = kill_resp.boxed();
      loop {
        match futures::future::select(next_event, kill_resp).await {
          futures::future::Either::Left((incomming, other)) => {
            let Ok(event) = incomming else {
              return;
            };
            if let Err(e) = cb(node.clone(), event).await {
              error!("execute handler error: {e}");
              return;
            }
            kill_resp = other;
            next_event = event_chan.recv().boxed();
          }
          futures::future::Either::Right(_) => {
            node.receiver.is_killed.store(true, Ordering::Relaxed);
            return;
          }
        }
      }
    });
    std::mem::forget(handle);
  }

  pub fn spin_up(&self) -> Ret {
    if self.config.enable_relay() {
      for (peer, addr) in self.config.relay_nodes() {
        self.connect_relay(addr.clone().with(Protocol::P2p(*peer)).to_string())?;
      }
    }

    if self.config.enable_rendezvous() {
      for (peer, addr) in self.config.rendezvous_points() {
        self.dial(addr.clone().with(Protocol::P2p(*peer)).to_string())?;
      }
      for namespace in self.config.rendezvous_namespaces() {
        self.register(namespace)?;
      }
      self.discover()?;
    }

    if self.config.enable_dht() {
      self.begin_bootstrap()?;
    }

    Ok(())
  }

  pub fn send_command(&self, command: Command) -> Ret {
    self.command_chan.send(command)?;
    Ok(())
  }

  pub fn add_known_peer(&self, peer_id: impl Into<String>, addr: impl Into<String>) -> Ret {
    let add_known_peer_cmd = Command::AddKnownPeer {
      peer: peer_id.into(),
      address: addr.into(),
    };
    self.command_chan.send(add_known_peer_cmd)?;
    Ok(())
  }

  pub fn add_bootstrap_node(&self, peer_id: impl Into<String>, addr: impl Into<String>) -> Ret {
    let add_bootstrap_node_cmd = Command::AddBootNode {
      peer: peer_id.into(),
      address: addr.into(),
    };
    self.command_chan.send(add_bootstrap_node_cmd)?;
    Ok(())
  }

  pub fn dial(&self, addr: impl Into<String>) -> Ret {
    let dial_cmd = Command::Dial(addr.into());
    self.command_chan.send(dial_cmd)?;
    Ok(())
  }

  pub fn begin_bootstrap(&self) -> Ret {
    let begin_bootstrap_cmd = Command::BeginBootstrap;
    self.command_chan.send(begin_bootstrap_cmd)?;
    Ok(())
  }

  pub fn put_record(&self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Ret {
    let put_record_cmd = Command::PutRecord {
      key: key.into(),
      value: value.into(),
    };
    self.command_chan.send(put_record_cmd)?;
    Ok(())
  }

  pub fn get_record(&self, key: impl Into<Vec<u8>>, retry: u8) -> Ret {
    let get_record_cmd = Command::GetRecord {
      key: key.into(),
      retry,
    };
    self.command_chan.send(get_record_cmd)?;
    Ok(())
  }

  pub fn lookup_peer(&self, peer_id: impl Into<String>) -> Ret {
    let lookup_peer_cmd = Command::LookupPeer(peer_id.into());
    self.command_chan.send(lookup_peer_cmd)?;
    Ok(())
  }

  pub fn connect_relay(&self, addr: impl Into<String>) -> Ret {
    let connect_relay_cmd = Command::ListenViaRelay(addr.into());
    self.command_chan.send(connect_relay_cmd)?;
    Ok(())
  }

  pub fn register(&self, namespace: impl Into<String>) -> Ret {
    let namespace = namespace.into();
    for (point, addr) in self.config.rendezvous_points() {
      let register_cmd = Command::Register {
        point: addr.clone().with(Protocol::P2p(*point)).to_string(),
        namespace: namespace.clone(),
      };
      self.command_chan.send(register_cmd)?;
    }
    Ok(())
  }

  pub fn discover(&self) -> Ret {
    for ((point, addr), namespace) in self
      .config
      .rendezvous_points()
      .iter()
      .zip(self.config.rendezvous_namespaces())
    {
      let discover_rdvz_cmd = Command::Discover {
        point: addr.clone().with(Protocol::P2p(*point)).to_string(),
        namespace: namespace.to_owned(),
      };
      self.command_chan.send(discover_rdvz_cmd)?;
    }
    Ok(())
  }

  pub fn publish(&self, topic: impl Into<String>, contents: impl Into<Vec<u8>>) -> Ret {
    let gossip_cmd = Command::Publish {
      topic: topic.into(),
      contents: contents.into(),
    };
    self.command_chan.send(gossip_cmd)?;
    Ok(())
  }

  pub fn subscribe(&self, topic: impl Into<String>) -> Ret {
    let subscribe_cmd = Command::Subscribe(topic.into());
    self.command_chan.send(subscribe_cmd)?;
    Ok(())
  }

  pub fn unsubscribe(&self, topic: impl Into<String>) -> Ret {
    let unsubscribe_cmd = Command::Unsubscribe(topic.into());
    self.command_chan.send(unsubscribe_cmd)?;
    Ok(())
  }

  pub async fn get_connected_peers(&self) -> Ret<HashSet<PeerId>> {
    let (s, r) = oneshot::channel();
    let get_connected_peers_cmd = Command::GetConnectedPeers(s);
    self.command_chan.send(get_connected_peers_cmd)?;
    Ok(r.await?)
  }

  pub async fn get_connected_peers_len(&self) -> Ret<usize> {
    let (s, r) = oneshot::channel();
    let get_connected_peers_len_cmd = Command::GetConnectedPeersLen(s);
    self.command_chan.send(get_connected_peers_len_cmd)?;
    Ok(r.await?)
  }

  pub async fn shutdown(&self) -> Ret {
    self.command_chan.send(Command::Shutdown)?;
    if let Some(kill_req) = self.receiver.kill_req.lock().await.take() {
      warn!("shutting down..");
      if kill_req.send(()).is_err() {
        error!("send shutdown signal failed");
      }
    }
    Ok(())
  }

  pub fn is_down(&self) -> bool {
    self.receiver.is_killed.load(Ordering::Relaxed)
  }

  pub async fn modify_state<F>(&self, cb: F)
  where
    F: FnMut(&mut State),
  {
    self.state.modify(cb).await;
  }

  pub async fn peer_id(&self) -> PeerId {
    self.peer_id
  }
}

impl<State: Clone> Node<State> {
  pub async fn state(&self) -> State {
    self.state.cloned().await
  }
}

impl NodeInner {
  async fn start_listen(&mut self) -> Ret {
    // let listen_addr = build_multiaddr!(Ip4([0, 0, 0, 0]), Udp(0u16), QuicV1);
    // self.swarm.listen_on(listen_addr)?;
    let listen_addr = build_multiaddr!(Ip4([0, 0, 0, 0]), Tcp(0u16));
    self.swarm.listen_on(listen_addr)?;
    // Listen on all interfaces
    loop {
      futures::select! {
        event = self.swarm.select_next_some() => {
          if let SwarmEvent::NewListenAddr {
            address,
            ..
          } = event {
            info!("listening on {address}");
            // If the address is loopback address, do not add as external address to prevent
            // making rendezvous record be like trash.
            if !is_loopback(&address) {
              self.add_external_address(address);
            }
          }
        }
        _ = sleep(Duration::from_secs(1)).fuse() => {
          break;
        }
      }
    }
    Ok(())
  }

  async fn spawn_listeners(mut self) -> Ret<(UnboundedSender<Command>, UnboundedReceiver<Event>)> {
    let (req_input, req_output) = unbounded::<Command>();
    let (resp_input, resp_output) = unbounded::<Event>();

    tokio::spawn(async move {
      let mut fuse = req_output.recv().boxed().fuse();
      loop {
        futures::select! {
          command = fuse => {
            if let Ok(command) = command {
              let shutdown_required = self.handle_command(command).await?;
              if shutdown_required {
                break;
              }
            }
            fuse = req_output.recv().boxed().fuse();
          }
          event = self.swarm.select_next_some() => {
            // TODO Apply event received from swarm
            match event {
              SwarmEvent::Behaviour(event) => self.handle_event(event, &resp_input).await?,
              SwarmEvent::NewListenAddr { address, .. } => {
                if !is_loopback(&address) {
                  self.add_external_address(address);
                }
              }
              SwarmEvent::ConnectionEstablished { endpoint: ConnectedPoint::Dialer { address, .. }  , .. } => {
                self.dialing_relays.remove(&address);
              }
              SwarmEvent::ConnectionClosed { endpoint, .. } => {
                if endpoint.is_relayed() {
                  _ = self.swarm.dial(endpoint.get_remote_address().to_owned());
                }
              }
              e => warn!("swarm event: {e:?}")
            }
          }
        }
      }
      Ok::<_, anyhow::Error>(())
    });

    Ok((req_input, resp_output))
  }

  async fn handle_command(&mut self, command: Command) -> Ret<bool> {
    match command {
      Command::Dial(addr) => {
        info!("dialing to {addr}..");
        self.swarm.dial(Multiaddr::from_str(&addr)?)?;
      }
      Command::BeginBootstrap => {
        if let Some(kad_dht) = self.swarm.behaviour_mut().dht.as_mut() {
          kad_dht.begin_bootstrap();
        }
      }
      Command::LookupPeer(_peer_id) => {}
      Command::AddKnownPeer { peer, address } => {
        if let Some(kad_dht) = self.swarm.behaviour_mut().dht.as_mut() {
          let peer_id = PeerId::from_str(&peer)?;
          let address = Multiaddr::from_str(&address)?;
          info!("adding peer {peer_id} at {address}..");
          kad_dht.add_address(peer_id, address);
        }
      }
      Command::AddBootNode { peer, address } => {
        if let Some(kad_dht) = self.swarm.behaviour_mut().dht.as_mut() {
          let peer = PeerId::from_str(&peer)?;
          let address = Multiaddr::from_str(&address)?;
          kad_dht.add_bootnodes(peer, address);
        }
      }
      Command::PutRecord { key, value } => {
        if let Some(kad_dht) = self.swarm.behaviour_mut().dht.as_mut() {
          let query = KadPutQuery {
            backoff: Default::default(),
            key,
            value,
          };
          kad_dht.put_record(query);
        }
      }
      Command::GetRecord { key, retry } => {
        if let Some(kad_dht) = self.swarm.behaviour_mut().dht.as_mut() {
          kad_dht.get_record(key, Default::default(), retry)
        }
      }
      Command::Shutdown => {
        return Ok(true);
      }
      Command::ListenViaRelay(address) => {
        if let Err(e) = self
          .handle_listen_via_relay(Multiaddr::from_str(&address)?)
          .await
        {
          error!("listen via relay failed: {e}");
        }
      }
      Command::Register { point, namespace } => {
        if let Some(rdvz) = self.swarm.behaviour_mut().rdvz.as_mut() {
          info!("registering on `{namespace}`..");
          rdvz.register(Multiaddr::from_str(&point)?, namespace)?;
        } else {
          warn!("unsupported rendezvous");
        }
      }
      Command::Discover { point, namespace } => {
        if let Some(rdvz) = self.swarm.behaviour_mut().rdvz.as_mut() {
          info!("discovering on `{namespace}`..");
          rdvz.discover(Multiaddr::from_str(&point)?, namespace)?;
        } else {
          warn!("unsupported rendezvous");
        }
      }
      Command::Publish { topic, contents } => {
        self.swarm.behaviour_mut().gossip.publish(topic, contents);
      }
      Command::Subscribe(topic) => {
        info!("subscribing topic `{topic}`..");
        self.swarm.behaviour_mut().gossip.subscribe(topic);
      }
      Command::Unsubscribe(topic) => {
        info!("unsubscribing topic `{topic}`..");
        self.swarm.behaviour_mut().gossip.unsubscribe(topic);
      }
      Command::GetConnectedPeers(_) => {}
      Command::GetConnectedPeersLen(_) => {}
    }
    Ok(false)
  }

  async fn handle_event(
    &mut self,
    event: NodeBehaviourEvent,
    send_to_client: &UnboundedSender<Event>,
  ) -> Ret {
    let maybe_event = match event {
      NodeBehaviourEvent::Identify(_) => {
        // TODO: Handle with DHT.
        None
      }
      NodeBehaviourEvent::Gossip(gossip) => match gossip {
        gossip_bhv::Event::MsgReceived { topic, contents } => {
          Some(Event::MsgReceived { topic, contents })
        }
      },
      NodeBehaviourEvent::Mdns(mdns) => {
        // TODO: Handle expired?
        if let mdns_bhv::Event::Discovered(peers) = mdns {
          let peers = peers.into_iter().map(|p| p.0).collect::<Vec<_>>();
          self.swarm.behaviour_mut().gossip.add_explicit_peers(peers);
        }
        None
      }
      NodeBehaviourEvent::Rdvz(rdvz) => {
        // TODO: Handle other events
        if let rendezvous_bhv::Event::Discovered(discovered) = rdvz {
          for peer in discovered {
            let p2p_suffix = Protocol::P2p(peer.0);
            let address_with_p2p = if !peer
              .1
              .ends_with(&Multiaddr::empty().with(p2p_suffix.clone()))
            {
              peer.1.clone().with(p2p_suffix)
            } else {
              peer.1.clone()
            };
            self.swarm.dial(address_with_p2p)?;
          }
        }
        None
      }
      NodeBehaviourEvent::Dht(_) => None,
      NodeBehaviourEvent::Dcutr(dcutr) => {
        if dcutr.result.is_ok() {
          self
            .swarm
            .behaviour_mut()
            .gossip
            .add_explicit_peers(vec![dcutr.remote_peer_id]);
        }
        None
      }
      _ => None,
    };
    if let Some(event) = maybe_event {
      send_to_client.send(event)?;
    }
    Ok(())
  }

  fn add_external_address(&mut self, addr: Multiaddr) {
    self.swarm.add_external_address(addr);
  }

  async fn handle_listen_via_relay(&mut self, address: Multiaddr) -> Ret {
    info!("connecting to relay {address}..");
    self.swarm.dial(address.clone())?;

    let mut learned_observed_addr = false;
    let mut told_relay_observed_addr = false;

    // TODO: Need configurable timeout for begin of connecting relay?
    tokio::time::timeout(Duration::from_secs(4), async {
      loop {
        match self.swarm.select_next_some().await {
          SwarmEvent::NewListenAddr { .. } => {}
          SwarmEvent::Dialing { .. } => {}
          SwarmEvent::ConnectionEstablished { .. } => {}
          SwarmEvent::Behaviour(NodeBehaviourEvent::Identify(identify::Event::Sent { .. })) => {
            info!("told relay its public address");
            told_relay_observed_addr = true;
          }
          SwarmEvent::Behaviour(NodeBehaviourEvent::Identify(identify::Event::Received {
            info: identify::Info { observed_addr, .. },
            ..
          })) => {
            info!("relay told us our public address: {observed_addr:?}");
            self.swarm.add_external_address(observed_addr);
            learned_observed_addr = true;
          }
          event => debug!("{event:?}"),
        }

        if learned_observed_addr && told_relay_observed_addr {
          break;
        }
      }
    })
    .await
    .map_err(|e| {
      self.dialing_relays.insert(address.clone());
      e
    })?;

    let new_address = address.with(Protocol::P2pCircuit);
    self.swarm.listen_on(new_address.clone())?;
    self.swarm.add_external_address(new_address);

    Ok(())
  }
}

async fn build_swarm(identity: &Keypair, config: &Config) -> Ret<Swarm<NodeBehaviour>> {
  let swarm = SwarmBuilder::with_existing_identity(identity.clone())
    .with_tokio()
    .with_tcp(
      tcp::Config::default().nodelay(true).port_reuse(true),
      noise::Config::new,
      yamux::Config::default,
    )?
    // .with_quic()
    .with_dns()? // TODO: Needed?
    .with_relay_client(noise::Config::new, yamux::Config::default)?
    .with_behaviour(move |key, relay| {
      let peer_id = key.public().to_peer_id();

      let mdns = Toggle::from(if config.enable_mdns() {
        let mdns_cfg = mdns_bhv::Config::default().service_name(config.mdns_service_name());
        let mdns = mdns_bhv::Behaviour::new(mdns_cfg, peer_id)?;
        Some(mdns)
      } else {
        None
      });

      let rdvz = Toggle::from(if config.enable_rendezvous() {
        let rdvz = rendezvous::client::Behaviour::new(key.clone());
        let rdvz = rendezvous_bhv::Behaviour::new(rdvz, peer_id, config.rendezvous_ttl());
        Some(rdvz)
      } else {
        None
      });

      let gossip_cfg = gossipsub::ConfigBuilder::default()
        // TODO: Testing purpose, remove later needed
        .heartbeat_interval(Duration::from_secs(30))
        .validation_mode(gossipsub::ValidationMode::Strict)
        .history_gossip(32)
        .history_length(32)
        .mesh_outbound_min(1)
        .mesh_n_low(1)
        .max_transmit_size(8 * 1024 * 1024)
        .build()?;
      let gossip = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(key.clone()),
        gossip_cfg,
      )?;
      let gossip = gossip_bhv::Behaviour::new(gossip, config.enable_republish());

      let identify_cfg = identify::Config::new("/AumPoS/identify/1.0".to_owned(), key.public());
      let identify = identify::Behaviour::new(identify_cfg);

      let dht = Toggle::from(if config.enable_dht() {
        let mut dht_cfg = kad::Config::default();
        dht_cfg
          .set_parallelism(NonZeroUsize::new(1).unwrap())
          .set_replication_factor(NonZeroUsize::new(3).unwrap());
        let mut dht = kad::Behaviour::with_config(peer_id, MemoryStore::new(peer_id), dht_cfg);
        dht.set_mode(Some(kad::Mode::Server));
        let mut dht = dht_bhv::Behaviour::new(dht, NonZeroUsize::new(1).unwrap());
        for (peer, addr) in config.bootstrap_nodes() {
          dht.add_address(*peer, addr.clone());
          dht.add_bootnodes(*peer, addr.clone());
        }
        Some(dht)
      } else {
        None
      });

      let dcutr = Toggle::from(if config.enable_relay() {
        Some(dcutr::Behaviour::new(peer_id))
      } else {
        None
      });

      let relay = Toggle::from(if config.enable_relay() {
        Some(relay)
      } else {
        None
      });

      Ok(NodeBehaviour {
        identify,
        gossip,
        mdns,
        rdvz,
        dht,
        dcutr,
        relay,
      })
    })?
    .with_swarm_config(|cfg| {
      cfg
        .with_dial_concurrency_factor(NonZeroU8::new(1).unwrap())
        // TODO: Connection timeout is upto 14 days. Need to be configurable?
        .with_idle_connection_timeout(Duration::from_secs(86400 * 14))
    })
    .build();

  Ok(swarm)
}
