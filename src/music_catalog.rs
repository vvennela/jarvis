//! Artist discovery through official You Search and Spotify oEmbed APIs.
//! Search hits are suggestions; only Spotify metadata confirms their identity.
//! https://you.com/docs/api-reference/search/v1-search
//! https://developer.spotify.com/documentation/embeds/reference/oembed
use reqwest::{Client, Url, header::HeaderValue};
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashSet, fmt, net::IpAddr, sync::Arc, time::Duration};

pub const SEARCH_ENDPOINT: &str = "https://ydc-index.io/v1/search";
pub const OEMBED_ENDPOINT: &str = "https://open.spotify.com/oembed";
pub const MAX_SEARCH_BYTES: usize = 512 * 1024;
pub const MAX_OEMBED_BYTES: usize = 64 * 1024;
pub const MAX_EMBED_BYTES: usize = 256 * 1024;
pub const MAX_CANDIDATES: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogError {
    InvalidConfiguration,
    InvalidInput,
    Authentication,
    RateLimited,
    Unavailable,
    Timeout,
    ResponseTooLarge,
    InvalidResponse,
    NotFound,
    Ambiguous,
}
impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "Music search isn't configured yet.",
            Self::InvalidInput => "Which artist would you like to hear?",
            Self::Authentication => "Music search couldn't sign in. Check its configuration.",
            Self::RateLimited => "Music search is busy right now. Try again shortly.",
            Self::Unavailable => "I couldn't reach music search. Try again shortly.",
            Self::Timeout => "That artist lookup took too long. Try again shortly.",
            Self::ResponseTooLarge | Self::InvalidResponse => {
                "I couldn't verify that artist's details."
            }
            Self::NotFound => {
                "I couldn't find an exact artist match on Spotify. Which artist did you mean?"
            }
            Self::Ambiguous => {
                "I found several Spotify artists with that name, so I haven't chosen one."
            }
        })
    }
}
impl std::error::Error for CatalogError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedArtist {
    name: String,
    id: String,
    uri: String,
    track_uri: Option<String>,
}
impl ResolvedArtist {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn uri(&self) -> &str {
        &self.uri
    }
    pub fn track_uri(&self) -> Option<&str> {
        self.track_uri.as_deref()
    }
}

pub struct CatalogConfig {
    search: Url,
    oembed: Url,
    embed: Option<Url>,
    timeout: Duration,
}
impl CatalogConfig {
    pub fn production() -> Self {
        Self {
            search: Url::parse(SEARCH_ENDPOINT).expect("constant URL"),
            oembed: Url::parse(OEMBED_ENDPOINT).expect("constant URL"),
            embed: Some(
                Url::parse("https://open.spotify.com/embed/artist/").expect("constant URL"),
            ),
            timeout: Duration::from_secs(10),
        }
    }
    /// Explicit numeric-loopback fixture endpoints; never populated from env.
    pub fn loopback_for_tests(
        search: &str,
        oembed: &str,
        timeout: Duration,
    ) -> Result<Self, CatalogError> {
        fn endpoint(raw: &str) -> Result<Url, CatalogError> {
            let url = Url::parse(raw).map_err(|_| CatalogError::InvalidConfiguration)?;
            let loopback = url
                .host_str()
                .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback());
            if url.scheme() != "http"
                || !loopback
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(CatalogError::InvalidConfiguration);
            }
            Ok(url)
        }
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(CatalogError::InvalidConfiguration);
        }
        Ok(Self {
            search: endpoint(search)?,
            oembed: endpoint(oembed)?,
            embed: None,
            timeout,
        })
    }
    pub fn loopback_with_embed_for_tests(
        search: &str,
        oembed: &str,
        embed_base: &str,
        timeout: Duration,
    ) -> Result<Self, CatalogError> {
        let mut config = Self::loopback_for_tests(search, oembed, timeout)?;
        let validated = Self::loopback_for_tests(search, embed_base, timeout)?;
        if !validated.oembed.path().ends_with('/') {
            return Err(CatalogError::InvalidConfiguration);
        }
        config.embed = Some(validated.oembed);
        Ok(config)
    }
}

