use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime}, str::FromStr,
};

use reqwest::{
    header::{HeaderMap, HeaderValue, InvalidHeaderValue},
    redirect::Policy,
    Client, Response, Url,
};
use scraper::Selector;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

type Result<T = ()> = std::result::Result<T, Error>;
type Reply<T> = oneshot::Sender<Result<T>>;

const CLIENT_ID: &str = "IOS_CGI_MYQ";
const CLIENT_SECRET: &str = "VUQ0RFhuS3lQV3EyNUJTdw==";
const REDIRECT_URI: &str = "com.myqops://ios";

pub struct DeviceStateActor {
    token: LongLivedToken,
    devices: HashMap<String, Device>,
    rx: mpsc::Receiver<Request>,
    client: Client,
    config: Config,
    last_update: Instant,
    tx: broadcast::Sender<StateChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum StateChange {
    DeviceAdded {
        device_id: String,
        device_type: DeviceType,
    },
    DeviceUpdate {
        device_id: String,
        device_type: DeviceType,
    },
    DeviceRemoved {
        device_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind", content = "state")]
pub enum DeviceType {
    Lamp(LampState),
    GarageDoor(GarageDoorState),
}

impl DeviceType {
    fn toggle(self) -> Self {
        match self {
            Self::Lamp(state) => Self::Lamp(match state {
                LampState::On => LampState::Off,
                LampState::Off => LampState::On,
            }),
            Self::GarageDoor(state) => Self::GarageDoor(match state {
                GarageDoorState::Open => GarageDoorState::Closed,
                GarageDoorState::Opening => GarageDoorState::Closed,
                GarageDoorState::Closed => GarageDoorState::Open,
                GarageDoorState::Closing => GarageDoorState::Open,
            }),
        }
    }
    pub fn lower_str(&self) -> &'static str {
        match self {
            Self::Lamp(state) => state.lower_str(),
            Self::GarageDoor(state) => state.lower_str(),
        }
    }
    pub fn url_base(&self) -> &'static str {
        match self {
            DeviceType::Lamp(_) => "https://account-devices-lamp.myq-cloud.com/api/v5.2/Accounts",
            DeviceType::GarageDoor(_) => {
                "https://account-devices-gdo.myq-cloud.com/api/v5.2/Accounts"
            }
        }
    }
    pub fn url_segment(&self) -> &'static str {
        match self {
            DeviceType::Lamp(_) => "lamps",
            DeviceType::GarageDoor(_) => "door_openers",
        }
    }
}

pub struct DeviceStateHandle(mpsc::Sender<Request>);

pub enum Request {
    GetAllDevices(Reply<Vec<Device>>),
    GetDevice(String, Reply<Option<Device>>),
    ForceRefresh(Reply<()>),
    Subscribe(Reply<broadcast::Receiver<StateChange>>),
    ToggleDevice(String, Reply<()>),
}

impl DeviceStateActor {
    pub fn new(config: Config) -> (Self, DeviceStateHandle) {
        let (tx, rx) = mpsc::channel(512);
        let handle = DeviceStateHandle(tx);
        let (tx, _rx) = broadcast::channel(512);
        std::mem::forget(_rx);
        let ret = Self {
            client: Client::default(),
            config,
            devices: HashMap::new(),
            last_update: Instant::now() - Duration::from_secs(30),
            rx,
            token: LongLivedToken {
                access_token: String::new(),
                expires_at: SystemTime::now(),
                refresh_token: String::new(),
                scope: String::new(),
                token_type: String::new(),
            },
            tx,
        };
        (ret, handle)
    }

