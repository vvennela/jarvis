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

/// One contact address, with the person's company when Contacts has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub address: String,
    pub organization: String,
}

/// Several people match; the draft waits for the user to say which one.
struct Choice {
    query: String,
    category: &'static str,
    candidates: Vec<Candidate>,
    subject: String,
    body: String,
    asked_at: Instant,
}

pub enum Composed {
    Draft(EmailDraft),
    /// A spoken question; the next answer picks the recipient.
    Choose(String),
}

const PERSONAL_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "yahoo.com",
    "hotmail.com",
    "outlook.com",
    "live.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
];
const WORK_WORDS: &[&str] = &[
    "work",
    "feature",
    "features",
    "project",
    "deploy",
    "ship",
    "shipped",
    "launch",
    "release",
    "meeting",
    "client",
    "customer",
    "pr",
    "bug",
    "bugs",
    "review",
    "standup",
    "sprint",
    "deadline",
    "office",
    "team",
    "report",
    "demo",
    "spec",
    "roadmap",
    "build",
    "code",
    "invoice",
    "contract",
    "proposal",
    "manager",
    "boss",
    "coworker",
    "colleague",
    "slides",
    "quarter",
    "metrics",
    "ticket",
    "jira",
    "linear",
    "deck",
    "presentation",
];

/// "work" when the request is about work, otherwise "personal".
pub fn context_category(request: &str) -> &'static str {
    let lower = request.to_lowercase();
    let work = lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| WORK_WORDS.contains(&word));
    if work { "work" } else { "personal" }
}

fn personal_address(address: &str) -> bool {
    address
        .rsplit_once('@')
        .is_some_and(|(_, domain)| PERSONAL_DOMAINS.contains(&domain.to_lowercase().as_str()))
}

fn fits(candidate: &Candidate, category: &str) -> bool {
    if category == "work" {
        !candidate.organization.trim().is_empty() || !personal_address(&candidate.address)
    } else {
        personal_address(&candidate.address)
    }
}

/// Recipient lookup and sending through Mail/Contacts via fixed AppleScript.
/// Values are passed as argv, never interpolated into script source.
pub struct EmailClient {
    executable: PathBuf,
    timeout: Duration,
    pending: Mutex<Option<Pending>>,
    next_revision: Mutex<u64>,
    choice: Mutex<Option<Choice>>,
    /// Learned recipients: "brian|work" -> address, from the user's choices.
    memory: Option<PathBuf>,
    /// The whole spoken request, when a multi-step request split it into clauses.
    request_context: Mutex<String>,
}

