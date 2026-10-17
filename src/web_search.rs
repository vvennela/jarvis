//! Public research through You Search. The caller supplies only the original
//! trusted user request; retrieved text is evidence, never executable authority.
//! API: https://you.com/docs/api-reference/search/v1-search
use reqwest::{Client, Url, header::HeaderValue};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const SEARCH_ENDPOINT: &str = "https://ydc-index.io/v1/search";
pub const MAX_RESPONSE_BYTES: usize = 512 * 1024;
pub const MAX_QUERY_BYTES: usize = 2048;
pub const MAX_RESULTS: usize = 5;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchError {
    InvalidConfiguration,
    InvalidInput,
    Authentication,
    RateLimited,
    Unavailable,
    Timeout,
    ResponseTooLarge,
    InvalidResponse,
}
impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "Web research needs a setup check.",
            Self::InvalidInput => {
                "What would you like me to research? Keep the request under 2,048 bytes."
            }
            Self::Authentication => "Web research couldn't sign in. Check its API key.",
            Self::RateLimited => "Web research is busy. Try again shortly.",
            Self::Unavailable => "I couldn't reach web research. Try again shortly.",
            Self::Timeout => "The web search took too long. Try again.",
            Self::ResponseTooLarge | Self::InvalidResponse => {
                "I couldn't verify the web search results."
            }
        })
    }
}
impl std::error::Error for SearchError {}
#[derive(Clone)]
pub struct WebSearchConfig {
    endpoint: Url,
    timeout: Duration,
}
impl WebSearchConfig {
    pub fn production() -> Self {
        Self {
            endpoint: Url::parse(SEARCH_ENDPOINT).expect("constant endpoint"),
            timeout: Duration::from_secs(10),
        }
    }
    /// Numeric-loopback transport injection for tests; never configured by model,
    /// page content or an endpoint environment variable.
    pub fn loopback_for_tests(endpoint: &str, timeout: Duration) -> Result<Self, SearchError> {
        let endpoint = Url::parse(endpoint).map_err(|_| SearchError::InvalidConfiguration)?;
        let loopback = endpoint
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if endpoint.scheme() != "http"
            || !loopback
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || timeout.is_zero()
            || timeout > Duration::from_secs(10)
        {
            return Err(SearchError::InvalidConfiguration);
        }
        Ok(Self { endpoint, timeout })
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WebResult {
    pub title: String,
    pub url: String,
    pub description: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct WebSearchResults {
    pub results: Vec<WebResult>,
    pub source: &'static str,
    pub trust: &'static str,
    pub retrieved_at_ms: u64,
}
#[derive(Clone)]
pub struct WebSearch {
    client: Client,
    key: HeaderValue,
    config: WebSearchConfig,
    permits: Arc<tokio::sync::Semaphore>,
}
impl WebSearch {
    pub fn production(key: impl Into<String>) -> Result<Self, SearchError> {
        Self::new(key, WebSearchConfig::production())
    }
    pub fn new(key: impl Into<String>, config: WebSearchConfig) -> Result<Self, SearchError> {
        let key = key.into();
        if key.is_empty()
            || key.len() > 4096
            || key
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(SearchError::InvalidConfiguration);
        }
        let mut key = HeaderValue::from_str(&key).map_err(|_| SearchError::InvalidConfiguration)?;
        key.set_sensitive(true);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(config.timeout)
            .build()
            .map_err(|_| SearchError::InvalidConfiguration)?;
        Ok(Self {
            client,
            key,
            config,
            permits: Arc::new(tokio::sync::Semaphore::new(2)),
        })
    }
    pub async fn search(
        &self,
        original_user_request: &str,
    ) -> Result<WebSearchResults, SearchError> {
        if original_user_request.trim().is_empty()
            || original_user_request.len() > MAX_QUERY_BYTES
            || original_user_request.chars().any(char::is_control)
        {
            return Err(SearchError::InvalidInput);
        }
        tokio::time::timeout(self.config.timeout, async {
            let _permit = self
                .permits
                .acquire()
                .await
                .map_err(|_| SearchError::Unavailable)?;
            let response = self
                .client
                .post(self.config.endpoint.clone())
                .header("X-API-Key", self.key.clone())
                .json(&serde_json::json!({"query":original_user_request,"count":MAX_RESULTS}))
                .send()
                .await
                .map_err(http_error)?;
            let bytes = bounded(response).await?;
            let response: SearchResponse =
                serde_json::from_slice(&bytes).map_err(|_| SearchError::InvalidResponse)?;
            if response
                .results
                .web
                .len()
                .saturating_add(response.results.news.len())
                > 100
            {
                return Err(SearchError::InvalidResponse);
            }
            let mut seen = HashSet::new();
            let mut results = Vec::with_capacity(MAX_RESULTS);
            for hit in response
                .results
                .web
                .into_iter()
                .chain(response.results.news)
            {
                let Some(url) = public_url(&hit.url) else {
                    continue;
                };
                let title = bounded_text(&hit.title, 512);
                if title.is_empty() || !seen.insert(url.clone()) {
                    continue;
                }
                let mut description = bounded_text(&hit.description, 2048);
                for snippet in hit.snippets.into_iter().take(3) {
                    if description.len() >= 2048 {
                        break;
                    }
                    let snippet = bounded_text(&snippet, 2048 - description.len());
                    if !snippet.is_empty() && !description.contains(&snippet) {
                        if !description.is_empty() {
                            description.push(' ');
                        }
                        description.push_str(&snippet);
                        description = bounded_text(&description, 2048);
                    }
                }
                results.push(WebResult {
                    title,
                    url,
                    description,
                });
                if results.len() == MAX_RESULTS {
                    break;
                }
            }
            let retrieved_at_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| SearchError::Unavailable)?
                .as_millis()
                .min(u64::MAX as u128) as u64;
            Ok(WebSearchResults {
                results,
                source: "you_search",
                trust: "untrusted_web",
                retrieved_at_ms,
            })
        })
        .await
        .map_err(|_| SearchError::Timeout)?
    }
}
#[derive(Deserialize)]
struct SearchResponse {
    results: Sections,
}
#[derive(Deserialize)]
struct Sections {
    #[serde(default)]
    web: Vec<Hit>,
    #[serde(default)]
    news: Vec<Hit>,
}
#[derive(Deserialize)]
struct Hit {
    url: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    snippets: Vec<String>,
}
fn http_error(error: reqwest::Error) -> SearchError {
    if error.is_timeout() {
        SearchError::Timeout
    } else {
        SearchError::Unavailable
    }
}
async fn bounded(mut response: reqwest::Response) -> Result<Vec<u8>, SearchError> {
    match response.status().as_u16() {
        200 => {}
        401 | 403 => return Err(SearchError::Authentication),
        429 => return Err(SearchError::RateLimited),
        _ => return Err(SearchError::Unavailable),
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(SearchError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
            return Err(SearchError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn bounded_text(value: &str, limit: usize) -> String {
    let mut result = String::new();
    for word in value.split_whitespace() {
        if !result.is_empty() && result.len() < limit {
            result.push(' ');
        }
        for ch in word.chars().filter(|c| !c.is_control()) {
            if result.len() + ch.len_utf8() > limit {
                return result.trim_end().to_owned();
            }
            result.push(ch);
        }
        if result.len() >= limit {
            break;
        }
    }
    result.trim_end().to_owned()
}
fn public_url(raw: &str) -> Option<String> {
    if raw.len() > 2048 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let mut url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let host = url
        .host_str()?
        .trim_matches(['[', ']'])
        .trim_end_matches('.');
    if let Ok(ip) = host.parse::<IpAddr>() {
        let public = match ip {
            IpAddr::V4(v) => public_v4(v),
            IpAddr::V6(v) => public_v6(v),
        };
        if !public {
            return None;
        }
    } else {
        if !host.contains('.')
            || host == "localhost"
            || [
                ".localhost",
                ".local",
                ".internal",
                ".localdomain",
                ".corp",
                ".private",
                ".intranet",
                ".lan",
                ".home",
                ".home.arpa",
                ".test",
                ".invalid",
                ".example",
                ".onion",
                ".i2p",
                ".alt",
            ]
            .iter()
            .any(|suffix| host.ends_with(suffix))
        {
            return None;
        }
    }
    url.set_fragment(None);
    let canonical = url.to_string();
    (canonical.len() <= 2048).then_some(canonical)
}
fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_documentation()
        || a == 0
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && b == 0 && c == 0))
}
fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return public_v4(mapped);
    }
    let s = ip.segments();
    s[0] & 0xe000 == 0x2000
        && s[0] != 0x2002
        && !(s[0] == 0x2001 && (s[1] == 0x0db8 || s[1] <= 0x002f))
}
