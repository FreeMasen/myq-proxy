
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
    let login = myq_proxy::login(&client, &config).await.unwrap();
    println!("{login:#?}");
}
