use std::{
    collections::HashMap,
    time::{Duration, SystemTime},
};

use reqwest::{
    header::{HeaderMap, HeaderValue},
    redirect::Policy,
    Client, RequestBuilder, Response, Url,
};
use scraper::Selector;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

type Error = Box<dyn std::error::Error>;

const AUTH_URL_BASE: &str = "https://api.myqdevice.com/api/v5";
const DEVICE_URL_BASE: &str = "https://api.myqdevice.com/api/v5.1";
const MYQ_API_CLIENT_ID: &str = "IOS_CGI_MYQ";
const MYQ_API_CLIENT_SECRET: &str = "VUQ0RFhuS3lQV3EyNUJTdw==";
const MYQ_API_REDIRECT_URI: &str = "com.myqops://ios";

pub fn build_client() -> Result<Client, Error> {
    let headers = HeaderMap::new();
    let ret = Client::builder().default_headers(headers).build()?;
    Ok(ret)
}

pub async fn login(client: &Client, config: &Config) -> Result<LongLivedToken, Error> {
    let ret = get_oauth_token(client, config).await?;
    Ok(ret)
}

async fn get_oauth_page(code_challenge: String) -> Result<Response, Error> {
    const URL: &str = "https://partner-identity.myq-cloud.com/connect/authorize";
    let client = Client::builder().build().unwrap();
    let search_params = serde_json::json!({
        "client_id": MYQ_API_CLIENT_ID,
        "code_challenge": code_challenge,
        "code_challenge_method": "S256",
        "redirect_uri": MYQ_API_REDIRECT_URI,
        "response_type": "code",
        "scope": "MyQ_Residential offline_access",
    });
    //println!("{search_params:#?}");
    let req = client
        .get(URL)
        .query(&search_params)
        .header(
            "user-agent",
            HeaderValue::from_maybe_shared("null").unwrap(),
        )
        .build()
        .unwrap();
    //println!("{req:#?}");
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

pub async fn oauth_redirect(login_response: Response) -> Result<Response, Error> {
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

pub async fn oauth_login(auth_page: Response, config: &Config) -> Result<Response, Error> {
    let cookie = trim_cookies(auth_page.headers());
    //println!("{cookie:#?}");
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
    //println!("oauth_login: {req:#?}");
    let res = client.execute(req).await?;
    //println!(
    //     "{:#?}",
    //     res.headers()
    //         .into_iter()
    //         .map(|(k, h)| format!("{k:}: {}", h.to_str().unwrap()))
    //         .collect::<Vec<_>>()
    // );
    assert!(
        res.headers().get_all("set-cookie").iter().count() >= 2,
        "Not enough cookies"
    );
    Ok(res)
}

pub async fn get_oauth_token(client: &Client, config: &Config) -> Result<LongLivedToken, Error> {
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
        "client_id": MYQ_API_CLIENT_ID,
        "client_secret": String::from_utf8_lossy(&base64::decode(&MYQ_API_CLIENT_SECRET).unwrap()),
        "code": code,
        "code_verifier": String::from_utf8_lossy(&code_verify),
        "grant_type": "authorization_code",
        "scope": scope,
        "redirect_uri": MYQ_API_REDIRECT_URI
    });
    //println!("body: {request_body:#?}");
    let req = client
        .post(URL)
        .form(&request_body)
        .header(
            "user-agent",
            HeaderValue::from_maybe_shared("null").unwrap(),
        )
        .build()?;
    //println!("Final login: {req:#?}");
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

pub async fn refresh_token(
    existing: LongLivedToken,
    client: &Client,
) -> Result<LongLivedToken, Error> {
    let body = existing.get_refresh_body();
    let res: RawToken = client
        .post("https://partner-identity.myq-cloud.com/connect/token")
        .form(&body)
        .header(
            "user-agent",
            HeaderValue::from_maybe_shared("null").unwrap(),
        )
        .send()
        .await?
        .json()
        .await?;
    Ok(res.into())
}

pub async fn make_request<T>(request: RequestBuilder) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    use serde_json::Value;
    let res = request.send().await?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap();
        panic!("Error making request: {status}: {body}");
    }
    let ret: Value = res.error_for_status()?.json().await?;
    match serde_json::from_value::<T>(ret.clone()) {
        Ok(ret) => Ok(ret),
        Err(e) => {
            panic!("Error deserializing, got {ret:?}");
        }
    }
}

pub async fn get_accounts(client: &Client, token: &LongLivedToken) -> Result<AccountList, Error> {
    const URL: &str = "https://accounts.myq-cloud.com/api/v6.0/accounts";
    let list = client
        .get(URL)
        .bearer_auth(&token.access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(list)
}

pub async fn get_devices(
    client: &Client,
    token: &LongLivedToken,
    accounts: &AccountList,
) -> Result<HashMap<String, DeviceList>, Error> {
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
            .await?
            .error_for_status()?
            .json()
            .await?;
        ret.insert(acct.id.clone(), res);
    }
    Ok(ret)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Config {
    username: String,
    password: String,
    #[serde(default)]
    security_token: Option<String>,
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
    pub fn get_auth_value(&self) -> Result<HeaderValue, Error> {
        let value = format!("{} {}", self.token_type, self.access_token);
        let ret = HeaderValue::from_maybe_shared(value)?;
        Ok(ret)
    }
    pub fn has_expired(&self) -> bool {
        SystemTime::now() > self.expires_at
    }
    pub fn get_refresh_body(&self) -> serde_json::Value {
        serde_json::json!({
            "client_id": MYQ_API_CLIENT_ID,
            "client_secret": String::from_utf8_lossy(&base64::decode(&MYQ_API_CLIENT_SECRET).unwrap()),
            "grant_type": "refresh_token",
            "redirect_uri": MYQ_API_REDIRECT_URI,
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
    created_by: String,
    id: String,
    max_users: MaxUsers,
    name: String,
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

#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub created_date: String,
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

#[derive(Debug, Clone, Deserialize)]
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
    pub door_state: Option<String>,
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
    pub lamp_state: Option<String>,
    #[serde(default)]
    pub lamp_subtype: Option<String>,
    #[serde(default)]
    pub last_event: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_update: Option<String>,
    #[serde(default)]
    pub learn: Option<String>,
    #[serde(default)]
    pub learn_mode: bool,
    #[serde(default)]
    pub light_state: Option<String>,
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
    #[serde(default)]
    pub updated_date: Option<String>,
    #[serde(default)]
    pub use_aux_relay: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Links {
    events: String,
    stream: String,
}
