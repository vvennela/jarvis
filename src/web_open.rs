//! Resolve an explicitly requested Wikipedia article before opening its canonical URL.
//! User and model text never become executable code or an arbitrary destination.
use reqwest::{Client, Url};
use serde_json::Value;
use std::{fmt, net::IpAddr, path::PathBuf, process::Stdio, time::Duration};

pub const MAX_PAGE_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebOpenError {
    InvalidConfiguration,
    NotAuthorized,
    NotFound,
    Ambiguous,
    InvalidResponse,
    Unavailable,
    Timeout,
    OpenFailed,
}
impl fmt::Display for WebOpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "The webpage opener needs a setup fix.",
            Self::NotAuthorized => "Sorry, I didn't quite catch which page.",
            Self::NotFound => {
                "I couldn't find that Wikipedia article. What exact topic did you mean?"
            }
            Self::Ambiguous => "That Wikipedia topic has several meanings. Which one did you mean?",
            Self::InvalidResponse => {
                "I couldn't verify the Wikipedia article, so I haven't opened it."
            }
            Self::Unavailable => "I couldn't reach Wikipedia. Please try again shortly.",
            Self::Timeout => {
                "Wikipedia took too long to respond. I haven't confirmed the page opened."
            }
            Self::OpenFailed => "I found the article, but couldn't open your browser.",
        })
    }
}
impl std::error::Error for WebOpenError {}