    pub fn spawn_refresh_task(tx: mpsc::Sender<Request>, interval: Duration) {
        tokio::task::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let (s, r) = oneshot::channel();
                tx.send(Request::ForceRefresh(s)).await.ok();
                r.await.ok();
            }
        });
    }

    pub async fn run(mut self) -> Result {
        self.init_token().await?;
        while let Some(request) = self.rx.recv().await {
            self.process_request(request).await;
        }
        Ok(())
    }

    async fn process_request(&mut self, request: Request) {
        match request {
            Request::GetAllDevices(reply) => {
                let res = self.get_all_devices().await;
                reply.send(res).ok();
            }
            Request::GetDevice(id, reply) => {
                if self.should_refresh_devices() {
                    if let Err(e) = self.refresh_devices().await {
                        reply.send(Err(e)).ok();
                        return;
                    }
                }
                let device = self.devices.get(&id).cloned();
                reply.send(Ok(device)).ok();
            }
            Request::ForceRefresh(reply) => {
                let ret = self.refresh_devices().await;
                reply.send(ret).ok();
            }
            Request::Subscribe(reply) => {
                reply.send(Ok(self.tx.subscribe())).ok();
            }
            Request::ToggleDevice(id, reply) => {
                let ret = self.toggle_device(id).await;
                reply.send(ret).ok();
            }
        }
    }

    async fn get_all_devices(&mut self) -> Result<Vec<Device>> {
        self.refresh_token().await?;
        if self.should_refresh_devices() {
            self.refresh_devices().await?;
        }
        Ok(self.devices.values().cloned().collect())
    }

    async fn refresh_devices(&mut self) -> Result {
        if Instant::now().duration_since(self.last_update) < Duration::from_secs(2) {
            return Ok(());
        }
        self.refresh_token().await?;
        let accounts = get_accounts(&self.client, &self.token).await?;
        let device_list = get_devices(&self.client, &self.token, &accounts).await?;
        let mut updated_map = HashMap::new();
        for (_, acct_devices) in device_list {
            for device in acct_devices.items {
                if let Some(entry) = self.devices.get(&device.serial_number) {
                    if entry.get_type_state() != device.get_type_state() {
                        if let Some(device_type) = device.get_type_state() {
                            self.send_state_change(StateChange::DeviceUpdate {
                                device_id: device.serial_number.clone(),
                                device_type,
                            });
                        }
                    }
                } else if let Some(device_type) = device.get_type_state() {
                    self.send_state_change(StateChange::DeviceAdded {
                        device_id: device.serial_number.clone(),
                        device_type,
                    });
                }
                updated_map.insert(device.serial_number.clone(), device);
            }
        }
        for k in self.devices.keys() {
            if !updated_map.contains_key(k) {
                self.send_state_change(StateChange::DeviceRemoved {
                    device_id: k.clone(),
                });
            }
        }
        self.devices = updated_map;
        self.last_update = Instant::now();
        Ok(())
    }

    async fn toggle_device(&self, id: String) -> Result {
        if let Some(device) = self.devices.get(&id) {
            if let Some(state) = device.get_type_state() {
                let to_send = state.toggle();
                let url = format!(
                    "{}/{}/{}/{}/{}",
                    to_send.url_base(),
                    device.account_id,
                    to_send.url_segment(),
                    device.serial_number,
                    to_send.lower_str()
                );
                let resp = self.client
                    .put(&url)
                    .bearer_auth(&self.token.access_token)
                    .header("content-length", "0")
                    .send()
                    .await?;
                if !resp.status().is_success() {
                    log::error!("Error changing state {} {}", resp.status(), resp.text().await.unwrap());
                }
            }
        }
        Ok(())
    }

    fn send_state_change(&self, ev: StateChange) {
        log::trace!("send_state_change: {ev:?}");
        self.tx
            .send(ev)
            .map_err(|e| {
                log::error!("Failed to send event: {e:?}");
            })
            .ok();
    }

    async fn init_token(&mut self) -> Result {
        let updated = login(&self.client, &self.config).await?;
        self.token = updated;
        Ok(())
    }

    async fn refresh_token(&mut self) -> Result {
        if !self.should_refresh_token() {
            return Ok(());
        }
        let updated = refresh_token(&self.token, &self.client).await?;
        self.token = updated;
        Ok(())
    }

    fn should_refresh_token(&self) -> bool {
        self.token.has_expired()
            || self
                .token
                .expires_at
                .duration_since(SystemTime::now())
                .map(|d| d <= Duration::from_secs(7))
                .unwrap_or(true)
    }

    fn should_refresh_devices(&self) -> bool {
        Instant::now().duration_since(self.last_update)
            > Duration::from_secs(self.config.state_refresh_s.unwrap_or(5))
    }
}

