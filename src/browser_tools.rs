//! Fresh, read-only browser requests over the authenticated native-host session.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const DEADLINE: Duration = Duration::from_secs(5);
pub const MAX_FRAME: usize = 512 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserError {
    InvalidInput,
    Unavailable,
    PermissionRequired,
    StaleDocument,
    Timeout,
    InvalidResponse,
}
impl fmt::Display for BrowserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "Choose a browser tab from the latest search.",
            Self::Unavailable => "Connect the Jarvis browser extension to read approved tabs.",
            Self::PermissionRequired => "Allow that website in Jarvis browser settings first.",
            Self::StaleDocument => "That tab changed. Search again to read its current page.",
            Self::Timeout => "The browser took too long to respond. Try again.",
            Self::InvalidResponse => "I couldn't verify the browser response.",
        })
    }
}
impl std::error::Error for BrowserError {}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserTab {
    pub resource_id: String,
    pub tab_id: i64,
    pub window_id: i64,
    pub document_id: String,
    pub url: String,
    pub title: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserPage {
    pub tab: BrowserTab,
    pub content: String,
    pub truncated: bool,
    pub trust: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "arguments", deny_unknown_fields)]
pub enum BrowserRequest {
    #[serde(rename = "search_tabs")]
    SearchTabs { query: String },
    #[serde(rename = "read_tab")]
    ReadTab {
        resource_id: String,
        expected_document_id: String,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientRequest {
    pub request_id: String,
    #[serde(flatten)]
    pub request: BrowserRequest,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub request_id: String,
    pub status: String,
    #[serde(default)]
    pub tabs: Vec<BrowserTab>,
    pub page: Option<BrowserPage>,
}
#[derive(Clone)]
pub struct BrowserClient {
    path: PathBuf,
    discovered: Arc<Mutex<HashMap<String, BrowserTab>>>,
}
impl BrowserClient {
    pub fn native() -> Result<Self, BrowserError> {
        Self::new(socket_path()?)
    }
    pub fn new(path: PathBuf) -> Result<Self, BrowserError> {
        if !path.is_absolute() || path.as_os_str().len() > 100 {
            return Err(BrowserError::InvalidInput);
        }
        Ok(Self {
            path,
            discovered: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub async fn search_tabs(&self, query: &str) -> Result<Vec<BrowserTab>, BrowserError> {
        if query.trim().is_empty() || query.len() > 256 || query.chars().any(char::is_control) {
            return Err(BrowserError::InvalidInput);
        }
        // A failed or cancelled new search must not retain old read authority.
        self.discovered
            .lock()
            .map_err(|_| BrowserError::Unavailable)?
            .clear();
        let response = self
            .request(BrowserRequest::SearchTabs {
                query: query.to_owned(),
            })
            .await?;
        if response.tabs.len() > 16
            || response.page.is_some()
            || response.tabs.iter().any(|t| !valid_tab(t))
        {
            return Err(BrowserError::InvalidResponse);
        }
        let mut validated = HashMap::new();
        for tab in &response.tabs {
            if validated
                .insert(tab.resource_id.clone(), tab.clone())
                .is_some()
            {
                return Err(BrowserError::InvalidResponse);
            }
        }
        *self
            .discovered
            .lock()
            .map_err(|_| BrowserError::Unavailable)? = validated;
        Ok(response.tabs)
    }
    pub async fn read_tab(
        &self,
        resource_id: &str,
        expected_document_id: &str,
    ) -> Result<BrowserPage, BrowserError> {
        let expected = self
            .discovered
            .lock()
            .map_err(|_| BrowserError::Unavailable)?
            .get(resource_id)
            .cloned()
            .filter(|t| t.document_id == expected_document_id)
            .ok_or(BrowserError::InvalidInput)?;
        let response = self
            .request(BrowserRequest::ReadTab {
                resource_id: resource_id.to_owned(),
                expected_document_id: expected_document_id.to_owned(),
            })
            .await?;
        let page = response.page.ok_or(BrowserError::InvalidResponse)?;
        if !response.tabs.is_empty()
            || !valid_tab(&page.tab)
            || page.tab.resource_id != expected.resource_id
            || page.tab.document_id != expected.document_id
            || page.tab.tab_id != expected.tab_id
            || page.tab.url != expected.url
            || page.content.len() > 65536
            || page.content.contains('\0')
            || page.trust != "untrusted_page"
        {
            return Err(BrowserError::InvalidResponse);
        }
        Ok(page)
    }
    async fn request(&self, request: BrowserRequest) -> Result<Response, BrowserError> {
        let request_id = random_id()?;
        let body = serde_json::to_vec(&ClientRequest {
            request_id: request_id.clone(),
            request,
        })
        .map_err(|_| BrowserError::InvalidInput)?;
        tokio::time::timeout(DEADLINE, async {
            validate_socket(&self.path)?;
            let mut stream = tokio::net::UnixStream::connect(&self.path)
                .await
                .map_err(|_| BrowserError::Unavailable)?;
            if !same_user(&stream) {
                return Err(BrowserError::Unavailable);
            }
            stream
                .write_u32_le(body.len() as u32)
                .await
                .map_err(|_| BrowserError::Unavailable)?;
            stream
                .write_all(&body)
                .await
                .map_err(|_| BrowserError::Unavailable)?;
            let len = stream
                .read_u32_le()
                .await
                .map_err(|_| BrowserError::Unavailable)? as usize;
            if len == 0 || len > MAX_FRAME {
                return Err(BrowserError::InvalidResponse);
            }
            let mut bytes = vec![0; len];
            stream
                .read_exact(&mut bytes)
                .await
                .map_err(|_| BrowserError::Unavailable)?;
            let response: Response =
                serde_json::from_slice(&bytes).map_err(|_| BrowserError::InvalidResponse)?;
            if response.request_id != request_id {
                return Err(BrowserError::InvalidResponse);
            }
            match response.status.as_str() {
                "ok" => Ok(response),
                "stale_document" => Err(BrowserError::StaleDocument),
                "permission_required" => Err(BrowserError::PermissionRequired),
                "timeout" => Err(BrowserError::Timeout),
                "unavailable" | "cancelled" => Err(BrowserError::Unavailable),
                _ => Err(BrowserError::InvalidResponse),
            }
        })
        .await
        .map_err(|_| BrowserError::Timeout)?
    }
}
pub(crate) fn valid_tab(tab: &BrowserTab) -> bool {
    if !(0..=i32::MAX as i64).contains(&tab.tab_id)
        || !(0..=i32::MAX as i64).contains(&tab.window_id)
        || tab.document_id.is_empty()
        || tab.document_id.len() > 128
        || tab.document_id.chars().any(char::is_control)
        || tab.title.len() > 2048
        || tab.title.contains('\0')
        || tab.url.len() > 8192
    {
        return false;
    }
    let Some((session, id)) = tab
        .resource_id
        .strip_prefix("browser-live:")
        .and_then(|r| r.split_once(':'))
    else {
        return false;
    };
    if session.len() != 64
        || !session.bytes().all(|b| b.is_ascii_hexdigit())
        || id != tab.tab_id.to_string()
    {
        return false;
    }
    reqwest::Url::parse(&tab.url).ok().is_some_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}
pub(crate) fn random_id() -> Result<String, BrowserError> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|_| BrowserError::Unavailable)?;
    Ok(b.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn socket_path() -> Result<PathBuf, BrowserError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or(BrowserError::Unavailable)?;
    Ok(home.join("Library/Application Support/Jarvis/BrowserBridge/bridge.sock"))
}
#[cfg(target_os = "macos")]
pub(crate) fn same_user<F: std::os::fd::AsFd>(fd: &F) -> bool {
    nix::unistd::getpeereid(fd).is_ok_and(|(uid, _)| uid == nix::unistd::geteuid())
}
#[cfg(target_os = "linux")]
pub(crate) fn same_user<F: std::os::fd::AsFd>(fd: &F) -> bool {
    nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::PeerCredentials)
        .is_ok_and(|cred| cred.uid() == nix::unistd::geteuid().as_raw())
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn same_user<F: std::os::fd::AsFd>(_: &F) -> bool {
    false
}
fn validate_socket(path: &Path) -> Result<(), BrowserError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = std::fs::symlink_metadata(path).map_err(|_| BrowserError::Unavailable)?;
    let parent = path.parent().ok_or(BrowserError::Unavailable)?;
    let directory = std::fs::symlink_metadata(parent).map_err(|_| BrowserError::Unavailable)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || !directory.is_dir()
        || directory.file_type().is_symlink()
        || directory.uid() != nix::unistd::geteuid().as_raw()
        || directory.mode() & 0o077 != 0
    {
        return Err(BrowserError::Unavailable);
    }
    Ok(())
}

pub(crate) struct HostCommand {
    pub request: ClientRequest,
    pub response: std::sync::mpsc::SyncSender<Response>,
}
pub(crate) struct SocketServer {
    stop: Arc<std::sync::atomic::AtomicBool>,
    path: PathBuf,
}
impl Drop for SocketServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        let _ = std::fs::remove_file(&self.path);
    }
}
pub(crate) fn serve_socket(
    path: &Path,
    sender: std::sync::mpsc::SyncSender<crate::bridge::LiveInput>,
) -> Result<SocketServer, BrowserError> {
    use std::{
        io::{Read, Write},
        os::unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        sync::atomic::{AtomicBool, Ordering},
    };
    let parent = path.parent().ok_or(BrowserError::InvalidInput)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|_| BrowserError::Unavailable)?;
    let meta = std::fs::symlink_metadata(parent).map_err(|_| BrowserError::Unavailable)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(BrowserError::Unavailable);
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket()
            || meta.uid() != nix::unistd::geteuid().as_raw()
            || UnixStream::connect(path).is_ok()
        {
            return Err(BrowserError::Unavailable);
        }
        std::fs::remove_file(path).map_err(|_| BrowserError::Unavailable)?;
    }
    let listener = UnixListener::bind(path).map_err(|_| BrowserError::Unavailable)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| BrowserError::Unavailable)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| BrowserError::Unavailable)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    std::thread::spawn(move || {
        while !stopped.load(Ordering::Acquire) {
            let (mut stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            };
            if !same_user(&stream) {
                continue;
            }
            let _ = stream.set_read_timeout(Some(DEADLINE));
            let _ = stream.set_write_timeout(Some(DEADLINE));
            let mut header = [0u8; 4];
            if stream.read_exact(&mut header).is_err() {
                continue;
            }
            let len = u32::from_le_bytes(header) as usize;
            if len == 0 || len > 4096 {
                continue;
            }
            let mut bytes = vec![0; len];
            if stream.read_exact(&mut bytes).is_err() {
                continue;
            }
            let Ok(request) = serde_json::from_slice::<ClientRequest>(&bytes) else {
                continue;
            };
            let id = request.request_id.clone();
            if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let command = HostCommand {
                request,
                response: tx,
            };
            if sender
                .try_send(crate::bridge::LiveInput::Command(command))
                .is_err()
            {
                continue;
            }
            let response = rx.recv_timeout(DEADLINE).unwrap_or(Response {
                request_id: id,
                status: "timeout".into(),
                tabs: vec![],
                page: None,
            });
            if let Ok(bytes) = serde_json::to_vec(&response) {
                let _ = stream
                    .write_all(&(bytes.len() as u32).to_le_bytes())
                    .and_then(|()| stream.write_all(&bytes));
            }
        }
    });
    Ok(SocketServer {
        stop,
        path: path.to_owned(),
    })
}

