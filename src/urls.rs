use crate::DeviceType;

#[cfg(not(any(test, feature = "mockito-urls")))]
fn partner_url_base() -> String {
    "https://partner-identity.myq-cloud.com/connect".into()
}

#[cfg(any(test, feature = "mockito-urls"))]
fn partner_url_base() -> String {
    mockito::server_url()
}

pub fn authorize_url() -> String {
    format!("{}/authorize", partner_url_base())
}

pub fn token_url() -> String {
    format!("{}/token", partner_url_base())
}

#[cfg(not(any(test, feature = "mockito-urls")))]
pub fn accounts_url() -> String {
    "https://accounts.myq-cloud.com/api/v6.0/accounts".into()
}
#[cfg(any(test, feature = "mockito-urls"))]
pub fn accounts_url() -> String {
    format!("{}/accounts", mockito::server_url())
}

#[cfg(not(any(test, feature = "mockito-urls")))]
pub fn devices_url(account_id: &str) -> String {
    format!("https://devices.myq-cloud.com/api/v5.2/Accounts/{account_id}/Devices")
}
#[cfg(any(test, feature = "mockito-urls"))]
pub fn devices_url(account_id: &str) -> String {
    format!("{}/Accounts/{account_id}/Devices", mockito::server_url())
}

#[cfg(not(any(test, feature = "mockito-urls")))]
pub fn device_url_base(dt: &DeviceType) -> String {
    match dt {
        DeviceType::Lamp(_) => "https://account-devices-lamp.myq-cloud.com/api/v5.2/Accounts",
        DeviceType::GarageDoor(_) => "https://account-devices-gdo.myq-cloud.com/api/v5.2/Accounts",
    }
    .into()
}

#[cfg(any(test, feature = "mockito-urls"))]
pub fn device_url_base(_dt: &DeviceType) -> String {
    format!("{}/Accounts", mockito::server_url())
}
