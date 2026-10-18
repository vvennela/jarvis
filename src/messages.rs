//! Recent unread text messages, read-only. Content is returned to the caller
//! for one summary and is never stored or logged. Requires Full Disk Access.
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const MAX_MESSAGES: usize = 20;
const MAX_TEXT: usize = 500;
const APPLE_EPOCH_OFFSET: i64 = 978_307_200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessagesError {
    PermissionDenied,
    Unavailable,
}
impl std::fmt::Display for MessagesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::PermissionDenied => {
                "I can't see your messages yet. Give Jarvis Full Disk Access in System Settings, Privacy & Security, then restart me."
            }
            Self::Unavailable => "I couldn't read your messages right now.",
        })
    }
}
impl std::error::Error for MessagesError {}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Message {
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub minutes_ago: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Unread {
    pub messages: Vec<Message>,
    /// Unread messages in the window beyond the returned cap.
    pub more: usize,
}

pub struct MessagesReader {
    chat_db: PathBuf,
    address_books: Vec<PathBuf>,
}

impl MessagesReader {
    pub fn native() -> Option<Self> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let root = home.join("Library/Application Support/AddressBook");
        let mut address_books = vec![root.join("AddressBook-v22.abcddb")];
        if let Ok(sources) = std::fs::read_dir(root.join("Sources")) {
            for source in sources.flatten() {
                address_books.push(source.path().join("AddressBook-v22.abcddb"));
            }
        }
        Some(Self::with_paths(
            home.join("Library/Messages/chat.db"),
            address_books,
        ))
    }
    pub fn with_paths(chat_db: PathBuf, address_books: Vec<PathBuf>) -> Self {
        Self {
            chat_db,
            address_books,
        }
    }

    /// Unread incoming messages from the last `window`, newest first.
    pub fn unread(&self, window: Duration) -> Result<Unread, MessagesError> {
        let connection = open(&self.chat_db).map_err(|error| match error {
            OpenError::Denied => MessagesError::PermissionDenied,
            OpenError::Other => MessagesError::Unavailable,
        })?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| MessagesError::Unavailable)?
            .as_secs() as i64;
        let since_seconds = now - window.as_secs() as i64 - APPLE_EPOCH_OFFSET;
        // Modern databases store nanoseconds; older ones store seconds.
        let since_nanos = since_seconds.saturating_mul(1_000_000_000);
        let mut statement = connection
            .prepare(
                "SELECT message.text, message.attributedBody, handle.id, message.date, chat.display_name
                 FROM message
                 LEFT JOIN handle ON message.handle_id = handle.ROWID
                 LEFT JOIN chat_message_join ON chat_message_join.message_id = message.ROWID
                 LEFT JOIN chat ON chat.ROWID = chat_message_join.chat_id
                 WHERE message.is_from_me = 0 AND message.is_read = 0
                   AND message.item_type = 0 AND message.associated_message_type = 0
                   AND (message.date > ?1 OR (message.date < 100000000000 AND message.date > ?2))
                 ORDER BY message.date DESC LIMIT 200",
            )
            .map_err(|_| MessagesError::Unavailable)?;
        let rows = statement
            .query_map(params![since_nanos, since_seconds], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|_| MessagesError::Unavailable)?;
        let names = self.contact_names();
        let mut messages = Vec::new();
        let mut total = 0;
        for row in rows {
            let (text, body, handle, date, group) = row.map_err(|_| MessagesError::Unavailable)?;
            let Some(text) = text
                .filter(|text| !text.trim().is_empty())
                .or_else(|| body.as_deref().and_then(attributed_text))
            else {
                continue;
            };
            let text = text.replace('\u{fffc}', "").trim().to_owned();
            if text.is_empty() {
                continue;
            }
            total += 1;
            if messages.len() >= MAX_MESSAGES {
                continue;
            }
            let seconds = if date > 100_000_000_000 {
                date / 1_000_000_000
            } else {
                date
            };
            let handle = handle.unwrap_or_else(|| "unknown".into());
            messages.push(Message {
                from: names
                    .get(&normalize_handle(&handle))
                    .cloned()
                    .unwrap_or(handle),
                group: group.filter(|name| !name.trim().is_empty()),
                minutes_ago: ((now - APPLE_EPOCH_OFFSET - seconds) / 60).max(0),
                text: truncate(&text, MAX_TEXT),
            });
        }
        Ok(Unread {
            more: total - messages.len(),
            messages,
        })
    }

    /// Phone numbers and emails mapped to contact names. Unreadable address
    /// books are skipped; senders then appear by number or address.
    fn contact_names(&self) -> HashMap<String, String> {
        let mut names = HashMap::new();
        for path in &self.address_books {
            let Ok(connection) = open(path) else {
                continue;
            };
            for query in [
                "SELECT ZABCDPHONENUMBER.ZFULLNUMBER, ZABCDRECORD.ZFIRSTNAME, ZABCDRECORD.ZLASTNAME, ZABCDRECORD.ZORGANIZATION
                 FROM ZABCDPHONENUMBER JOIN ZABCDRECORD ON ZABCDPHONENUMBER.ZOWNER = ZABCDRECORD.Z_PK",
                "SELECT ZABCDEMAILADDRESS.ZADDRESS, ZABCDRECORD.ZFIRSTNAME, ZABCDRECORD.ZLASTNAME, ZABCDRECORD.ZORGANIZATION
                 FROM ZABCDEMAILADDRESS JOIN ZABCDRECORD ON ZABCDEMAILADDRESS.ZOWNER = ZABCDRECORD.Z_PK",
            ] {
                let Ok(mut statement) = connection.prepare(query) else {
                    continue;
                };
                let Ok(rows) = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                }) else {
                    continue;
                };
                for (handle, first, last, organization) in rows.flatten() {
                    let Some(handle) = handle else { continue };
                    let name = [first, last]
                        .into_iter()
                        .flatten()
                        .filter(|part| !part.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let name = if name.is_empty() {
                        organization.unwrap_or_default()
                    } else {
                        name
                    };
                    if !name.is_empty() {
                        names.insert(normalize_handle(&handle), name);
                    }
                }
            }
        }
        names
    }
}

