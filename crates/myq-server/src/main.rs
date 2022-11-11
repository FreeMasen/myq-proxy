use std::fs::File;

use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, Stream, StreamExt, TryFutureExt,
};
use myq_proxy::{Config, Device, DeviceStateActor, DeviceStateHandle, StateChange};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
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
    let with_handle = warp::any().map(move || handle.clone());
    let event_stream = warp::path("event-stream")
        .and(warp::ws())
        .and(with_handle)
        .map(|ws: Ws, handle: DeviceStateHandle| {
            ws.on_upgrade(|sock| async move {
                let (tx, rx) = sock.split();
                let tx = wrap_ws_in_ch(tx);
                wrap_receiver(rx, tx.clone(), handle.clone());
                forward_event_stream(handle, tx);
            })
        });
    let all = event_stream.with(warp::log("myq-server"));
    tokio::task::spawn(async move {
        warp::serve(all).run(([127, 0, 0, 1], 6578)).await;
    });
    actor.run().await.unwrap();
}

/// Forward all message through an UnboundedReceiver to allow for cloning the tx into
/// multiple places
fn wrap_ws_in_ch(mut tx: SplitSink<WebSocket, Message>) -> UnboundedSender<OutgoingMsg> {
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<OutgoingMsg>();
    tokio::task::spawn(async move {
        while let Some(msg) = reply_rx.recv().await {
            let msg = serde_json::to_string(&msg).unwrap();
            tx.send(Message::text(msg)).await.ok();
        }
    });
    reply_tx
}

fn wrap_receiver(
    mut rx: SplitStream<WebSocket>,
    tx: UnboundedSender<OutgoingMsg>,
    handle: DeviceStateHandle,
) {
    tokio::task::spawn(async move {
        while let Some(msg) = rx.next().await {
            if let Ok(msg) = msg.map_err(|e| log::error!("received error {e}")) {
                if let Ok(msg) = serde_json::from_slice::<IncomingMsg>(msg.as_bytes())
                    .map_err(|e| log::error!("Error deserializing message {e}"))
                {
                    match msg {
                        IncomingMsg::GetDevice(_) => {
                            let message = handle
                                .get_devices()
                                .await
                                .map(|devs| OutgoingMsg::GetDevices(devs))
                                .unwrap_or_else(|e| OutgoingMsg::Error(format!("{e}")));
                            tx.send(message)
                                .map_err(|e| log::error!("Error sending to wrapped ws: {e:?}"))
                                .ok();
                        }
                    }
                }
            }
        }
    });
}

fn forward_event_stream(handle: DeviceStateHandle, tx: UnboundedSender<OutgoingMsg>) {
    tokio::task::spawn(async move {
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
                tx.send(OutgoingMsg::StateChange(msg))
                    .map_err(|e| {
                        log::error!("Error sending msg: {e}");
                    })
                    .ok();
            }
        }
    });
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum IncomingMsg {
    GetDevice(GetDevices),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetDevices;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum OutgoingMsg {
    GetDevices(Vec<Device>),
    StateChange(StateChange),
    Error(String),
}
