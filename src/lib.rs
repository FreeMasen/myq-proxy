use std::collections::HashMap;

use reqwest::{
    header::{HeaderMap, HeaderValue},
    redirect::{Policy},
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
    let ret = Client::builder()
        .default_headers(headers)
        .build()?;
    Ok(ret)
}

pub async fn login(client: &Client, config: &Config) -> Result<LoginResponse, Error> {
    get_oauth_token(client, config).await?;
    todo!();
}

async fn get_oauth_page(_client: &Client) -> Result<Response, Error> {
    const URL: &str = "https://partner-identity.myq-cloud.com/connect/authorize";
    let client = Client::builder().build().unwrap();
    let code_verify = pkce::code_verifier(128);
    let code_challenge = pkce::code_challenge(&code_verify);
    let search_params = serde_json::json!({
        "client_id": MYQ_API_CLIENT_ID,
        "code_challenge": code_challenge,
        "code_challenge_method": "S256",
        "redirect_uri": MYQ_API_REDIRECT_URI,
        "response_type": "code",
        "scope": "MyQ_Residential offline_access",
    });
    println!("{search_params:#?}");
    let req = client
        .get(URL)
        .query(&search_params)
        .header(
            "user-agent",
            HeaderValue::from_maybe_shared("null").unwrap(),
        ).build().unwrap();
    println!("{req:#?}");
    let res = client
        .execute(req)
        .await?;
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
    let cookie = trim_cookies(login_response.headers());
    let client = Client::builder().redirect(Policy::none()).build()?;
    let req = client.get(redirect_url).header("cookie", cookie)
        .header("user-agent", "null")
        .build();
    let res = client.execute(req).await?;
    Ok(res)
}

pub async fn oauth_login(auth_page: Response, config: &Config) -> Result<Response, Error> {
    let cookie = trim_cookies(auth_page.headers());
    println!("{cookie:#?}");
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
    let res = client
        .post(auth_page_url)
        .form(&login_body)
        .header(
            "content-type",
            HeaderValue::from_maybe_shared("application/x-www-form-urlencoded").unwrap(),
        )
        .header("cookie", cookie)
        .header(
            "user-agent",
            HeaderValue::from_maybe_shared("null").unwrap(),
        )
        .send()
        .await?;
    println!(
        "{:#?}",
        res.headers()
            .into_iter()
            .map(|(k, h)| format!("{k:}: {}", h.to_str().unwrap()))
            .collect::<Vec<_>>()
    );
    // assert!(res.headers().get_all("set-cookie").iter().count() >= 2, "Not enough cookies");
    Ok(res)
}

pub async fn get_oauth_token(client: &Client, config: &Config) -> Result<(), Error> {
    let oauth_page = get_oauth_page(client).await?;
    let login_res = oauth_login(oauth_page, config).await?;
    let redir_res = oauth_redirect(login_res).await?;
    let redir_url = Url::parse(redir_res.headers().get("location").unwrap().to_str()?)?;
    let query: HashMap<_, _> = redir_url.query_pairs().collect();
    let code = query.get("code").unwrap().to_string();
    let scope = query.get("scope").unwrap().to_string();
    todo!()
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

async fn open(client: &Client) -> Result<(), Error> {
    todo!()
}

async fn close(client: &Client) -> Result<(), Error> {
    todo!()
}

async fn get_state(client: &Client) -> Result<(), Error> {
    todo!()
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
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    data: LoginResponseInner,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponseInner {
    pub security_token: String,
}

#[derive(Debug, Clone)]
pub struct Session {
    security_token: String,
}