#[derive(Debug, Serialize)]
pub struct BrowserSetupStatus {
    pub native_host_registered: bool,
    pub extension_id: Option<String>,
    pub bridge_connected: bool,
    pub status: &'static str,
}
/// Inspect only Jarvis's registration and authenticated local transport. No tabs
/// are queried and no installation files are created by this status check.
pub async fn setup_status() -> BrowserSetupStatus {
    let extension_id = registered_extension();
    let registered = extension_id.is_some();
    let connected = if let Ok(path) = socket_path() {
        if validate_socket(&path).is_ok() {
            tokio::time::timeout(Duration::from_millis(500), async {
                tokio::net::UnixStream::connect(path)
                    .await
                    .is_ok_and(|stream| same_user(&stream))
            })
            .await
            .unwrap_or(false)
        } else {
            false
        }
    } else {
        false
    };
    BrowserSetupStatus {
        native_host_registered: registered,
        extension_id,
        bridge_connected: connected,
        status: if connected && registered {
            "ready"
        } else if registered {
            "extension_not_connected"
        } else {
            "registration_missing"
        },
    }
}
fn registered_extension() -> Option<String> {
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())?;
    let path = home.join(
        "Library/Application Support/Google/Chrome/NativeMessagingHosts/com.jarvis.bridge.json",
    );
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || metadata.len() > 16384
        || metadata.uid() != nix::unistd::geteuid().as_raw()
    {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 16384 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("name")?.as_str()? != "com.jarvis.bridge"
        || value.get("type")?.as_str()? != "stdio"
    {
        return None;
    }
    let launcher = Path::new(value.get("path")?.as_str()?);
    let launcher_metadata = std::fs::symlink_metadata(launcher).ok()?;
    if !launcher.is_absolute()
        || !launcher_metadata.is_file()
        || launcher_metadata.uid() != nix::unistd::geteuid().as_raw()
    {
        return None;
    }
    let origins = value.get("allowed_origins")?.as_array()?;
    if origins.len() != 1 {
        return None;
    }
    let origin = origins[0].as_str()?;
    let id = origin
        .strip_prefix("chrome-extension://")?
        .strip_suffix('/')?;
    crate::bridge::validate_extension_origin(id, origin).ok()?;
    Some(id.to_owned())
}
