use std::time::{Duration, SystemTime};

use myq_proxy::Config;
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
    let client = myq_proxy::build_client().unwrap();
    let mut login = myq_proxy::login(&client, &config).await.unwrap();
    // println!("{login:#?}");
    let accounts = myq_proxy::get_accounts(&client, &login).await.unwrap();
    // println!("{accounts:#?}");
    loop {
        if login.has_expired()
            || login
                .expires_at
                .duration_since(SystemTime::now())
                .map(|d| d < Duration::from_secs(7))
                .unwrap_or(true)
        {
            login = myq_proxy::login(&client, &config).await.unwrap();
        }
        let device_lists = myq_proxy::get_devices(&client, &login, &accounts)
            .await
            .unwrap();
        for (_acct, devices) in device_lists {
            if devices.items.is_empty() {
                continue;
            }
            for device in devices.items {
                if let Some(state) = device.state.door_state {
                    println!("{}: {} ({})", device.name, state, device.state.last_status.unwrap_or_else(||"??".into()));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}
