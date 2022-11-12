use std::{convert::Infallible, fs::File, net::IpAddr};

use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use myq_proxy::{
    Config, Device, DeviceStateActor, DeviceStateHandle, DeviceType, GarageDoorState, LampState,
    StateChange,
};
use serde::{Deserialize, Serialize};
use socket2::Socket;
use tokio::sync::mpsc::{self, UnboundedSender};
use warp::{
    hyper::StatusCode,
    reply::Response,
    ws::{Message, WebSocket, Ws},
    Filter, Reply,
};

mod discovery;

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    let ip = find_my_ip();
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
        .and(with_handle.clone())
        .map(|ws: Ws, handle: DeviceStateHandle| ws.on_upgrade(|sock| on_ws_upgrade(sock, handle)));
    let get_devices = warp::path("devices")
        .and(warp::get())
        .and(with_handle.clone())
        .and_then(|handle: DeviceStateHandle| async move {
            Result::<_, Infallible>::Ok(get_devices(&handle).await.as_reply())
        });
    let get_device = warp::path!("device" / String)
        .and(warp::get())
        .and(with_handle.clone())
        .and_then(|id: String, handle: DeviceStateHandle| async move {
            Result::<_, Infallible>::Ok(get_device(id, &handle).await.as_reply())
        });
    let execute_command = warp::path!("command" / String)
        .and(warp::post())
        .and(warp::body::json())
        .and(with_handle.clone())
        .and_then(
            |id: String, command: Command, handle: DeviceStateHandle| async move {
                Result::<_, Infallible>::Ok(execute_command(id, command, &handle).await.as_reply())
            },
        );
    let all = event_stream
        .or(get_devices)
        .or(get_device)
        .or(execute_command)
        .with(warp::log("myq_server"));
    let (addr, server) = warp::serve(all).bind_ephemeral((ip, 0));
    let ip = match addr.ip() {
        std::net::IpAddr::V4(inner) => inner,
        std::net::IpAddr::V6(_) => panic!("v4 address not yet supported"),
    };
    tokio::task::spawn(discovery::discovery_task(ip, addr.port()));
    tokio::task::spawn(server);
    actor.run().await.unwrap();
}

async fn on_ws_upgrade(sock: WebSocket, handle: DeviceStateHandle) {
    let (tx, rx) = sock.split();
    let tx = wrap_ws_in_ch(tx);
    wrap_receiver(rx, tx.clone(), handle.clone());
    forward_event_stream(handle, tx);
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
            let msg = process_incoming(msg, &handle).await.unwrap_or_else(|e| e);
            tx.send(msg)
                .map_err(|e| log::error!("Error sending to wrapped ws: {e:?}"))
                .ok();
        }
    });
}

async fn process_incoming(
    incoming: Result<Message, warp::Error>,
    handle: &DeviceStateHandle,
) -> Result<OutgoingMsg, OutgoingMsg> {
    log::trace!("incoming msg: {incoming:?}");
    let msg = incoming.map_err(|e| {
        log::error!("received error {e}");
        e
    })?;
    let msg = serde_json::from_slice::<IncomingMsg>(msg.as_bytes()).map_err(|e| {
        log::error!("Error deserializing message {e}");
        e
    })?;
    Ok(process_incoming_message(msg, handle).await)
}

async fn process_incoming_message(msg: IncomingMsg, handle: &DeviceStateHandle) -> OutgoingMsg {
    match msg {
        IncomingMsg::GetDevices => get_devices(handle).await,
        IncomingMsg::GetDevice { device_id } => get_device(device_id, handle).await,
        IncomingMsg::ExecuteCommand { device_id, command } => {
            execute_command(device_id, command, handle).await
        }
    }
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
                tx.send(OutgoingMsg::StateChange { change: msg })
                    .map_err(|e| {
                        log::error!("Error sending msg: {e}");
                    })
                    .ok();
            }
        }
    });
}

async fn get_devices(handle: &DeviceStateHandle) -> OutgoingMsg {
    handle
        .get_devices()
        .await
        .map(|devices| OutgoingMsg::GetDevices { devices })
        .unwrap_or_else(|e| OutgoingMsg::Error {
            err: format!("{e}"),
        })
}

async fn get_device(device_id: String, handle: &DeviceStateHandle) -> OutgoingMsg {
    match handle.get_device(device_id).await {
        Ok(Some(device)) => OutgoingMsg::GetDevice { device },
        Ok(None) => OutgoingMsg::Error {
            err: "Device Not Found".into(),
        },
        Err(e) => OutgoingMsg::Error {
            err: format!("{e}"),
        },
    }
}

async fn execute_command(id: String, command: Command, handle: &DeviceStateHandle) -> OutgoingMsg {
    try_execute_command(id, command, handle.clone())
        .await
        .map(|_| OutgoingMsg::Success)
        .unwrap_or_else(|err| OutgoingMsg::Error { err })
}

