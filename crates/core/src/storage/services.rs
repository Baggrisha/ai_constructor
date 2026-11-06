use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{json, tracked_bytes, validate_current};
use crate::{CoreError, ProjectSession, Result, State};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Chat {
    id: Uuid,
    project_id: Uuid,
    title: String,
    mode: String,
    created_at: u64,
    updated_at: u64,
    archived: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum ContentBlock {
    Text { text: String },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Attachment {
    asset_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewMessage {
    role: String,
    content: Vec<ContentBlock>,
    attachments: Vec<Attachment>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Message {
    id: Uuid,
    chat_id: Uuid,
    role: String,
    content: Vec<ContentBlock>,
    attachments: Vec<Attachment>,
    created_at: u64,
}

fn timestamp() -> Result<u64> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CoreError::io("System clock is before the Unix epoch"))?
        .as_millis();
    u64::try_from(ms).map_err(|_| CoreError::invalid("Timestamp overflow"))
}

fn text<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| CoreError::invalid(e.to_string()))
}

fn parse_id(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|_| CoreError::invalid("Invalid chat ID"))
}

fn chat_path(id: Uuid) -> String {
    format!("chats/{id}/chat.json")
}
fn messages_path(id: Uuid) -> String {
    format!("chats/{id}/messages.jsonl")
}

fn validate_chat(chat: &Chat, id: Uuid, project: Uuid) -> Result<()> {
    if chat.id != id
        || chat.project_id != project
        || chat.title.trim().is_empty()
        || !matches!(chat.mode.as_str(), "discuss" | "plan" | "edit")
        || chat.updated_at < chat.created_at
    {
        return Err(CoreError::invalid(format!(
            "Invalid metadata for chat {id}"
        )));
    }
    Ok(())
}

fn messages(bytes: &[u8], id: Uuid) -> Result<Vec<Message>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let body = bytes
        .strip_suffix(b"\n")
        .ok_or_else(|| CoreError::invalid(format!("Incomplete message in chat {id}")))?;
    let mut ids = BTreeSet::new();
    let mut result = Vec::new();
    for line in body.split(|b| *b == b'\n') {
        if line.is_empty() {
            return Err(CoreError::invalid(format!(
                "Empty JSONL record in chat {id}"
            )));
        }
        super::control::check()?;
        let message: Message = serde_json::from_slice(line)
            .map_err(|e| CoreError::invalid(format!("Invalid message in chat {id}: {e}")))?;
        if message.chat_id != id
            || !ids.insert(message.id)
            || !matches!(message.role.as_str(), "user" | "assistant" | "system")
        {
            return Err(CoreError::invalid(format!("Invalid message in chat {id}")));
        }
        result.push(message);
    }
    Ok(result)
}