impl DeviceStateHandle {
    pub fn spawn_refresh_task(&self, interval: Duration) {
        DeviceStateActor::spawn_refresh_task(self.0.clone(), interval);
    }
    pub async fn get_devices(&self) -> Result<Vec<Device>> {
        let (s, r) = oneshot::channel();
        self.0
            .send(Request::GetAllDevices(s))
            .await
            .map_err(|_| Error::SendFailure)?;
        let ret = r.await?;
        ret
    }
    pub async fn get_device(&self, device_id: String) -> Result<Option<Device>> {
        let (s, r) = oneshot::channel();
        self.0
            .send(Request::GetDevice(device_id, s))
            .await
            .map_err(|_| Error::SendFailure)?;
        let ret = r.await?;
        ret
    }
    pub async fn force_refresh(&self) -> Result {
        let (s, r) = oneshot::channel();
        self.0
            .send(Request::ForceRefresh(s))
            .await
            .map_err(|_| Error::SendFailure)?;
        let ret = r.await?;
        ret
    }
    pub async fn subscribe(&self) -> Result<broadcast::Receiver<StateChange>> {
        let (s, r) = oneshot::channel();
        self.0
            .send(Request::Subscribe(s))
            .await
            .map_err(|_| Error::SendFailure)?;
        let ret = r.await?;
        ret
    }
    pub async fn toggle_device(&self, id: String) -> Result {
        let (s, r) = oneshot::channel();
        self.0
            .send(Request::ToggleDevice(id, s))
            .await
            .map_err(|_| Error::SendFailure)?;
        let ret = r.await?;
        ret
    }
}

pub fn build_client() -> Result<Client> {
    let headers = HeaderMap::new();
    let ret = Client::builder().default_headers(headers).build()?;
    Ok(ret)
}

pub async fn login(client: &Client, config: &Config) -> Result<LongLivedToken> {
    let ret = get_oauth_token(client, config).await?;
    Ok(ret)
}

async fn get_oauth_page(code_challenge: String) -> Result<Response> {
    const URL: &str = "https://partner-identity.myq-cloud.com/connect/authorize";
    let client = Client::builder().build().unwrap();
    let search_params = serde_json::json!({
        "client_id": CLIENT_ID,
        "code_challenge": code_challenge,
        "code_challenge_method": "S256",
        "redirect_uri": REDIRECT_URI,
        "response_type": "code",
        "scope": "MyQ_Residential offline_access",
    });

    let req = client
        .get(URL)
        .query(&search_params)
        .header(
            "user-agent",
            HeaderValue::from_maybe_shared("null").unwrap(),
        )
        .build()
        .unwrap();

    let res = client.execute(req).await?;
    Ok(res)
}

fn trim_cookies(headers: &HeaderMap) -> HeaderValue {
    let cookies: Vec<String> = headers
        .get_all("set-cookie")
        .into_iter()
        .filter_map(|h| h.to_str().ok()?.split(";").map(|s| s.to_string()).next())
        .collect();
    let cookie = cookies.join("; ");
    HeaderValue::from_maybe_shared(cookie).unwrap()
}