async fn try_execute_command(
    id: String,
    command: Command,
    handle: DeviceStateHandle,
) -> Result<(), String> {
    let device = handle
        .get_device(id.clone())
        .await
        .map_err(|e| format!("{e:?}"))?
        .ok_or_else(|| format!("Unknown Device {id}"))?;
    match (device.get_type_state(), &command) {
        (Some(DeviceType::Lamp(LampState::Off)), Command::On)
        | (Some(DeviceType::Lamp(LampState::On)), Command::Off) => {
            handle
                .toggle_device(id)
                .await
                .map_err(|e| format!("{e:?}"))
                .ok();
        }
        (Some(DeviceType::GarageDoor(GarageDoorState::Open)), Command::Close)
        | (Some(DeviceType::GarageDoor(GarageDoorState::Closed)), Command::Open) => {
            handle
                .toggle_device(id)
                .await
                .map_err(|e| format!("{e:?}"))
                .ok();
        }
        (None, _) => return Err(format!("Device State not found")),
        _ => {
            return Err(format!(
                "Invalid command for {}: {:?}",
                device.device_family, command
            ))
        }
    }
    Ok(())
}

impl OutgoingMsg {
    fn as_reply(&self) -> impl Reply {
        let status = if matches!(self, OutgoingMsg::Error { .. }) {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::OK
        };
        warp::reply::with_status(warp::reply::json(&self), status)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum IncomingMsg {
    GetDevices,
    GetDevice { device_id: String },
    ExecuteCommand { device_id: String, command: Command },
}

#[derive(Debug, Serialize, Deserialize)]
enum Command {
    On,
    Off,
    Open,
    Close,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum OutgoingMsg {
    GetDevices { devices: Vec<Device> },
    GetDevice { device: Device },
    StateChange { change: StateChange },
    Success,
    Error { err: String },
}

impl From<warp::Error> for OutgoingMsg {
    fn from(e: warp::Error) -> Self {
        Self::Error {
            err: format!("{e}"),
        }
    }
}

impl From<serde_json::Error> for OutgoingMsg {
    fn from(e: serde_json::Error) -> Self {
        Self::Error {
            err: format!("{e}"),
        }
    }
}

fn find_my_ip() -> IpAddr {
    use socket2::{Domain, SockAddr, Type};
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

    let sock_addr = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 1), 0);
    let sock_addr = SocketAddr::V4(sock_addr);
    let sock_addr = SockAddr::from(sock_addr);
    let udp = Socket::new(Domain::IPV4, Type::DGRAM, None).unwrap();
    udp.connect(&sock_addr).unwrap();
    let peer_addr = udp.local_addr().unwrap();
    IpAddr::V4(*peer_addr.as_socket_ipv4().unwrap().ip())
}

#[cfg(test)]
mod test {
    use myq_proxy::DeviceState;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn incoming_msg_ser() {
        let incoming = vec![
            IncomingMsg::GetDevices,
            IncomingMsg::GetDevice {
                device_id: String::new(),
            },
            IncomingMsg::ExecuteCommand {
                command: Command::Close,
                device_id: String::new(),
            },
        ];
        insta::assert_json_snapshot!(incoming);
    }
    #[test]
    fn outgoing_msg_ser() {
        let outgoing = vec![
            OutgoingMsg::GetDevices { devices: vec![] },
            OutgoingMsg::GetDevice {
                device: Device {
                    account_id: Uuid::from_u128(0),
                    created_date: None,
                    device_family: String::new(),
                    device_model: String::new(),
                    device_platform: String::new(),
                    device_type: String::new(),
                    href: String::new(),
                    name: String::new(),
                    parent_device_id: String::new(),
                    serial_number: String::new(),
                    state: DeviceState {
                        attached_work_light_error_present: false,
                        aux_relay_behavior: None,
                        aux_relay_delay: None,
                        close: None,
                        command_channel_report_status: Default::default(),
                        control_from_browser: Default::default(),
                        door_ajar_interval: Default::default(),
                        door_state: Some(GarageDoorState::Closed),
                        dps_low_battery_mode: Default::default(),
                        firmware_version: Default::default(),
                        gdo_lock_connected: Default::default(),
                        homekit_capable: Default::default(),
                        homekit_enabled: Default::default(),
                        invalid_credential_window: Default::default(),
                        invalid_shutout_period: Default::default(),
                        is_unattended_close_allowed: Default::default(),
                        is_unattended_open_allowed: Default::default(),
                        lamp_state: Some(LampState::Off),
                        lamp_subtype: Default::default(),
                        last_event: Default::default(),
                        last_status: Default::default(),
                        last_update: Default::default(),
                        learn: Default::default(),
                        learn_mode: Default::default(),
                        light_state: Default::default(),
                        links: Default::default(),
                        max_invalid_attempts: Default::default(),
                        online: Default::default(),
                        online_change_time: Default::default(),
                        open: Default::default(),
                        passthrough_interval: Default::default(),
                        pending_bootload_abandoned: Default::default(),
                        physical_devices: Default::default(),
                        report_ajar: Default::default(),
                        report_forced: Default::default(),
                        rex_fires_door: Default::default(),
                        servers: Default::default(),
                        updated_date: Default::default(),
                        use_aux_relay: Default::default(),
                    },
                },
            },
            OutgoingMsg::StateChange {
                change: StateChange::DeviceAdded {
                    device_id: String::new(),
                    device_type: DeviceType::GarageDoor(GarageDoorState::Closed),
                },
            },
            OutgoingMsg::Success,
            OutgoingMsg::Error { err: String::new() },
        ];
        insta::assert_json_snapshot!(outgoing);
    }
}
