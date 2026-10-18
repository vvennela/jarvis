//! Email drafts that are read back before sending. The model may only compose;
//! sending requires the user's explicit confirmation of the exact draft that
//! was read back, checked by fixed code before any model call.
use std::{
    path::PathBuf,
    process::Stdio,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;

const MAX_SUBJECT: usize = 256;
const MAX_BODY: usize = 4000;
const MAX_OUTPUT: usize = 64 * 1024;
/// A read-back draft can be sent for this long; later it must be re-read.
pub const SEND_WINDOW: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailError {
    InvalidConfiguration,
    NotAuthorized,
    InvalidDraft,
    RecipientNotFound,
    AmbiguousRecipient,
    NoDraft,
    PermissionDenied,
    Unavailable,
    Timeout,
    NotSent,
}
impl std::fmt::Display for EmailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "Email isn't set up yet.",
            Self::NotAuthorized => "Tell me who the email is for and I'll write it up.",
            Self::InvalidDraft => "I couldn't put that email together. Want to try again?",
            Self::RecipientNotFound => {
                "I couldn't find an email address for them in your contacts."
            }
            Self::AmbiguousRecipient => {
                "A few contacts match that name. Who exactly should it go to?"
            }
            Self::NoDraft => "There's no email draft waiting to send.",
            Self::PermissionDenied => {
                "I need permission to use Mail and Contacts. Enable Jarvis under System Settings, Privacy & Security, Automation."
            }
            Self::Unavailable => "I couldn't reach Mail.",
            Self::Timeout => {
                "Mail took too long to answer, so I'm not sure it went out. Check your Sent folder before I try again."
            }
            Self::NotSent => "Mail didn't accept that email, so nothing was sent.",
        })
    }
}
impl std::error::Error for EmailError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailDraft {
    pub revision: u64,
    pub to_name: String,
    pub to_address: String,
    pub subject: String,
    pub body: String,
}
impl EmailDraft {
    /// The exact text spoken to the user; sending is bound to this revision.
    pub fn read_back(&self) -> String {
        format!(
            "Here's the email to {} at {}. Subject: {}. It says: {} Want me to send it?",
            self.to_name,
            spoken_address(&self.to_address),
            self.subject,
            self.body
        )
    }
}

fn spoken_address(address: &str) -> String {
    address.replace('@', " at ").replace('.', " dot ")
}

struct Pending {
    draft: EmailDraft,
    read_back_at: Instant,
}

/// Recipient lookup and sending through Mail/Contacts via fixed AppleScript.
/// Values are passed as argv, never interpolated into script source.
pub struct EmailClient {
    executable: PathBuf,
    timeout: Duration,
    pending: Mutex<Option<Pending>>,
    next_revision: Mutex<u64>,
}

const CONTACTS_SCRIPT: &str = r#"on run argv
set query to item 1 of argv
set output to ""
try
tell application "Contacts"
set matches to (every person whose name contains query)
repeat with person_ref in matches
set person_name to name of person_ref
repeat with email_ref in emails of person_ref
set output to output & person_name & tab & (value of email_ref) & linefeed
end repeat
end repeat
end tell
on error number error_number
if error_number is -1743 then return "permission_denied"
return "unavailable"
end try
return "ok" & linefeed & output
end run"#;

const SEND_SCRIPT: &str = r#"on run argv
set {to_address, mail_subject, mail_body} to {item 1 of argv, item 2 of argv, item 3 of argv}
try
tell application "Mail"
set outgoing to make new outgoing message with properties {subject:mail_subject, content:mail_body, visible:false}
tell outgoing to make new to recipient at end of to recipients with properties {address:to_address}
set accepted to send outgoing
end tell
on error number error_number
if error_number is -1743 then return "permission_denied"
return "unavailable"
end try
if accepted then return "sent"
return "not_sent"
end run"#;