pub async fn oauth_redirect(login_response: Response) -> Result<Response> {
    let redirect_url = login_response.headers().get("location").unwrap().to_str()?;
    let redirect_url = login_response.url().join(redirect_url)?;
    let cookie = trim_cookies(login_response.headers());
    let client = Client::builder().redirect(Policy::none()).build()?;

    let req = client
        .get(redirect_url)
        .header("cookie", cookie)
        .header("user-agent", "null")
        .build()?;
    let res = client.execute(req).await?;
    Ok(res)
}

pub async fn oauth_login(auth_page: Response, config: &Config) -> Result<Response> {
    let cookie = trim_cookies(auth_page.headers());

    let auth_page_url = auth_page.url().clone();
    let html_text = auth_page.text().await?;
    let login_page_html = scraper::Html::parse_document(&html_text);
    let input = login_page_html
        .select(&Selector::parse("input[name=__RequestVerificationToken]").unwrap())
        .next()
        .unwrap();
    let request_verification_token = input.value().attr("value").unwrap().to_string();
    let login_body = serde_json::json!({
        "Email": config.username,
        "Password": config.password,
        "__RequestVerificationToken": request_verification_token,
    });
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()?;
    let req = client
        .post(auth_page_url)
        .form(&login_body)
        .header("cookie", cookie)
        .header(
            "user-agent",
            HeaderValue::from_maybe_shared("null").unwrap(),
        )
        .build()?;

    let res = client.execute(req).await?;

    assert!(
        res.headers().get_all("set-cookie").iter().count() >= 2,
        "Not enough cookies"
    );
    Ok(res)
}

pub async fn get_oauth_token(client: &Client, config: &Config) -> Result<LongLivedToken> {
    const URL: &str = "https://partner-identity.myq-cloud.com/connect/token";
    let code_verify = pkce::code_verifier(43);
    let code_challenge = pkce::code_challenge(&code_verify);
    let oauth_page = get_oauth_page(code_challenge).await?;
    let login_res = oauth_login(oauth_page, config).await?;
    let redir_res = oauth_redirect(login_res).await?;
    let redir_url = Url::parse(redir_res.headers().get("location").unwrap().to_str()?)?;
    let query: HashMap<_, _> = redir_url.query_pairs().collect();
    let code = query.get("code").unwrap().to_string();
    let scope = query.get("scope").unwrap().to_string();
    let request_body = serde_json::json!({
        "client_id": CLIENT_ID,
        "client_secret": String::from_utf8_lossy(&base64::decode(&CLIENT_SECRET).unwrap()),
        "code": code,
        "code_verifier": String::from_utf8_lossy(&code_verify),
        "grant_type": "authorization_code",
        "scope": scope,
        "redirect_uri": REDIRECT_URI
    });
    let req = client
        .post(URL)
        .form(&request_body)
        .header(
            "user-agent",
            HeaderValue::from_maybe_shared("null").unwrap(),
        )
        .build()?;
    let res = client.execute(req).await?;
    if !res.status().is_success() {
        panic!(
            "Error requesting login: {} {}",
            res.status(),
            res.text().await.unwrap()
        );
    }
    let token: RawToken = res.json().await?;
    Ok(token.into())
}

pub async fn refresh_token(existing: &LongLivedToken, client: &Client) -> Result<LongLivedToken> {
    let body = existing.get_refresh_body();
    let res: RawToken = client
        .post("https://partner-identity.myq-cloud.com/connect/token")
        .form(&body)
        .header(
            "user-agent",
            HeaderValue::from_maybe_shared("null").unwrap(),
        )
        .send()
        .await
        .map_err(|e| {
            log::error!("Failed to get response for refresh token {e}");
            e
        })?
        .json()
        .await
        .map_err(|e| {
            log::error!("Failed to parse json response for refresh token: {e}");
            e
        })?;
    Ok(res.into())
}