const CONTACTS_SCRIPT: &str = r#"on run argv
set query to item 1 of argv
set output to ""
try
tell application "Contacts"
set matches to (every person whose name contains query)
repeat with person_ref in matches
set person_name to name of person_ref
set person_org to ""
try
set org_value to organization of person_ref
if org_value is not missing value then set person_org to org_value
end try
repeat with email_ref in emails of person_ref
set output to output & person_name & tab & (value of email_ref) & tab & person_org & linefeed
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
            choice: Mutex::new(None),
            memory: None,
            request_context: Mutex::new(String::new()),
        })
    }

    /// The user's whole request for this turn; a clause like "email Brian" is
    /// judged work or personal by everything the user said.
    pub fn set_request_context(&self, request: &str) {
        let mut context = self
            .request_context
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        context.clear();
        context.push_str(&request[..request.len().min(2048)]);
    }

    /// Remember which person the user means by a name, per work/personal context.
    pub fn with_recipient_memory(mut self, path: PathBuf) -> Self {
        self.memory = Some(path);
        self
    }

    fn remembered(&self) -> std::collections::HashMap<String, String> {
        self.memory
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .filter(|bytes| bytes.len() <= 64 * 1024)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn remember(&self, key: String, address: &str) {
        let Some(path) = &self.memory else {
            return;
        };
        let mut map = self.remembered();
        if map.len() >= 200 && !map.contains_key(&key) {
            return;
        }
        map.insert(key, address.to_owned());
        if let Ok(bytes) = serde_json::to_vec_pretty(&map) {
            let temporary = path.with_extension("json.tmp");
            if std::fs::write(&temporary, bytes).is_ok() {
                let _ = std::fs::rename(&temporary, path);
            }
        }
    }

    /// Contacts whose name contains what the user said, with their companies.
    async fn candidates(&self, recipient: &str) -> Result<Vec<Candidate>, EmailError> {
        let output = self.run(CONTACTS_SCRIPT, &[recipient]).await?;
        let mut lines = output.lines();
        if lines.next() != Some("ok") {
            return Err(EmailError::Unavailable);
        }
        let mut found = Vec::new();
        for line in lines.filter(|line| !line.trim().is_empty()) {
            let mut parts = line.split('\t');
            let (Some(name), Some(address)) = (parts.next(), parts.next()) else {
                continue;
            };
            let candidate = Candidate {
                name: name.trim().to_owned(),
                address: address.trim().to_owned(),
                organization: parts.next().unwrap_or("").trim().to_owned(),
            };
            if valid_address(&candidate.address) && !found.contains(&candidate) {
                found.push(candidate);
            }
        }
        Ok(found)
    }

    /// Resolve the recipient and stage a draft, or ask which person is meant.
    /// A remembered choice for this context wins; then the work/personal fit;
    /// otherwise the user is asked once and the answer is remembered.
    pub async fn compose(
        &self,
        user_request: &str,
        recipient: &str,
        subject: &str,
        body: &str,
    ) -> Result<Composed, EmailError> {
        let recipient = recipient.trim();
        let request = user_request.to_lowercase();
        if recipient.is_empty()
            || recipient.len() > 128
            || recipient.chars().any(char::is_control)
            || !mentions_email(&request)
        {
            return Err(EmailError::NotAuthorized);
        }
        if explicit_address(user_request).is_some() {
            let (name, address) = self.resolve_recipient(user_request, recipient).await?;
            return self
                .stage(name, address, subject, body)
                .map(Composed::Draft);
        }
        if !contains_words(&request, &recipient.to_lowercase()) {
            return Err(EmailError::NotAuthorized);
        }
        let candidates = self.candidates(recipient).await?;
        if candidates.is_empty() {
            return Err(EmailError::RecipientNotFound);
        }
        let whole = self
            .request_context
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let category = context_category(&format!("{user_request} {whole}"));
        let key = format!("{}|{category}", recipient.to_lowercase());
        let pick = |chosen: &Candidate| {
            self.stage(chosen.name.clone(), chosen.address.clone(), subject, body)
                .map(Composed::Draft)
        };
        if let Some(address) = self.remembered().get(&key)
            && let Some(chosen) = candidates.iter().find(|c| &c.address == address)
        {
            return pick(chosen);
        }
        let mut people: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
        people.sort_unstable();
        people.dedup();
        let fitting: Vec<&Candidate> = candidates.iter().filter(|c| fits(c, category)).collect();
        let mut fitting_people: Vec<&str> = fitting.iter().map(|c| c.name.as_str()).collect();
        fitting_people.sort_unstable();
        fitting_people.dedup();
        if people.len() == 1 {
            // One person: prefer the address that fits the context.
            return pick(fitting.first().copied().unwrap_or(&candidates[0]));
        }
        if fitting_people.len() == 1 && fitting.len() == 1 {
            return pick(fitting[0]);
        }
        let options: Vec<String> = candidates
            .iter()
            .take(3)
            .map(|c| {
                if c.organization.is_empty() {
                    format!("{} ({})", c.name, c.address)
                } else {
                    format!("{} at {}", c.name, c.organization)
                }
            })
            .collect();
        let question = format!("Which one: {}?", options.join(", or "));
        *self.choice.lock().unwrap_or_else(|p| p.into_inner()) = Some(Choice {
            query: recipient.to_lowercase(),
            category,
            candidates: candidates.into_iter().take(3).collect(),
            subject: subject.to_owned(),
            body: body.to_owned(),
            asked_at: Instant::now(),
        });
        Ok(Composed::Choose(question))
    }

    /// Drop a pending "which one" question without choosing.
    pub fn discard_choice(&self) {
        self.choice.lock().unwrap_or_else(|p| p.into_inner()).take();
    }

    pub fn awaiting_choice(&self) -> bool {
        self.choice
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|choice| choice.asked_at.elapsed() < Duration::from_secs(300))
    }

    /// The user's answer to "which one": a name, company, ordinal, or
    /// "the work one". The choice is remembered for this context.
    pub fn choose(&self, answer: &str) -> Option<Result<EmailDraft, EmailError>> {
        let mut slot = self.choice.lock().unwrap_or_else(|p| p.into_inner());
        let choice = slot.as_ref()?;
        if choice.asked_at.elapsed() >= Duration::from_secs(300) {
            *slot = None;
            return None;
        }
        let lower = answer.to_lowercase();
        let words: Vec<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        let ordinal = ["first", "second", "third"]
            .iter()
            .position(|o| words.contains(o));
        let by_words: Vec<&Candidate> = choice
            .candidates
            .iter()
            .filter(|c| {
                let label = format!("{} {}", c.name, c.organization).to_lowercase();
                let label_words: Vec<&str> = label
                    .split(|ch: char| !ch.is_alphanumeric())
                    .filter(|w| w.len() > 1 && *w != choice.query)
                    .collect();
                words.iter().any(|w| label_words.contains(w))
            })
            .collect();
        let by_kind: Vec<&Candidate> = if words.contains(&"work") {
            choice
                .candidates
                .iter()
                .filter(|c| fits(c, "work"))
                .collect()
        } else if words.contains(&"personal") {
            choice
                .candidates
                .iter()
                .filter(|c| fits(c, "personal"))
                .collect()
        } else {
            Vec::new()
        };
        let chosen = if let Some(index) = ordinal {
            choice.candidates.get(index)
        } else if by_words.len() == 1 {
            Some(by_words[0])
        } else if by_kind.len() == 1 {
            Some(by_kind[0])
        } else {
            None
        }?
        .clone();
        let choice = slot.take()?;
        drop(slot);
        self.remember(
            format!("{}|{}", choice.query, choice.category),
            &chosen.address,
        );
        Some(self.stage(chosen.name, chosen.address, &choice.subject, &choice.body))
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
        let candidates = self.candidates(recipient).await?;
        let mut names: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        match names.len() {
            0 => Err(EmailError::RecipientNotFound),
            // One person: use their first listed address. The read-back
            // speaks it, so a wrong address is caught before sending.
            1 => Ok((candidates[0].name.clone(), candidates[0].address.clone())),
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
    !crate::request_intent::negated_before(request, &["email", "e-mail", "mail", "emails"])
        && request
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