impl EmailClient {
    pub fn native() -> Result<Self, EmailError> {
        Self::with_executable(PathBuf::from("/usr/bin/osascript"), Duration::from_secs(20))
    }
    pub fn with_executable(executable: PathBuf, timeout: Duration) -> Result<Self, EmailError> {
        if !executable.is_absolute() || timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(EmailError::InvalidConfiguration);
        }
        Ok(Self {
            executable,
            timeout,
            pending: Mutex::new(None),
            next_revision: Mutex::new(0),
        })
    }

    async fn run(&self, script: &str, args: &[&str]) -> Result<String, EmailError> {
        let mut child = tokio::process::Command::new(&self.executable)
            .arg("-e")
            .arg(script)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| EmailError::Unavailable)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or(EmailError::Unavailable)?
            .take(MAX_OUTPUT as u64 + 1);
        let mut out = Vec::new();
        let result = tokio::time::timeout(self.timeout, async {
            tokio::try_join!(stdout.read_to_end(&mut out), child.wait())
        })
        .await;
        let status = match result {
            Ok(Ok((_, status))) => status,
            Ok(Err(_)) => {
                let _ = child.kill().await;
                return Err(EmailError::Unavailable);
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(EmailError::Timeout);
            }
        };
        if !status.success() || out.len() > MAX_OUTPUT {
            return Err(EmailError::Unavailable);
        }
        let text = String::from_utf8(out).map_err(|_| EmailError::Unavailable)?;
        match text.trim() {
            "permission_denied" => Err(EmailError::PermissionDenied),
            "unavailable" => Err(EmailError::Unavailable),
            _ => Ok(text),
        }
    }

    /// An explicit address in the user's request, or exactly one contact whose
    /// name the user said. The model's argument must name that same recipient.
    pub async fn resolve_recipient(
        &self,
        user_request: &str,
        recipient: &str,
    ) -> Result<(String, String), EmailError> {
        let recipient = recipient.trim();
        if recipient.is_empty() || recipient.len() > 128 || recipient.chars().any(char::is_control)
        {
            return Err(EmailError::NotAuthorized);
        }
        let request = user_request.to_lowercase();
        if !mentions_email(&request) {
            return Err(EmailError::NotAuthorized);
        }
        if let Some(address) = explicit_address(user_request) {
            if !address.eq_ignore_ascii_case(recipient)
                && !request.contains(&recipient.to_lowercase())
            {
                return Err(EmailError::NotAuthorized);
            }
            return Ok((address.clone(), address));
        }
        if !contains_words(&request, &recipient.to_lowercase()) {
            return Err(EmailError::NotAuthorized);
        }
        let output = self.run(CONTACTS_SCRIPT, &[recipient]).await?;
        let mut lines = output.lines();
        if lines.next() != Some("ok") {
            return Err(EmailError::Unavailable);
        }
        let mut people: Vec<(String, String)> = Vec::new();
        for line in lines.filter(|line| !line.trim().is_empty()) {
            let Some((name, address)) = line.split_once('\t') else {
                continue;
            };
            if valid_address(address.trim()) {
                people.push((name.trim().to_owned(), address.trim().to_owned()));
            }
        }
        let mut names: Vec<&str> = people.iter().map(|(name, _)| name.as_str()).collect();
        names.dedup();
        match names.len() {
            0 => Err(EmailError::RecipientNotFound),
            // One person: use their first listed address. The read-back
            // speaks it, so a wrong address is caught before sending.
            1 => Ok(people.swap_remove(0)),
            _ => Err(EmailError::AmbiguousRecipient),
        }
    }

    /// Store a new draft revision; any earlier unsent draft is replaced.
    pub fn stage(
        &self,
        to_name: String,
        to_address: String,
        subject: &str,
        body: &str,
    ) -> Result<EmailDraft, EmailError> {
        let subject = subject.trim();
        let body = body.trim();
        if subject.is_empty()
            || body.is_empty()
            || subject.len() > MAX_SUBJECT
            || body.len() > MAX_BODY
            || subject.chars().any(char::is_control)
            || body.contains('\0')
            || !valid_address(&to_address)
        {
            return Err(EmailError::InvalidDraft);
        }
        let mut next = self.next_revision.lock().unwrap_or_else(|p| p.into_inner());
        *next += 1;
        let draft = EmailDraft {
            revision: *next,
            to_name,
            to_address,
            subject: subject.to_owned(),
            body: body.to_owned(),
        };
        *self.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(Pending {
            draft: draft.clone(),
            read_back_at: Instant::now(),
        });
        Ok(draft)
    }

    pub fn pending(&self) -> Option<EmailDraft> {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|pending| pending.draft.clone())
    }

    /// Reading the draft back again renews its send window.
    pub fn read_back(&self) -> Option<String> {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let pending = pending.as_mut()?;
        pending.read_back_at = Instant::now();
        Some(pending.draft.read_back())
    }

    pub fn discard(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .is_some()
    }

    /// Send the read-back draft exactly once. The draft is consumed before
    /// Mail is called, so an uncertain outcome is never retried blindly.
    pub async fn send_pending(&self) -> Result<EmailDraft, EmailError> {
        let draft = {
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            match pending.take() {
                Some(item) if item.read_back_at.elapsed() < SEND_WINDOW => item.draft,
                Some(item) => {
                    // Too old to trust the user remembers it: read it again first.
                    *pending = Some(item);
                    return Err(EmailError::NoDraft);
                }
                None => return Err(EmailError::NoDraft),
            }
        };
        let output = self
            .run(
                SEND_SCRIPT,
                &[&draft.to_address, &draft.subject, &draft.body],
            )
            .await?;
        match output.trim() {
            "sent" => Ok(draft),
            _ => Err(EmailError::NotSent),
        }
    }
}