pub async fn get_accounts(client: &Client, token: &LongLivedToken) -> Result<AccountList> {
    log::trace!("get_accounts");
    const URL: &str = "https://accounts.myq-cloud.com/api/v6.0/accounts";
    let list = client
        .get(URL)
        .bearer_auth(&token.access_token)
        .send()
        .await
        .map_err(|e| {
            log::error!("Failed to get response for accounts {e}");
            e
        })?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| {
            log::error!("Failed to parse json response for accounts: {e}");
            e
        })?;
    log::trace!("got accounts");
    Ok(list)
}

pub async fn get_devices(
    client: &Client,
    token: &LongLivedToken,
    accounts: &AccountList,
) -> Result<HashMap<String, DeviceList>> {
    log::trace!("get_devices");
    let mut ret = HashMap::new();
    for acct in &accounts.accounts {
        let url = format!(
            "https://devices.myq-cloud.com/api/v5.2/Accounts/{}/Devices",
            acct.id
        );
        let res: DeviceList = client
            .get(url)
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|e| {
                log::error!("Failed to get response for devices {e}");
                e
            })?
            .error_for_status()?
            .json()
            .await
            .map_err(|e| {
                log::error!("Failed to parse json response for devices: {e}");
                e
            })?;
        ret.insert(acct.id.clone(), res);
        log::trace!("got devices for account: {}", acct.id);
    }
    Ok(ret)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Config {
    username: String,
    password: String,
    #[serde(default)]
    state_refresh_s: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawToken {
    pub access_token: String,
    pub expires_in: f64,
    pub refresh_token: String,
    pub scope: String,
    pub token_type: String,
}

#[derive(Debug)]
pub struct LongLivedToken {
    pub access_token: String,
    pub expires_at: SystemTime,
    refresh_token: String,
    scope: String,
    token_type: String,
}

impl From<RawToken> for LongLivedToken {
    fn from(other: RawToken) -> Self {
        let expires_at = SystemTime::now() + Duration::from_secs_f64(other.expires_in);
        Self {
            access_token: other.access_token,
            expires_at,
            refresh_token: other.refresh_token,
            scope: other.scope,
            token_type: other.token_type,
        }
    }
}
impl LongLivedToken {
    pub fn get_auth_value(&self) -> Result<HeaderValue> {
        let value = format!("{} {}", self.token_type, self.access_token);
        let ret = HeaderValue::from_maybe_shared(value)?;
        Ok(ret)
    }
    pub fn has_expired(&self) -> bool {
        SystemTime::now() > self.expires_at
    }
    pub fn get_refresh_body(&self) -> serde_json::Value {
        serde_json::json!({
            "client_id": CLIENT_ID,
            "client_secret": String::from_utf8_lossy(&base64::decode(&CLIENT_SECRET).unwrap()),
            "grant_type": "refresh_token",
            "redirect_uri": REDIRECT_URI,
            "refresh_token": self.refresh_token,
            "scope": self.scope,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountList {
    pub accounts: Vec<Account>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub created_by: String,
    pub id: String,
    pub max_users: MaxUsers,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MaxUsers {
    pub co_owner: u64,
    pub guest: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceList {
    #[serde(default)]
    pub count: usize,
    #[serde(default)]
    pub href: Option<String>,
    #[serde(default)]
    pub items: Vec<Device>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Device {
    #[serde(default)]
    pub account_id: Uuid,
    #[serde(default)]
    pub created_date: Option<String>,
    #[serde(default)]
    pub device_family: String,
    #[serde(default)]
    pub device_model: String,
    #[serde(default)]
    pub device_platform: String,
    #[serde(default)]
    pub device_type: String,
    #[serde(default)]
    pub href: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub parent_device_id: String,
    #[serde(default)]
    pub serial_number: String,
    pub state: DeviceState,
}

impl Device {
    pub fn get_type_state(&self) -> Option<DeviceType> {
        let ret = if self.device_family.contains("garagedoor") {
            DeviceType::GarageDoor(self.state.door_state?)
        } else if self.device_family.contains("lamp") {
            DeviceType::Lamp(self.state.lamp_state?)
        } else {
            return None;
        };
        Some(ret)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeviceState {
    #[serde(default)]
    pub attached_work_light_error_present: bool,
    #[serde(default)]
    pub aux_relay_behavior: Option<String>,
    #[serde(default)]
    pub aux_relay_delay: Option<String>,
    #[serde(default)]
    pub close: Option<String>,
    #[serde(default)]
    pub command_channel_report_status: bool,
    #[serde(default)]
    pub control_from_browser: bool,
    #[serde(default)]
    pub door_ajar_interval: Option<String>,
    #[serde(default)]
    pub door_state: Option<GarageDoorState>,
    #[serde(default)]
    pub dps_low_battery_mode: bool,
    #[serde(default)]
    pub firmware_version: Option<String>,
    #[serde(default)]
    pub gdo_lock_connected: bool,
    #[serde(default)]
    pub homekit_capable: bool,
    #[serde(default)]
    pub homekit_enabled: bool,
    #[serde(default)]
    pub invalid_credential_window: Option<String>,
    #[serde(default)]
    pub invalid_shutout_period: Option<String>,
    #[serde(default)]
    pub is_unattended_close_allowed: bool,
    #[serde(default)]
    pub is_unattended_open_allowed: bool,
    #[serde(default)]
    pub lamp_state: Option<LampState>,
    #[serde(default)]
    pub lamp_subtype: Option<String>,
    #[serde(default)]
    pub last_event: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_status: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_update: Option<OffsetDateTime>,
    #[serde(default)]
    pub learn: Option<String>,
    #[serde(default)]
    pub learn_mode: bool,
    #[serde(default)]
    pub light_state: Option<LampState>,
    #[serde(default)]
    pub links: Option<Links>,
    #[serde(default)]
    pub max_invalid_attempts: usize,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub online_change_time: Option<String>,
    #[serde(default)]
    pub open: Option<String>,
    #[serde(default)]
    pub passthrough_interval: Option<String>,
    #[serde(default)]
    pub pending_bootload_abandoned: bool,
    #[serde(default)]
    pub physical_devices: Vec<serde_json::Value>,
    #[serde(default)]
    pub report_ajar: bool,
    #[serde(default)]
    pub report_forced: bool,
    #[serde(default)]
    pub rex_fires_door: bool,
    #[serde(default)]
    pub servers: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub updated_date: Option<OffsetDateTime>,
    #[serde(default)]
    pub use_aux_relay: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LampState {
    On,
    Off,
}

impl LampState {
    pub fn lower_str(&self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

impl FromStr for LampState {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let ret = match s.trim().to_lowercase().as_str() {
            "on" => Self::On,
            "off" => Self::Off,
            _ => return Err(Error::UnknownState(s.into()))
        };
        Ok(ret)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GarageDoorState {
    Open,
    Opening,
    Closed,
    Closing,
}

impl FromStr for GarageDoorState {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        let ret = match s.trim().to_lowercase().as_str() {
            "open" | "opened" | "opening" => Self::Open,
            "close" | "closed" | "closing" => Self::Closed,
            _ => return Err(Error::UnknownState(s.to_string()))
        };
        return Ok(ret);
    }
}

impl GarageDoorState {
    pub fn lower_str(&self) -> &'static str {
        match self {
            GarageDoorState::Open => "open",
            GarageDoorState::Opening => "opening",
            GarageDoorState::Closed => "close",
            GarageDoorState::Closing => "closing",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Links {
    pub events: String,
    pub stream: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    InvalidHeaderValue(#[from] InvalidHeaderValue),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    HeaderStr(#[from] http::header::ToStrError),
    #[error(transparent)]
    OneshotReceive(#[from] oneshot::error::RecvError),
    #[error("Failed to send from handle")]
    SendFailure,
    #[error("Unknown Device: {0}")]
    UnknownDevice(String),
    #[error("Unknown State: {0}")]
    UnknownState(String),
}