enum OpenError {
    Denied,
    Other,
}

fn open(path: &Path) -> Result<Connection, OpenError> {
    // Probe the file first: without Full Disk Access, macOS denies the open.
    if let Err(error) = std::fs::File::open(path) {
        return Err(if error.kind() == std::io::ErrorKind::PermissionDenied {
            OpenError::Denied
        } else {
            OpenError::Other
        });
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| OpenError::Other)?;
    connection
        .busy_timeout(Duration::from_millis(500))
        .map_err(|_| OpenError::Other)?;
    Ok(connection)
}

/// Phone numbers compare by their last ten digits; addresses by lowercase.
pub fn normalize_handle(handle: &str) -> String {
    if handle.contains('@') {
        return handle.trim().to_lowercase();
    }
    let digits: String = handle.chars().filter(char::is_ascii_digit).collect();
    let start = digits.len().saturating_sub(10);
    digits[start..].to_owned()
}

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Newer macOS stores message text only in a typedstream `attributedBody`.
/// The string follows the `NSString` class marker as `+`, a length, then UTF-8.
pub fn attributed_text(body: &[u8]) -> Option<String> {
    let marker = body.windows(8).position(|window| window == b"NSString")?;
    let rest = &body[marker + 8..];
    let plus = rest.iter().take(16).position(|&byte| byte == b'+')?;
    let rest = &rest[plus + 1..];
    let (length, start): (usize, usize) = match *rest.first()? {
        0x81 => (
            u16::from_le_bytes([*rest.get(1)?, *rest.get(2)?]) as usize,
            3,
        ),
        0x82 => (
            u32::from_le_bytes([*rest.get(1)?, *rest.get(2)?, *rest.get(3)?, *rest.get(4)?])
                as usize,
            5,
        ),
        byte if byte < 0x80 => (byte as usize, 1),
        _ => return None,
    };
    let bytes = rest.get(start..start.checked_add(length)?)?;
    String::from_utf8(bytes.to_vec()).ok()
}

/// The user's own request must ask about texts or messages.
pub fn mentions_messages(request: &str) -> bool {
    !crate::request_intent::negated_before(
        request,
        &[
            "text",
            "texts",
            "message",
            "messages",
            "imessage",
            "imessages",
            "sms",
        ],
    ) && request
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| {
            matches!(
                word,
                "text" | "texts" | "message" | "messages" | "imessage" | "imessages" | "sms"
            )
        })
}