fn mentions_email(request: &str) -> bool {
    request
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .any(|word| matches!(word, "email" | "e-mail" | "mail" | "emails"))
}

fn contains_words(haystack: &str, needle: &str) -> bool {
    let words: Vec<&str> = haystack
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    let target: Vec<&str> = needle
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    !target.is_empty() && words.windows(target.len()).any(|window| window == target)
}

fn valid_address(address: &str) -> bool {
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    address.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && address
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._%+-".contains(c))
}

fn explicit_address(request: &str) -> Option<String> {
    request
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '<' | '>' | '(' | ')'))
        .map(|word| word.trim_end_matches(['.', '!', '?']))
        .find(|word| valid_address(word))
        .map(str::to_owned)
}

/// Explicit confirmation to send the pending draft. A bare "yes" counts only
/// when the caller knows the previous reply was this draft's read-back.
pub fn is_send_confirmation(answer: &str, after_read_back: bool) -> bool {
    let normalized = answer
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_lowercase()
        .replace(',', "");
    let normalized = normalized
        .strip_prefix("jarvis ")
        .or_else(|| normalized.strip_prefix("hey jarvis "))
        .unwrap_or(&normalized)
        .to_owned();
    matches!(
        normalized.as_str(),
        "send it"
            | "send"
            | "send the email"
            | "send that"
            | "send it now"
            | "yes send it"
            | "yeah send it"
            | "yep send it"
            | "go ahead and send it"
            | "please send it"
            | "send it please"
            | "ok send it"
            | "okay send it"
    ) || (after_read_back
        && matches!(
            normalized.as_str(),
            "yes" | "yeah" | "yep" | "yes please" | "do it" | "go ahead" | "sure"
        ))
}

pub fn is_read_back_request(answer: &str) -> bool {
    let normalized = answer
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_lowercase()
        .replace(',', "");
    matches!(
        normalized.as_str(),
        "read it back"
            | "read it back to me"
            | "read me the email"
            | "read the email"
            | "read it again"
            | "what does it say"
            | "read the draft"
            | "read me the draft"
    )
}

pub fn is_discard_request(answer: &str) -> bool {
    let normalized = answer
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_lowercase()
        .replace(',', "");
    matches!(
        normalized.as_str(),
        "don't send it"
            | "do not send it"
            | "cancel the email"
            | "delete the draft"
            | "discard it"
            | "scrap it"
            | "never mind the email"
            | "no don't send it"
    )
}
