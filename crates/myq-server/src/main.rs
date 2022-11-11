use std::fs::File;

use futures_util::{SinkExt, Stream, StreamExt, TryFutureExt, stream::{SplitStream, SplitSink}};
use myq_proxy::{Config, DeviceStateActor};
use serde::{Deserialize, Serialize};
use warp::{
    ws::{Message, WebSocket, Ws},
    Filter,
};

mod discovery;

#[tokio::main]
async fn main() {
    let config_dir = dirs::config_dir().unwrap_or_else(|| {
        panic!("config dir is required");
    });
    let config_dir = config_dir.join("myq-server");
    let config_file_path = config_dir.join("config.json");
    if !config_file_path.exists() {
        panic!("expected a config file at {}", config_file_path.display());
    }
    let mut f = File::open(&config_file_path).unwrap();
    let config: Config = serde_json::from_reader(&mut f).unwrap();
    let (actor, handle) = DeviceStateActor::new(config);
    let event_stream = warp::path("event-stream").and(warp::ws()).map({
        let handle = handle.clone();
        move |ws: Ws| {
            ws.on_upgrade({
                let handle = handle.clone();
                move |sock| async move {
                    let (mut tx, mut rx) = sock.split();
                    tokio::task::spawn(async move {
                        while let Some(msg) = rx.next().await {
                            if let Ok(msg) = msg.map_err(|e| log::error!("received error {e}")) {
                                if let Ok(msg) =
                                    serde_json::from_slice::<IncomingMsg>(msg.as_bytes())
                                        .map_err(|e| log::error!("Error deserializing message {e}"))
                                {

                                }
                            }
                        }
                    });
                    if let Ok(mut rx) = handle
                        .subscribe()
                        .await
                        .map_err(|e| log::error!("Error subscribing: {e}"))
                    {
                        while let Ok(msg) = rx
                            .recv()
                            .await
                            .map_err(|e| log::error!("error receiving {e}"))
                        {
                            if let Ok(json) = serde_json::to_string(&msg)
                                .map_err(|e| log::error!("Error serializing message {e}"))
                            {
                                tx.send(Message::text(json))
                                    .await
                                    .map_err(|e| {
                                        log::error!("Error sending msg: {e}");
                                    })
                                    .ok();
                            }
                        }
                    }
                }
            })
        }
    });
    tokio::task::spawn(async move {
        warp::serve(event_stream.with(warp::log("myq-server")))
            .run(([127, 0, 0, 1], 6578))
            .await;
    });
    actor.run().await.unwrap();
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum IncomingMsg {}
