use std::time::Duration;

use myq_proxy::{Config, DeviceStateActor};
use serde_json::Value;

#[tokio::main]
async fn main() {
    env_logger::init();
    let raw = include_str!("../../config.json");
    let config: Value = serde_json::from_str(raw).unwrap();
    let config: Config = serde_json::from_value(config.clone())
        .map_err(|e| {
            eprintln!("raw config: {config:#?}");
            e
        })
        .unwrap();
    let (actor, handle) = DeviceStateActor::new(config);
    handle.spawn_refresh_task(Duration::from_secs(2));

    tokio::task::spawn(async move {
        let devices = dbg!(handle.get_devices().await.unwrap());
        handle.toggle_device("CG080057D933".into()).await.unwrap();
        let mut rx = handle.subscribe().await.unwrap();
        while let Ok(change) = rx.recv().await {
            println!("{change:#?}");
        }
    });
    actor.run().await.unwrap();
}
