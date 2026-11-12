use serde_json::Value;
use std::{
    ffi::{CStr, CString},
    fs,
    path::Path,
    sync::Mutex,
};
static CONTACTS_LOCK: Mutex<()> = Mutex::new(());
static MESSAGES_LOCK: Mutex<()> = Mutex::new(());

unsafe extern "C" {
    fn monica_contacts_read(
        token: *const std::os::raw::c_char,
        prompt: i32,
    ) -> *mut std::os::raw::c_char;
}
unsafe extern "C" {
    fn monica_messages_read(
        token: *const std::os::raw::c_char,
        prompt: i32,
    ) -> *mut std::os::raw::c_char;
}
unsafe extern "C" {
    fn monica_contacts_revision() -> u64;
}

pub fn contacts_revision() -> u64 {
    unsafe { monica_contacts_revision() }
}

pub struct MessagesWatch {
    signature: Vec<(u64, Option<std::time::SystemTime>)>,
    passes: usize,
}

impl Default for MessagesWatch {
    fn default() -> Self {
        Self::new()
    }
}
impl MessagesWatch {
    pub fn new() -> Self {
        Self {
            signature: Vec::new(),
            passes: 6,
        }
    }

    pub fn poll(&mut self, root: &Path, reconcile: bool) -> Result<(), String> {
        let folder = root.join("raw/apple_messages");
        if folder.join("pending.json").exists() {
            return Ok(());
        }
        if reconcile && collect_attachment_retries(&folder)? {
            return Ok(());
        }
        let home = std::env::var("HOME").map_err(|e| e.to_string())?;
        let signature: Vec<_> = ["chat.db", "chat.db-wal"]
            .iter()
            .map(|name| {
                fs::metadata(Path::new(&home).join("Library/Messages").join(name))
                    .map(|m| (m.len(), m.modified().ok()))
                    .unwrap_or((0, None))
            })
            .collect();
        if reconcile || signature != self.signature {
            self.passes = 6;
            self.signature = signature;
        }
        let cursor = match fs::read(folder.join("cursor.json")) {
            Ok(raw) => serde_json::from_slice::<Value>(&raw).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
            Err(e) => return Err(e.to_string()),
        };
        let backfill = cursor["recent_complete"] != true || cursor["history_complete"] != true;
        if self.passes == 0 && !backfill {
            return Ok(());
        }
        collect_messages(root)?;
        let value: Value = serde_json::from_slice(
            &fs::read(folder.join("pending.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if value["records"].as_array().map_or(0, Vec::len) == 100 {
            self.passes = 6;
        } else {
            self.passes = self.passes.saturating_sub(1);
        }
        Ok(())
    }
}

pub fn collect_contacts(root: &Path, prompt: bool) -> Result<(), String> {
    collect(root, "apple_contacts", prompt)
}

pub fn collect_messages(root: &Path) -> Result<(), String> {
    collect(root, "apple_messages", false)
}

fn collect(root: &Path, provider: &str, prompt: bool) -> Result<(), String> {
    // A system Contacts consent sheet may remain open indefinitely. Its lock
    // belongs only to Contacts; background polling must still deliver Messages.
    let lock = if provider == "apple_contacts" {
        &CONTACTS_LOCK
    } else {
        &MESSAGES_LOCK
    };
    let _lock = if provider == "apple_contacts" && !prompt {
        match lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
    } else {
        lock.lock().map_err(|e| e.to_string())?
    };
    let folder = root.join("raw").join(provider);
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    if folder.join("pending.json").exists() {
        return Ok(());
    }
    let previous = match fs::read_to_string(folder.join("cursor.json")) {
        Ok(value) => {
            if provider == "apple_contacts" {
                serde_json::from_str::<Value>(&value).map_err(|e| e.to_string())?["token"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned()
            } else {
                value
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let token = CString::new(previous).map_err(|e| e.to_string())?;
    let result = unsafe {
        let raw = if provider == "apple_contacts" {
            monica_contacts_read(token.as_ptr(), i32::from(prompt))
        } else {
            monica_messages_read(token.as_ptr(), 0)
        };
        if raw.is_null() {
            return Err("通讯录读取失败".into());
        }
        let value = CStr::from_ptr(raw).to_string_lossy().into_owned();
        libc::free(raw.cast());
        value
    };
    let mut value: Value = serde_json::from_str(&result).map_err(|e| e.to_string())?;
    if let Some(error) = value["error"].as_str() {
        return Err(error.to_owned());
    }
    if provider == "apple_messages" {
        archive_attachments(&folder, &mut value)?;
    }
    let result = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = folder.join("pending.tmp");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(&result).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    fs::rename(temporary, folder.join("pending.json")).map_err(|e| e.to_string())?;
    Ok(())
}

fn archive_attachments(folder: &Path, value: &mut Value) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    let directory = folder.join("attachments");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    if let Some(records) = value["records"].as_array_mut() {
        for record in records {
            if let Some(attachments) = record["attachments"].as_array_mut() {
                for attachment in attachments {
                    if attachment["blob"].is_string() {
                        continue;
                    }
                    let source = attachment["path"].as_str().unwrap_or("");
                    if source.is_empty() {
                        attachment["availability"] = "not_on_this_mac".into();
                        continue;
                    }
                    let path = if let Some(tail) = source.strip_prefix("~/") {
                        Path::new(&home).join(tail)
                    } else {
                        source.into()
                    };
                    let mut input = match fs::File::open(path) {
                        Ok(file) => file,
                        Err(error) => {
                            attachment["availability"] = format!("{:?}", error.kind()).into();
                            continue;
                        }
                    };
                    let temporary = directory.join("copy.tmp");
                    let mut output = fs::OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .write(true)
                        .mode(0o600)
                        .open(&temporary)
                        .map_err(|e| e.to_string())?;
                    let mut hash = Sha256::new();
                    let mut size = 0u64;
                    let mut buffer = vec![0u8; 1024 * 1024];
                    loop {
                        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
                        if count == 0 {
                            break;
                        }
                        output
                            .write_all(&buffer[..count])
                            .map_err(|e| e.to_string())?;
                        hash.update(&buffer[..count]);
                        size += count as u64;
                    }
                    output.sync_all().map_err(|e| e.to_string())?;
                    let digest = format!("{:x}", hash.finalize());
                    fs::rename(&temporary, directory.join(&digest)).map_err(|e| e.to_string())?;
                    attachment["local_blob"] = digest.into();
                    attachment["size"] = size.into();
                    attachment["availability"] = "downloaded".into();
                }
            }
        }
    }
    Ok(())
}

fn collect_attachment_retries(folder: &Path) -> Result<bool, String> {
    let _lock = MESSAGES_LOCK.lock().map_err(|e| e.to_string())?;
    let path = folder.join("attachment-retries.json");
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if folder.join("pending.json").exists() {
        return Ok(false);
    }
    let mut value: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    archive_attachments(folder, &mut value)?;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = folder.join("pending.tmp");
    let mut output = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    output
        .write_all(&serde_json::to_vec(&value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())?;
    fs::rename(temporary, folder.join("pending.json")).map_err(|e| e.to_string())?;
    fs::remove_file(path).map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archives_available_attachments_and_records_missing_files() {
        let root = std::env::temp_dir().join(format!(
            "monica-native-media-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let original = root.join("original.bin");
        fs::write(&original, b"native attachment").unwrap();
        let mut value = serde_json::json!({"records":[{"attachments":[
            {"path":original,"filename":"original.bin"},{"path":root.join("missing.bin")}]}]});
        archive_attachments(&root, &mut value).unwrap();
        let attachment = &value["records"][0]["attachments"][0];
        let blob = attachment["local_blob"].as_str().unwrap();
        assert_eq!(
            fs::read(root.join("attachments").join(blob)).unwrap(),
            b"native attachment"
        );
        assert_eq!(attachment["size"], 17);
        assert_eq!(
            value["records"][0]["attachments"][1]["availability"],
            "NotFound"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
