use std::sync::Arc;

use p2pa::{prelude::*, ConfigBuilder, Event, Node};

#[tokio::main]
async fn main() -> Ret {
  env_logger::init();
  let config = ConfigBuilder::default()
    .enable_mdns(true)
    .mdns_service_name("")
    // .enable_rendezvous(true)
    // .enable_relay(true)
    // .enable_dht(true)
    .enable_republish(true)
    .rendezvous_namespaces(vec![String::from("aumpos::global")])
    .rendezvous_ttl(30)
    .build()?;
  let node = Node::init(0usize, config).await?;
  let node = Arc::new(node);
  node.execute(|node, event| async move {
    match event {
      Event::MsgReceived { topic, contents } => match topic.as_str() {
        "test" | "test-2" => {
          let msg = String::from_utf8_lossy(&contents).to_string();
          let counter = node.state().await;
          println!("{msg}_{counter}");
          node.modify_state(|counter| *counter += 1).await;
          node.publish(topic.as_str(), contents)?;
        }
        _ => {}
      },
    }
    Ok(())
  });
  node.spin_up()?;

  node.subscribe("test")?;
  node.publish("test", "Hello")?;
  // node.subscribe("test-2")?;
  // node.publish("test-2", "Goodbye")?;

  let mut buf = String::new();
  _ = std::io::stdin().read_line(&mut buf);
  node.shutdown().await?;
  while !node.is_down() {}
  Ok(())
}
