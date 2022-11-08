use std::{time::Duration, path::{Path, PathBuf}};

use myq_proxy::{Config, DeviceStateActor};
use serde_json::Value;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the config file
    #[arg(short, long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Watch all for all state changes (always begins with add events for each device)
    Watch,
    /// Get a snapshot of the devices and their state
    GetDevices,
    /// Set a garage door state
    Garage {
        id: String,
        target: myq_proxy::GarageDoorState,
    },
    /// Set a lamp state
    Lamp {
        id: String,
        target: myq_proxy::LampState
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let args = Args::parse();
    let config = read_config(&args.config).await.unwrap();
    let (actor, handle) = DeviceStateActor::new(config);
    handle.spawn_refresh_task(Duration::from_secs(2));
    
    tokio::task::spawn(async move {
        let mut rx = handle.subscribe().await.unwrap();
        while let Ok(change) = rx.recv().await {
            println!("{change:#?}");
        }
    });
    actor.run().await.unwrap();
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
