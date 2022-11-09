use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use clap::{Parser, Subcommand};
use myq_proxy::{Config, DeviceStateActor, DeviceStateHandle, GarageDoorState, LampState};
use serde_json::Value;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the config file
    #[arg(short, long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Watch all for all state changes (always begins with add events for each device)
    Watch,
    /// Get a snapshot of the devices and their state
    GetDevices {
        #[arg(short, long)]
        table: bool,
    },
    /// Set a garage door state
    Garage {
        /// The device ID from get-devices
        id: String,
        /// The desired state
        target: GarageDoorState,
    },
    /// Set a lamp state
    Lamp {
        /// The device ID from get-devices
        id: String,
        /// The desired state
        target: LampState,
    },
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let args = Args::parse();
    let config = read_config(&args.config).await.unwrap();
    let (actor, handle) = DeviceStateActor::new(config);
    tokio::task::spawn(async move {
        match args.command {
            Command::Watch => watch(handle).await,
            Command::GetDevices { table } => get_devices(handle, table).await,
            Command::Garage { id, target } => garage_command(handle, id, target).await,
            Command::Lamp { id, target } => lamp_command(handle, id, target).await,
        }
    });
    actor.run().await.unwrap();
}

async fn watch(handle: DeviceStateHandle) {
    handle.spawn_refresh_task(Duration::from_secs(2));
    let mut rx = handle.subscribe().await.unwrap();
    while let Ok(change) = rx.recv().await {
        println!("{}", serde_json::to_string(&change).unwrap());
    }
}

async fn get_devices(handle: DeviceStateHandle, table: bool) {
    let devices = handle.get_devices().await.unwrap();
    if table {
        let mut t = comfy_table::Table::new();
        t.set_header(vec!["Device Name", "Device ID", "Device State"]);
        for device in devices {
            t.add_row(vec![
                device.name.as_str(),
                device.serial_number.as_str(),
                device
                    .get_type_state()
                    .map(|s| s.lower_str())
                    .unwrap_or("unknown"),
            ]);
        }
        println!("{t}");
    } else {
        println!("{}", serde_json::to_string(&devices).unwrap());
    }
}

async fn garage_command(handle: DeviceStateHandle, id: String, cmd: GarageDoorState) {
    let device = handle.get_device(id.clone()).await.unwrap();
    let device = match device {
        Some(device) => device,
        None => std::process::exit(1),
    };
    if device
        .get_type_state()
        .map(|state| state.lower_str() == cmd.lower_str())
        .unwrap_or_default()
    {
        return;
    }
    handle.toggle_device(id).await.unwrap();
}

async fn lamp_command(handle: DeviceStateHandle, id: String, cmd: LampState) {
    let device = handle.get_device(id.clone()).await.unwrap();
    let device = match device {
        Some(device) => device,
        None => std::process::exit(1),
    };
    if device
        .get_type_state()
        .map(|state| state.lower_str() == cmd.lower_str())
        .unwrap_or_default()
    {
        return;
    }
    handle.toggle_device(id).await.unwrap();
}

async fn read_config(path: impl AsRef<Path>) -> Result<Config, std::io::Error> {
    let raw = tokio::fs::read_to_string(path).await?;
    let config: Value = serde_json::from_str(&raw).unwrap();
    let config: Config = serde_json::from_value(config.clone())
        .map_err(|e| {
            eprintln!("raw config: {config:#?}");
            e
        })
        .unwrap();
    Ok(config)
}