pub struct MusicCatalog {
    client: Client,
    key: HeaderValue,
    config: CatalogConfig,
    permits: Arc<tokio::sync::Semaphore>,
}
impl MusicCatalog {
    pub fn production(key: impl Into<String>) -> Result<Self, CatalogError> {
        Self::new(key, CatalogConfig::production())
    }
    pub fn new(key: impl Into<String>, config: CatalogConfig) -> Result<Self, CatalogError> {
        let key = key.into();
        if key.is_empty()
            || key.len() > 4096
            || key
                .bytes()
                .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        {
            return Err(CatalogError::InvalidConfiguration);
        }
        let mut key =
            HeaderValue::from_str(&key).map_err(|_| CatalogError::InvalidConfiguration)?;
        key.set_sensitive(true);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(config.timeout)
            .build()
            .map_err(|_| CatalogError::InvalidConfiguration)?;
        Ok(Self {
            client,
            key,
            config,
            permits: Arc::new(tokio::sync::Semaphore::new(2)),
        })
    }
    /// No action executes. Ambiguity, partial lookup failure and timeout never
    /// select a best-guess result; the caller can ask for clarification instead.
    pub async fn resolve_artist(&self, requested: &str) -> Result<ResolvedArtist, CatalogError> {
        let matches = self.resolve_candidates(requested).await?;
        match matches.len() {
            0 => Err(CatalogError::NotFound),
            1 => matches.into_iter().next().ok_or(CatalogError::NotFound),
            _ => Err(CatalogError::Ambiguous),
        }
    }
    /// Return all exact, Spotify-confirmed matches from the bounded candidate
    /// set, allowing a caller to preserve ambiguity without choosing one.
    pub async fn resolve_candidates(
        &self,
        requested: &str,
    ) -> Result<Vec<ResolvedArtist>, CatalogError> {
        if !valid_name(requested) {
            return Err(CatalogError::InvalidInput);
        }
        tokio::time::timeout(self.config.timeout, async {
            let _permit=self.permits.acquire().await.map_err(|_|CatalogError::Unavailable)?;
            let query=format!("\"{}\" Spotify artist",requested.trim().replace('\\',"\\\\").replace('"',"\\\""));
            let response=self.client.post(self.config.search.clone()).header("X-API-Key",self.key.clone()).json(&json!({"query":query,"count":MAX_CANDIDATES,"include_domains":["open.spotify.com"],"strict_operators":true})).send().await.map_err(http_error)?;
            let body=bounded(response,MAX_SEARCH_BYTES).await?;
            let response:SearchResponse=serde_json::from_slice(&body).map_err(|_|CatalogError::InvalidResponse)?;
            if response.results.web.len()>MAX_CANDIDATES { return Err(CatalogError::InvalidResponse); }
            let mut ids = HashSet::new();
            let mut checks = tokio::task::JoinSet::new();
            for hit in response.results.web {
                let Ok((id, url)) = artist_url(&hit.url) else { continue };
                if !ids.insert(id) { continue; }
                let client = self.client.clone();
                let mut endpoint = self.config.oembed.clone();
                endpoint.query_pairs_mut().append_pair("url", url.as_str());
                let requested = requested.to_owned();
                // Up to five independent metadata checks run in parallel. Only
                // search carries You credentials; candidate URLs are not destinations.
                checks.spawn(async move {
                    let response = client.get(endpoint)
                        .send().await.map_err(http_error)?;
                    if response.status() == reqwest::StatusCode::NOT_FOUND {
                        return Ok(None);
                    }
                    let body = bounded(response, MAX_OEMBED_BYTES).await?;
                    match verify_oembed_artist(&requested, url.as_str(), &body) {
                        Ok(artist) => Ok(Some(artist)),
                        Err(CatalogError::NotFound) => Ok(None),
                        Err(error) => Err(error),
                    }
                });
            }
            let mut matches = Vec::new();
            while let Some(result) = checks.join_next().await {
                if let Some(artist) = result.map_err(|_| CatalogError::Unavailable)?? {
                    matches.push(artist);
                }
            }
            if !matches.is_empty() && let Some(base) = &self.config.embed {
                let mut canonical = tokio::task::JoinSet::new();
                for artist in matches {
                    let client = self.client.clone();
                    let mut endpoint = base.clone();
                    endpoint.set_path(&format!("{}{}", base.path(), artist.id));
                    let requested = requested.to_owned();
                    canonical.spawn(async move {
                        let response = client.get(endpoint).send().await.map_err(http_error)?;
                        let body = bounded(response, MAX_EMBED_BYTES).await?;
                        verify_embed_artist(&requested, &body)
                    });
                }
                matches = Vec::new();
                let mut canonical_ids = HashSet::new();
                while let Some(result) = canonical.join_next().await {
                    let artist = result.map_err(|_| CatalogError::Unavailable)??;
                    if canonical_ids.insert(artist.id.clone()) { matches.push(artist); }
                }
            }
            matches.sort_by(|left, right| left.id.cmp(&right.id));
            Ok(matches)
        }).await.map_err(|_|CatalogError::Timeout)?
    }
}
fn http_error(error: reqwest::Error) -> CatalogError {
    if error.is_timeout() {
        CatalogError::Timeout
    } else {
        CatalogError::Unavailable
    }
}
async fn bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, CatalogError> {
    match response.status().as_u16() {
        200 => {}
        401 | 403 => return Err(CatalogError::Authentication),
        429 => return Err(CatalogError::RateLimited),
        _ => return Err(CatalogError::Unavailable),
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(CatalogError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        if chunk.len() > limit - body.len() {
            return Err(CatalogError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}
fn normalized(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn artist_url(raw: &str) -> Result<(String, Url), CatalogError> {
    if raw.len() > 2048 {
        return Err(CatalogError::InvalidResponse);
    }
    let mut url = Url::parse(raw).map_err(|_| CatalogError::InvalidResponse)?;
    if url.scheme() != "https"
        || url.host_str() != Some("open.spotify.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(CatalogError::InvalidResponse);
    }
    let segments: Vec<_> = url.path().split('/').collect();
    let id = match segments.as_slice() {
        ["", "artist", id] | ["", "embed", "artist", id] => *id,
        ["", locale, "artist", id]
            if locale.strip_prefix("intl-").is_some_and(|code| {
                code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_lowercase())
            }) =>
        {
            *id
        }
        _ => return Err(CatalogError::InvalidResponse),
    }
    .to_owned();
    if id.len() != 22 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(CatalogError::InvalidResponse);
    }
    url.set_query(None);
    url.set_fragment(None);
    url.set_path(&format!("/artist/{id}"));
    Ok((id, url))
}
/// Validate the metadata schema, canonical artist ID and exact artist name.
/// Production callers must supply bytes fetched from Spotify's approved oEmbed
/// endpoint; search titles and arbitrary page bodies are not verification.
fn verify_oembed_artist(
    requested: &str,
    url: &str,
    body: &[u8],
) -> Result<ResolvedArtist, CatalogError> {
    if !valid_name(requested) {
        return Err(CatalogError::InvalidInput);
    }
    if body.len() > MAX_OEMBED_BYTES {
        return Err(CatalogError::ResponseTooLarge);
    }
    let (id, _) = artist_url(url)?;
    let metadata: Oembed =
        serde_json::from_slice(body).map_err(|_| CatalogError::InvalidResponse)?;
    if metadata.provider_name != "Spotify"
        || metadata.provider_url != "https://spotify.com"
        || metadata.kind != "rich"
        || !valid_name(&metadata.title)
    {
        return Err(CatalogError::InvalidResponse);
    }
    if normalized(requested) != normalized(&metadata.title) {
        return Err(CatalogError::NotFound);
    }
    Ok(ResolvedArtist {
        name: metadata.title.trim().to_owned(),
        uri: format!("spotify:artist:{id}"),
        id,
        track_uri: None,
    })
}
#[derive(Deserialize)]
struct SearchResponse {
    results: SearchResults,
}
#[derive(Deserialize)]
struct SearchResults {
    #[serde(default)]
    web: Vec<SearchHit>,
}
#[derive(Deserialize)]
struct SearchHit {
    url: String,
}
#[derive(Deserialize)]
struct Oembed {
    provider_name: String,
    provider_url: String,
    #[serde(rename = "type")]
    kind: String,
    title: String,
}

// Spotify's public embed bootstrap carries canonical entity identity for artist
// aliases. Read only the inert JSON data; never evaluate scripts or render HTML.
fn verify_embed_artist(requested: &str, body: &[u8]) -> Result<ResolvedArtist, CatalogError> {
    let html = std::str::from_utf8(body).map_err(|_| CatalogError::InvalidResponse)?;
    let mut remaining = html;
    let mut bootstrap = None;
    while let Some(start) = remaining.find("<script") {
        remaining = &remaining[start + "<script".len()..];
        let end = remaining.find('>').ok_or(CatalogError::InvalidResponse)?;
        let attributes = &remaining[..end];
        let body_start = &remaining[end + 1..];
        let close = body_start
            .find("</script>")
            .ok_or(CatalogError::InvalidResponse)?;
        if attributes
            .split_ascii_whitespace()
            .any(|attribute| matches!(attribute, "id=\"__NEXT_DATA__\"" | "id='__NEXT_DATA__'"))
        {
            if bootstrap.is_some() {
                return Err(CatalogError::InvalidResponse);
            }
            bootstrap = Some(&body_start[..close]);
        }
        remaining = &body_start[close + "</script>".len()..];
    }
    let value: serde_json::Value =
        serde_json::from_str(bootstrap.ok_or(CatalogError::InvalidResponse)?)
            .map_err(|_| CatalogError::InvalidResponse)?;
    let entity: EmbedArtist = serde_json::from_value(
        value
            .pointer("/props/pageProps/state/data/entity")
            .ok_or(CatalogError::InvalidResponse)?
            .clone(),
    )
    .map_err(|_| CatalogError::InvalidResponse)?;
    if entity.kind != "artist"
        || !valid_name(&entity.name)
        || normalized(requested) != normalized(&entity.name)
        || entity.id.len() != 22
        || !entity.id.bytes().all(|byte| byte.is_ascii_alphanumeric())
        || entity.uri != format!("spotify:artist:{}", entity.id)
    {
        return Err(CatalogError::InvalidResponse);
    }
    if entity.track_list.len() > 100 {
        return Err(CatalogError::InvalidResponse);
    }
    let track_uri = entity
        .track_list
        .iter()
        .find(|track| {
            track.kind == "track"
                && track.playable
                && track.reason == "PLAYABLE"
                && normalized(&track.artist) == normalized(&entity.name)
                && track.uri.strip_prefix("spotify:track:").is_some_and(|id| {
                    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
                })
        })
        .map(|track| track.uri.clone());
    Ok(ResolvedArtist {
        name: entity.name.trim().to_owned(),
        id: entity.id,
        uri: entity.uri,
        track_uri,
    })
}
#[derive(Deserialize)]
struct EmbedArtist {
    #[serde(default, rename = "trackList")]
    track_list: Vec<EmbedTrack>,
    name: String,
    id: String,
    uri: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct EmbedTrack {
    uri: String,
    #[serde(rename = "subtitle")]
    artist: String,
    #[serde(rename = "entityType")]
    kind: String,
    #[serde(rename = "isPlayable")]
    playable: bool,
    #[serde(rename = "playabilityReason")]
    reason: String,
}