fn encode_messages(messages: &[Message]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for message in messages {
        super::control::check()?;
        serde_json::to_writer(&mut bytes, message)
            .map_err(|e| CoreError::invalid(e.to_string()))?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn get_chat(session: &ProjectSession, state: &State, id: Uuid) -> Result<Chat> {
    let path = chat_path(id);
    let bytes = tracked_bytes(&session.path, &state.manifest, &path)
        .map_err(|e| CoreError::new(e.code, format!("Cannot read chat {id}: {}", e.message)))?;
    let chat: Chat = serde_json::from_slice(&bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid metadata for chat {id}: {e}")))?;
    validate_chat(&chat, id, state.manifest.project_id)?;
    Ok(chat)
}

fn get_messages(session: &ProjectSession, state: &State, id: Uuid) -> Result<Vec<Message>> {
    let path = messages_path(id);
    if !state.manifest.files.contains_key(&path) {
        return Err(CoreError::invalid(format!(
            "Missing messages for chat {id}"
        )));
    }
    messages(&tracked_bytes(&session.path, &state.manifest, &path)?, id)
}

pub(crate) fn chat_attachment_ids(
    session: &ProjectSession,
    state: &State,
) -> Result<BTreeSet<Uuid>> {
    let mut assets = BTreeSet::new();
    for path in state
        .manifest
        .files
        .keys()
        .filter(|path| path.starts_with("chats/") && path.ends_with("/chat.json"))
    {
        let bytes = tracked_bytes(&session.path, &state.manifest, path)?;
        let chat: Chat = serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid {path}: {e}")))?;
        if *path != chat_path(chat.id) {
            return Err(CoreError::invalid(format!(
                "Chat path does not match metadata: {path}"
            )));
        }
        validate_chat(&chat, chat.id, state.manifest.project_id)?;
        for message in get_messages(session, state, chat.id)? {
            assets.extend(
                message
                    .attachments
                    .into_iter()
                    .map(|attachment| attachment.asset_id),
            );
        }
    }
    Ok(assets)
}

fn candidate_bytes<'a>(
    root: &std::path::Path,
    files: &BTreeMap<String, String>,
    changes: &'a BTreeMap<String, Vec<u8>>,
    path: &str,
) -> Result<std::borrow::Cow<'a, [u8]>> {
    use std::borrow::Cow;
    let expected = files
        .get(path)
        .ok_or_else(|| CoreError::invalid(format!("Missing chat file {path}")))?;
    let data = match changes.get(path) {
        Some(bytes) => Cow::Borrowed(bytes.as_slice()),
        None => Cow::Owned(
            super::file_bytes(root, path)?
                .ok_or_else(|| CoreError::conflict(format!("Missing chat file {path}")))?,
        ),
    };
    if super::hash(&data) != *expected {
        return Err(CoreError::conflict(format!("Chat file changed: {path}")));
    }
    Ok(data)
}

pub(super) fn validate_candidate(
    root: &std::path::Path,
    files: &BTreeMap<String, String>,
    changes: &BTreeMap<String, Vec<u8>>,
    project: Uuid,
) -> Result<()> {
    let document: crate::model::Document =
        serde_json::from_slice(&candidate_bytes(root, files, changes, "document.json")?)
            .map_err(|e| CoreError::invalid(format!("Invalid candidate document: {e}")))?;
    let assets: BTreeSet<_> = document.assets.iter().map(|asset| asset.id).collect();
    let mut handled = BTreeSet::new();
    for path in files
        .keys()
        .filter(|path| path.starts_with("chats/") && path.ends_with("/chat.json"))
    {
        let chat: Chat = serde_json::from_slice(&candidate_bytes(root, files, changes, path)?)
            .map_err(|e| CoreError::invalid(format!("Invalid {path}: {e}")))?;
        if *path != chat_path(chat.id) {
            return Err(CoreError::invalid(format!(
                "Chat path does not match metadata: {path}"
            )));
        }
        validate_chat(&chat, chat.id, project)?;
        let message_path = messages_path(chat.id);
        for message in messages(
            &candidate_bytes(root, files, changes, &message_path)?,
            chat.id,
        )? {
            if message
                .attachments
                .iter()
                .any(|attachment| !assets.contains(&attachment.asset_id))
            {
                return Err(CoreError::new(
                    "MISSING_ASSET",
                    format!("Missing attachment in chat {}", chat.id),
                ));
            }
        }
        handled.insert(path.clone());
        handled.insert(message_path);
    }
    if let Some(path) = files
        .keys()
        .find(|path| path.starts_with("chats/") && !handled.contains(*path))
    {
        return Err(CoreError::unsupported(format!(
            "Unrecognized chat file {path}"
        )));
    }
    Ok(())
}

fn commit(
    session: &ProjectSession,
    state: &mut State,
    changes: BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let next_generation = state.next_generation()?;
    let bytes = super::commit(
        &session.path,
        &state.manifest_bytes,
        &state.manifest,
        &changes,
    )?;
    session.after_commit(&state.manifest_bytes, &bytes, state.manifest.project_id);
    state.local_generation = next_generation;
    state.manifest =
        serde_json::from_slice(&bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
    state.manifest_bytes = bytes;
    Ok(())
}

pub(crate) struct ChatCopies(Vec<(Chat, Vec<Message>)>);

pub(crate) fn rewrite_chats(
    inputs: &BTreeMap<String, Vec<u8>>,
    project: Uuid,
) -> Result<ChatCopies> {
    let mut chats = Vec::new();
    let mut handled = BTreeSet::new();
    for (path, bytes) in inputs {
        super::control::check()?;
        if !path.starts_with("chats/") || !path.ends_with("/chat.json") {
            continue;
        }
        let chat: Chat = serde_json::from_slice(bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid {path}: {e}")))?;
        if *path != chat_path(chat.id) {
            return Err(CoreError::invalid(format!(
                "Chat path does not match metadata: {path}"
            )));
        }
        validate_chat(&chat, chat.id, project)?;
        let message_path = messages_path(chat.id);
        let records = messages(
            inputs
                .get(&message_path)
                .ok_or_else(|| CoreError::invalid(format!("Missing {message_path}")))?,
            chat.id,
        )?;
        handled.insert(path.clone());
        handled.insert(message_path);
        chats.push((chat, records));
    }
    if let Some(path) = inputs
        .keys()
        .find(|path| path.starts_with("chats/") && !handled.contains(*path))
    {
        return Err(CoreError::unsupported(format!(
            "Cannot duplicate unrecognized chat file {path}"
        )));
    }
    Ok(ChatCopies(chats))
}

impl ChatCopies {
    pub(crate) fn into_files(self, project: Uuid) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut output = BTreeMap::new();
        for (mut chat, mut records) in self.0 {
            super::control::check()?;
            let id = Uuid::new_v4();
            chat.id = id;
            chat.project_id = project;
            for record in &mut records {
                super::control::check()?;
                record.id = Uuid::new_v4();
                record.chat_id = id;
            }
            output.insert(chat_path(id), json(&chat)?);
            output.insert(messages_path(id), encode_messages(&records)?);
        }
        Ok(output)
    }
}

impl ProjectSession {
    pub fn chats_json(&self) -> Result<String> {
        let state = self.state()?;
        if state.read_only_reason.is_none() {
            validate_current(&self.path, &state.manifest_bytes)?;
        }
        let mut chats = Vec::new();
        for path in state.manifest.files.keys() {
            if let Some(id) = path
                .strip_prefix("chats/")
                .and_then(|p| p.strip_suffix("/chat.json"))
            {
                let id = parse_id(id)?;
                chats.push(get_chat(self, &state, id)?);
            }
        }
        text(&chats)
    }

    pub fn create_chat(&self, title: &str, mode: &str) -> Result<String> {
        let title = title.trim();
        if title.is_empty() || title.len() > 1024 || !matches!(mode, "discuss" | "plan" | "edit") {
            return Err(CoreError::invalid("A chat needs a title and a valid mode"));
        }
        self.save()?;
        let mut state = self.state()?;
        state.ensure_writable()?;
        let now = timestamp()?;
        let chat = Chat {
            id: Uuid::new_v4(),
            project_id: state.manifest.project_id,
            title: title.into(),
            mode: mode.into(),
            created_at: now,
            updated_at: now,
            archived: false,
        };
        let mut changes = BTreeMap::new();
        changes.insert(chat_path(chat.id), json(&chat)?);
        changes.insert(messages_path(chat.id), Vec::new());
        commit(self, &mut state, changes)?;
        text(&chat)
    }

    pub fn chat_messages_json(&self, chat_id: &str) -> Result<String> {
        let id = parse_id(chat_id)?;
        let state = self.state()?;
        if state.read_only_reason.is_none() {
            validate_current(&self.path, &state.manifest_bytes)?;
        }
        get_chat(self, &state, id)?;
        text(&get_messages(self, &state, id)?)
    }

    pub fn append_chat_message(&self, chat_id: &str, message_json: &str) -> Result<String> {
        let id = parse_id(chat_id)?;
        let input: NewMessage = serde_json::from_str(message_json)
            .map_err(|e| CoreError::invalid(format!("Invalid message: {e}")))?;
        if !matches!(input.role.as_str(), "user" | "assistant" | "system")
            || (input.content.is_empty() && input.attachments.is_empty())
        {
            return Err(CoreError::invalid("Message role or content is invalid"));
        }
        self.save()?;
        let mut state = self.state()?;
        state.ensure_writable()?;
        let mut chat = get_chat(self, &state, id)?;
        if chat.archived {
            return Err(CoreError::unsupported("Chat is archived"));
        }
        let document = state.model()?;
        for attachment in &input.attachments {
            if !document
                .assets
                .iter()
                .any(|asset| asset.id == attachment.asset_id)
            {
                return Err(CoreError::new(
                    "MISSING_ASSET",
                    format!("Unknown attachment {}", attachment.asset_id),
                ));
            }
        }
        let mut records = get_messages(self, &state, id)?;
        let now = timestamp()?.max(chat.updated_at);
        let message = Message {
            id: Uuid::new_v4(),
            chat_id: id,
            role: input.role,
            content: input.content,
            attachments: input.attachments,
            created_at: now,
        };
        records.push(message.clone());
        chat.updated_at = now;
        let mut changes = BTreeMap::new();
        changes.insert(chat_path(id), json(&chat)?);
        changes.insert(messages_path(id), encode_messages(&records)?);
        commit(self, &mut state, changes)?;
        text(&message)
    }

    pub fn rename_chat(&self, chat_id: &str, title: &str) -> Result<String> {
        let title = title.trim();
        if title.is_empty() || title.len() > 1024 {
            return Err(CoreError::invalid("Invalid chat title"));
        }
        self.update_chat(chat_id, |chat| chat.title = title.into())
    }

    pub fn archive_chat(&self, chat_id: &str, archived: bool) -> Result<String> {
        self.update_chat(chat_id, |chat| chat.archived = archived)
    }

    pub fn set_chat_mode(&self, chat_id: &str, mode: &str) -> Result<String> {
        if !matches!(mode, "discuss" | "plan" | "edit") {
            return Err(CoreError::invalid("Invalid chat mode"));
        }
        self.update_chat(chat_id, |chat| chat.mode = mode.into())
    }

    pub fn search_chats_json(&self, query: &str) -> Result<String> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Err(CoreError::invalid("Search text must not be empty"));
        }
        let state = self.state()?;
        if state.read_only_reason.is_none() {
            validate_current(&self.path, &state.manifest_bytes)?;
        }
        let mut matches = Vec::new();
        for path in state.manifest.files.keys() {
            super::control::check()?;
            if let Some(id) = path
                .strip_prefix("chats/")
                .and_then(|p| p.strip_suffix("/chat.json"))
            {
                let id = parse_id(id)?;
                let chat = get_chat(self, &state, id)?;
                for message in get_messages(self, &state, id)? {
                    super::control::check()?;
                    if message.content.iter().any(|block| match block {
                        ContentBlock::Text { text } => text.to_lowercase().contains(&query),
                    }) {
                        matches.push(serde_json::json!({ "chat": chat, "message": message }));
                    }
                }
            }
        }
        text(&matches)
    }

    pub fn fork_chat(&self, chat_id: &str, message_id: &str, title: &str) -> Result<String> {
        let id = parse_id(chat_id)?;
        let message_id =
            Uuid::parse_str(message_id).map_err(|_| CoreError::invalid("Invalid message ID"))?;
        let title = title.trim();
        if title.is_empty() || title.len() > 1024 {
            return Err(CoreError::invalid("Invalid chat title"));
        }
        self.save()?;
        let mut state = self.state()?;
        state.ensure_writable()?;
        let original = get_chat(self, &state, id)?;
        let mut records = get_messages(self, &state, id)?;
        let end = records
            .iter()
            .position(|message| message.id == message_id)
            .ok_or_else(|| CoreError::invalid("Message does not belong to this chat"))?;
        records.truncate(end + 1);
        let now = timestamp()?;
        let new_id = Uuid::new_v4();
        for message in &mut records {
            message.id = Uuid::new_v4();
            message.chat_id = new_id;
        }
        let chat = Chat {
            id: new_id,
            project_id: original.project_id,
            title: title.into(),
            mode: original.mode,
            created_at: now,
            updated_at: now,
            archived: false,
        };
        let changes = BTreeMap::from([
            (chat_path(new_id), json(&chat)?),
            (messages_path(new_id), encode_messages(&records)?),
        ]);
        commit(self, &mut state, changes)?;
        text(&chat)
    }

    pub fn delete_chat(&self, chat_id: &str) -> Result<()> {
        let id = parse_id(chat_id)?;
        self.save()?;
        let mut state = self.state()?;
        state.ensure_writable()?;
        get_chat(self, &state, id)?;
        get_messages(self, &state, id)?;
        let prefix = format!("chats/{id}/");
        let permitted = BTreeSet::from([chat_path(id), messages_path(id)]);
        if state
            .manifest
            .files
            .keys()
            .any(|path| path.starts_with(&prefix) && !permitted.contains(path))
        {
            return Err(CoreError::unsupported(
                "Chat contains unsupported additional files",
            ));
        }
        if state
            .manifest
            .files
            .keys()
            .any(|path| path.starts_with("agent/"))
        {
            return Err(CoreError::unsupported(
                "Cannot delete a chat while agent records exist",
            ));
        }
        let next_generation = state.next_generation()?;
        let bytes = super::commit_with_deletions(
            &self.path,
            &state.manifest_bytes,
            &state.manifest,
            &BTreeMap::new(),
            &permitted,
        )?;
        self.after_commit(&state.manifest_bytes, &bytes, state.manifest.project_id);
        state.local_generation = next_generation;
        state.manifest =
            serde_json::from_slice(&bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
        state.manifest_bytes = bytes;
        Ok(())
    }

    fn update_chat(&self, chat_id: &str, update: impl FnOnce(&mut Chat)) -> Result<String> {
        let id = parse_id(chat_id)?;
        self.save()?;
        let mut state = self.state()?;
        state.ensure_writable()?;
        let mut chat = get_chat(self, &state, id)?;
        update(&mut chat);
        chat.updated_at = timestamp()?.max(chat.updated_at);
        let changes = BTreeMap::from([(chat_path(id), json(&chat)?)]);
        commit(self, &mut state, changes)?;
        text(&chat)
    }
}