#[derive(Clone)]
pub struct WebOpenConfig {
    endpoint: Url,
    timeout: Duration,
}
impl WebOpenConfig {
    pub fn production() -> Self {
        Self {
            endpoint: Url::parse("https://en.wikipedia.org/w/api.php")
                .expect("static Wikipedia URL"),
            timeout: Duration::from_secs(10),
        }
    }
    /// Explicit numeric-loopback injection; production never accepts arbitrary hosts.
    pub fn loopback_for_tests(endpoint: &str, timeout: Duration) -> Result<Self, WebOpenError> {
        let endpoint = Url::parse(endpoint).map_err(|_| WebOpenError::InvalidConfiguration)?;
        let loopback = endpoint
            .host_str()
            .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if endpoint.scheme() != "http"
            || !loopback
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
            || endpoint.query().is_some()
            || timeout.is_zero()
            || timeout > Duration::from_secs(15)
        {
            return Err(WebOpenError::InvalidConfiguration);
        }
        Ok(Self { endpoint, timeout })
    }
}
impl Default for WebOpenConfig {
    fn default() -> Self {
        Self::production()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedPage {
    pub title: String,
    pub url: String,
}

pub struct WebOpener {
    client: Client,
    config: WebOpenConfig,
    executable: PathBuf,
}
impl WebOpener {
    pub fn native() -> Result<Self, WebOpenError> {
        #[cfg(not(target_os = "macos"))]
        return Err(WebOpenError::Unavailable);
        #[cfg(target_os = "macos")]
        Self::with_config(WebOpenConfig::production(), PathBuf::from("/usr/bin/open"))
    }
    pub fn with_executable(executable: PathBuf) -> Result<Self, WebOpenError> {
        Self::with_config(WebOpenConfig::production(), executable)
    }
    pub fn with_config(config: WebOpenConfig, executable: PathBuf) -> Result<Self, WebOpenError> {
        if !executable.is_absolute() {
            return Err(WebOpenError::InvalidConfiguration);
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.timeout.min(Duration::from_secs(3)))
            .timeout(config.timeout)
            .user_agent("Jarvis/0.1 (personal Wikipedia article opener)")
            .build()
            .map_err(|_| WebOpenError::InvalidConfiguration)?;
        Ok(Self {
            client,
            config,
            executable,
        })
    }
    pub async fn open(
        &self,
        original_request: &str,
        site: &str,
        topic: &str,
    ) -> Result<OpenedPage, WebOpenError> {
        authorize_page_request(original_request, site, topic)?;
        tokio::time::timeout(self.config.timeout, self.resolve_and_open(topic))
            .await
            .map_err(|_| WebOpenError::Timeout)?
    }
    async fn resolve_and_open(&self, topic: &str) -> Result<OpenedPage, WebOpenError> {
        let mut endpoint = self.config.endpoint.clone();
        endpoint.query_pairs_mut().extend_pairs([
            ("action", "query"),
            ("format", "json"),
            ("formatversion", "2"),
            ("redirects", "1"),
            ("prop", "info|pageprops"),
            ("inprop", "url"),
            ("titles", topic),
        ]);
        let mut response = self.client.get(endpoint).send().await.map_err(transport)?;
        if !response.status().is_success() {
            return Err(WebOpenError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PAGE_RESPONSE_BYTES as u64)
        {
            return Err(WebOpenError::InvalidResponse);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_PAGE_RESPONSE_BYTES {
                return Err(WebOpenError::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value =
            serde_json::from_slice(&bytes).map_err(|_| WebOpenError::InvalidResponse)?;
        let pages = body
            .pointer("/query/pages")
            .and_then(Value::as_array)
            .ok_or(WebOpenError::InvalidResponse)?;
        if pages.len() != 1 {
            return Err(WebOpenError::Ambiguous);
        }
        let page = &pages[0];
        if page.get("missing").is_some() || page.get("invalid").is_some() {
            return Err(WebOpenError::NotFound);
        }
        if page.pointer("/pageprops/disambiguation").is_some() {
            return Err(WebOpenError::Ambiguous);
        }
        let title = page
            .get("title")
            .and_then(Value::as_str)
            .ok_or(WebOpenError::InvalidResponse)?;
        if page.get("ns").and_then(Value::as_u64) != Some(0)
            || page
                .get("pageid")
                .and_then(Value::as_u64)
                .is_none_or(|id| id == 0)
            || title.contains("://")
            || !verified_title_mapping(&body, topic, title)
            || title.is_empty()
            || title.len() > 512
            || title.chars().any(char::is_control)
        {
            return Err(WebOpenError::InvalidResponse);
        }
        let mut url = Url::parse("https://en.wikipedia.org/wiki/").expect("static Wikipedia URL");
        url.path_segments_mut()
            .map_err(|_| WebOpenError::InvalidResponse)?
            .pop_if_empty()
            .push(&title.replace(' ', "_"));
        let mut child = tokio::process::Command::new(&self.executable)
            .arg(url.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| WebOpenError::OpenFailed)?;
        let status = child.wait().await.map_err(|_| WebOpenError::OpenFailed)?;
        if !status.success() {
            return Err(WebOpenError::OpenFailed);
        }
        // LaunchServices accepted the URL. This does not claim browser DOM verification.
        Ok(OpenedPage {
            title: title.to_owned(),
            url: url.into(),
        })
    }
}
fn transport(error: reqwest::Error) -> WebOpenError {
    if error.is_timeout() {
        WebOpenError::Timeout
    } else {
        WebOpenError::Unavailable
    }
}
fn normalized(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
/// Validate a single original-user clause, never model- or page-generated instructions.
pub fn authorize_page_request(request: &str, site: &str, topic: &str) -> Result<(), WebOpenError> {
    if request.is_empty()
        || request.len() > 8192
        || !site.eq_ignore_ascii_case("wikipedia")
        || topic.trim().is_empty()
        || topic.len() > 256
        || topic.trim() != topic
        || request
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '`' | '$' | ';'))
        || topic
            .chars()
            .any(|c| c.is_control() || matches!(c, '|' | ':' | '#' | '/' | '\\'))
    {
        return Err(WebOpenError::NotAuthorized);
    }
    let target = normalized(topic);
    let matches = crate::request_intent::wikipedia_page_intent(request)
        .is_some_and(|requested| !requested.is_empty() && requested == target);
    if !matches {
        return Err(WebOpenError::NotAuthorized);
    }
    Ok(())
}

// Redirect and normalization metadata must connect the requested title to the
// returned article; a well-shaped but unrelated page is not target evidence.
fn verified_title_mapping(body: &Value, topic: &str, title: &str) -> bool {
    let canonical = |s: &str| normalized(&s.replace('_', " "));
    let target = canonical(title);
    let mut current = canonical(topic);
    if current == target {
        return true;
    }
    let mut mappings = Vec::new();
    for path in ["/query/normalized", "/query/redirects"] {
        if let Some(entries) = body.pointer(path) {
            let Some(entries) = entries.as_array() else {
                return false;
            };
            if entries.len() > 16 {
                return false;
            }
            for entry in entries {
                let (Some(from), Some(to)) = (
                    entry.get("from").and_then(Value::as_str),
                    entry.get("to").and_then(Value::as_str),
                ) else {
                    return false;
                };
                if from.len() > 512 || to.len() > 512 {
                    return false;
                }
                mappings.push((canonical(from), canonical(to)));
            }
        }
    }
    for _ in 0..16 {
        let Some((_, next)) = mappings.iter().find(|(from, _)| from == &current) else {
            return false;
        };
        current = next.clone();
        if current == target {
            return true;
        }
    }
    false
}
