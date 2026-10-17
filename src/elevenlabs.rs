//! Bounded ElevenLabs synthesis. The response is buffered before playback;
//! dropping the future cancels local waiting, not remote billing.
use crate::{conversation::load_provider_settings, providers::ProviderError};
use reqwest::{Client, Url, header};
use serde::Serialize;
use std::{net::IpAddr, sync::Arc, time::Duration};

pub const DEFAULT_VOICE_ID: &str = "JBFqnCBsd6RMkjVDRZzb";
pub const MODEL: &str = "eleven_flash_v2_5";
pub const MAX_TEXT_BYTES: usize = 8192;
pub const MAX_AUDIO_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct ElevenLabsClient {
    client: Client,
    endpoint: Url,
    deadline: Duration,
    permits: Arc<tokio::sync::Semaphore>,
}
impl ElevenLabsClient {
    pub fn new(api_key: String, voice_id: &str) -> Result<Self, ProviderError> {
        if voice_id.is_empty()
            || voice_id.len() > 128
            || !voice_id.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        Self::build(
            api_key,
            &format!("https://api.elevenlabs.io/v1/text-to-speech/{voice_id}/stream"),
            MAX_DEADLINE,
            false,
        )
    }
    pub fn from_env() -> Result<Option<Self>, ProviderError> {
        let settings = load_provider_settings().map_err(|_| ProviderError::InvalidConfiguration)?;
        let Some(key) = settings
            .get("ELEVENLABS_API_KEY")
            .filter(|key| !key.is_empty())
        else {
            return Ok(None);
        };
        Self::new(
            key.clone(),
            settings
                .get("ELEVENLABS_VOICE_ID")
                .map(String::as_str)
                .unwrap_or(DEFAULT_VOICE_ID),
        )
        .map(Some)
    }
    /// Full endpoint injection is restricted to literal loopback HTTP for tests.
    pub fn loopback_for_tests(
        api_key: String,
        endpoint: &str,
        deadline: Duration,
    ) -> Result<Self, ProviderError> {
        Self::build(api_key, endpoint, deadline, true)
    }
    fn build(
        api_key: String,
        endpoint: &str,
        deadline: Duration,
        loopback: bool,
    ) -> Result<Self, ProviderError> {
        if api_key.is_empty()
            || api_key.len() > 4096
            || !api_key.bytes().all(|b| (33..=126).contains(&b))
            || endpoint.len() > 2048
            || endpoint
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
            || deadline.is_zero()
            || deadline > MAX_DEADLINE
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        let mut endpoint = Url::parse(endpoint).map_err(|_| ProviderError::InvalidConfiguration)?;
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        if loopback {
            let ip = endpoint
                .host_str()
                .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok());
            if endpoint.scheme() != "http" || !ip.is_some_and(|ip| ip.is_loopback()) {
                return Err(ProviderError::InvalidConfiguration);
            }
        } else if endpoint.scheme() != "https"
            || endpoint.host_str() != Some("api.elevenlabs.io")
            || endpoint.port_or_known_default() != Some(443)
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        endpoint
            .query_pairs_mut()
            .append_pair("output_format", "mp3_44100_128");
        let mut key = header::HeaderValue::from_str(&api_key)
            .map_err(|_| ProviderError::InvalidConfiguration)?;
        key.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert("xi-api-key", key);
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("audio/mpeg"),
        );
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .no_proxy()
            .https_only(!loopback)
            .connect_timeout(deadline.min(Duration::from_secs(5)))
            .timeout(deadline)
            .build()
            .map_err(|_| ProviderError::InvalidConfiguration)?;
        Ok(Self {
            client,
            endpoint,
            deadline,
            permits: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>, ProviderError> {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES || text.contains('\0') {
            return Err(ProviderError::InvalidInput);
        }
        #[derive(Serialize)]
        struct Request<'a> {
            text: &'a str,
            model_id: &'static str,
        }
        tokio::time::timeout(self.deadline, async {
            let _permit = self
                .permits
                .acquire()
                .await
                .map_err(|_| ProviderError::Transport)?;
            let mut response = self
                .client
                .post(self.endpoint.clone())
                .json(&Request {
                    text,
                    model_id: MODEL,
                })
                .send()
                .await
                .map_err(transport_error)?;
            match response.status().as_u16() {
                200..=299 => {}
                401 | 403 => return Err(ProviderError::Authentication),
                429 => return Err(ProviderError::RateLimited),
                500..=599 => return Err(ProviderError::ServerUnavailable),
                _ => return Err(ProviderError::Rejected),
            }
            if response
                .content_length()
                .is_some_and(|size| size > MAX_AUDIO_BYTES as u64)
            {
                return Err(ProviderError::ResponseTooLarge);
            }
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            if content_type
                .split(';')
                .next()
                .is_none_or(|kind| !kind.trim().eq_ignore_ascii_case("audio/mpeg"))
            {
                return Err(ProviderError::InvalidResponse);
            }
            let mut audio = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
                if chunk.len() > MAX_AUDIO_BYTES - audio.len() {
                    return Err(ProviderError::ResponseTooLarge);
                }
                audio.extend_from_slice(&chunk);
            }
            if audio.len() < 4
                || !(audio.starts_with(b"ID3") || (audio[0] == 0xff && audio[1] & 0xe0 == 0xe0))
            {
                return Err(ProviderError::InvalidResponse);
            }
            Ok(audio)
        })
        .await
        .map_err(|_| ProviderError::Timeout)?
    }
}
fn transport_error(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Transport
    }
}
