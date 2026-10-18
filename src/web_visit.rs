//! Open a website the user named ("look up cartoonnetwork.com", "go to cnn
//! dot com in Chrome"). Only a domain from the user's own words is opened,
//! always over https and without credentials. Opening is a launch, not a
//! verified page load.
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitError {
    NotAuthorized,
    Unavailable,
    Timeout,
}
impl std::fmt::Display for VisitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAuthorized => "Which website should I open?",
            Self::Unavailable => "I couldn't open that website.",
            Self::Timeout => "The browser took too long to respond.",
        })
    }
}
impl std::error::Error for VisitError {}

/// Browsers the user may name, with their app bundle and display name.
const BROWSERS: &[(&str, &str, &str)] = &[
    ("chrome", "com.google.Chrome", "Google Chrome"),
    ("safari", "com.apple.Safari", "Safari"),
    ("arc", "company.thebrowser.Browser", "Arc"),
    ("firefox", "org.mozilla.firefox", "Firefox"),
    ("brave", "com.brave.Browser", "Brave Browser"),
    ("edge", "com.microsoft.edgemac", "Microsoft Edge"),
];
const VERBS: &[&str] = &[
    "open", "go", "visit", "look", "pull", "load", "take", "bring", "show", "navigate", "head",
    "check", "launch",
];
/// Words that end a spoken domain when reading leftwards from its TLD.
const BOUNDARY: &[&str] = &[
    "open", "up", "to", "go", "visit", "look", "pull", "load", "take", "me", "bring", "show",
    "navigate", "head", "over", "check", "out", "launch", "the", "website", "site", "page", "web",
    "at", "in", "on", "and", "chrome", "safari", "arc", "firefox", "brave", "edge", "a", "can",
    "you", "please", "could", "would", "jarvis", "hey", "browser", "for", "of", "new", "tab",
];
const RESTRICTIVE: &[&str] = &[
    "not", "no", "never", "don't", "dont", "cannot", "can't", "if", "unless", "when", "until",
    "instead", "then",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    pub url: String,
    pub host: String,
    /// A browser the user named (spoken key), if any.
    pub browser: Option<&'static str>,
}

/// File names ("notes.md", "main.rs") are never websites.
const FILE_EXTENSIONS: &[&str] = &[
    "txt", "md", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "png", "jpg", "jpeg", "gif",
    "heic", "json", "csv", "py", "js", "ts", "rs", "swift", "toml", "yaml", "yml", "zip", "log",
    "sh", "html", "css", "mov", "mp3", "mp4", "wav", "key", "pages", "numbers", "sql", "db",
];
fn valid_tld(tld: &str) -> bool {
    (2..=24).contains(&tld.len())
        && tld.chars().all(|c| c.is_ascii_lowercase())
        && !FILE_EXTENSIONS.contains(&tld)
}

fn valid_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && host.len() <= 253
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && valid_tld(labels[labels.len() - 1])
}

/// A website request: an opening verb, then exactly one spoken domain.
pub fn website_intent(request: &str) -> Option<Visit> {
    if request.is_empty() || request.len() > 1024 || request.chars().any(char::is_control) {
        return None;
    }
    let lower = request.to_lowercase().replace(" dot ", ".");
    let words: Vec<String> = lower
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| matches!(c, ',' | '!' | '?' | '"' | '\'' | '(' | ')'))
                .trim_end_matches('.')
                .to_owned()
        })
        .filter(|word| !word.is_empty())
        .collect();
    if words
        .iter()
        .any(|word| RESTRICTIVE.contains(&word.as_str()))
        || words.iter().any(|word| word.contains('@'))
    {
        return None;
    }
    let mut found: Option<(usize, String, String)> = None;
    for (index, word) in words.iter().enumerate() {
        let candidate = word
            .strip_prefix("https://")
            .or_else(|| word.strip_prefix("http://"))
            .unwrap_or(word);
        let (host, path) = match candidate.split_once('/') {
            Some((host, path)) => (host, format!("/{path}")),
            None => (candidate, String::new()),
        };
        let host = host.strip_prefix("www.").unwrap_or(host);
        if !host.contains('.') {
            continue;
        }
        // "cartoon network.com": rejoin words spoken before the domain.
        let mut joined = host.to_owned();
        let mut start = index;
        while start > 0
            && index - start < 3
            && !BOUNDARY.contains(&words[start - 1].as_str())
            && words[start - 1]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            start -= 1;
            joined = format!("{}{joined}", words[start]);
        }
        if !valid_host(&joined)
            || path
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '@' | '\\'))
            || path.len() > 512
        {
            continue;
        }
        if found.is_some() {
            return None; // More than one site is ambiguous.
        }
        found = Some((start, joined, path));
    }
    let (start, host, path) = found?;
    if !words[..start]
        .iter()
        .any(|word| VERBS.contains(&word.as_str()))
    {
        return None;
    }
    let url = format!("https://{host}{path}");
    let parsed = reqwest::Url::parse(&url).ok()?;
    if parsed.host_str() != Some(host.as_str()) || !parsed.username().is_empty() {
        return None;
    }
    let browser = BROWSERS
        .iter()
        .find(|(key, _, _)| words.iter().any(|word| word == key))
        .map(|(key, _, _)| *key);
    Some(Visit {
        url: parsed.to_string(),
        host,
        browser,
    })
}

pub struct WebVisitor {
    executable: PathBuf,
    applications: Vec<PathBuf>,
    timeout: Duration,
}

impl WebVisitor {
    pub fn native() -> Self {
        let mut applications = vec![PathBuf::from("/Applications")];
        if let Some(home) = std::env::var_os("HOME") {
            applications.push(PathBuf::from(home).join("Applications"));
        }
        Self {
            executable: PathBuf::from("/usr/bin/open"),
            applications,
            timeout: Duration::from_secs(10),
        }
    }
    pub fn with_executable(executable: PathBuf, applications: Vec<PathBuf>) -> Self {
        Self {
            executable,
            applications,
            timeout: Duration::from_secs(5),
        }
    }

    fn installed(&self, app_name: &str) -> bool {
        self.applications
            .iter()
            .any(|dir| Path::new(dir).join(format!("{app_name}.app")).is_dir())
    }

    /// Open the site from this exact request. Returns the spoken result.
    pub async fn visit(&self, user_request: &str, url: &str) -> Result<String, VisitError> {
        let visit = website_intent(user_request).ok_or(VisitError::NotAuthorized)?;
        if visit.url != url {
            return Err(VisitError::NotAuthorized);
        }
        let mut command = tokio::process::Command::new(&self.executable);
        let mut note = String::new();
        let mut opened_in = None;
        if let Some(key) = visit.browser
            && let Some((_, bundle, app)) = BROWSERS.iter().find(|(k, _, _)| *k == key)
        {
            if self.installed(app) {
                command.args(["-b", bundle]);
                opened_in = Some(*app);
            } else {
                note = format!(" {app} isn't installed, so I used your default browser.");
            }
        }
        command
            .arg(&visit.url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| VisitError::Unavailable)?;
        let status = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(_)) => return Err(VisitError::Unavailable),
            Err(_) => {
                let _ = child.kill().await;
                return Err(VisitError::Timeout);
            }
        };
        if !status.success() {
            return Err(VisitError::Unavailable);
        }
        Ok(match opened_in {
            Some(app) => format!("Opened {} in {app}.", visit.host),
            None => format!("Opened {}.{note}", visit.host),
        })
    }
}
