//! Bounded offline WhatsApp import. The uploaded original is durable before journals reference it.
use super::{Result, invalid, operation};
use crate::auth::{AppState, user};
use axum::{
    Json,
    extract::{Multipart, State},
    http::HeaderMap,
};
use monica_core::{
    Provider, connections,
    media::MediaStore,
    source_blobs,
    whatsapp::{ExportOptions, PhoneExport, ZipExport},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{BufReader, Read, Write},
    path::Path,
};
use tokio::io::AsyncWriteExt;

struct Parsed {
    record: Value,
    attachments: Vec<(usize, tempfile::NamedTempFile)>,
}
fn parser(
    path: &Path,
    zip: bool,
    options: ExportOptions,
) -> anyhow::Result<PhoneExport<BufReader<Box<dyn Read>>>> {
    // ZIP text is copied to a temporary file so each record can open its attachment
    // without retaining a borrow on the archive or buffering the entire text in RAM.
    let reader: Box<dyn Read> = if zip {
        let mut archive = ZipExport::open(path)?;
        let mut file = tempfile::tempfile()?;
        let n = std::io::copy(&mut archive.text()?.take(512 * 1024 * 1024 + 1), &mut file)?;
        anyhow::ensure!(n <= 512 * 1024 * 1024, "export text exceeds 512 MiB");
        std::io::Seek::rewind(&mut file)?;
        Box::new(file)
    } else {
        Box::new(std::fs::File::open(path)?)
    };
    PhoneExport::new(BufReader::new(reader), options)
}
fn parse(
    path: &Path,
    zip: bool,
    mut options: ExportOptions,
    send: tokio::sync::mpsc::Sender<anyhow::Result<Parsed>>,
) -> anyhow::Result<()> {
    if options.conversation_kind == "direct" {
        let mut names = BTreeSet::from([options.owner.clone()]);
        for record in parser(path, zip, options.clone())? {
            if send.is_closed() {
                return Ok(());
            }
            let record = record?;
            for person in record["participants"].as_array().into_iter().flatten() {
                if let Some(name) = person["name"].as_str() {
                    anyhow::ensure!(name.len() <= 4096, "export author name exceeds limit");
                    names.insert(name.to_string());
                }
            }
            anyhow::ensure!(
                names.len() <= 2,
                "direct export requires exactly one other author"
            );
        }
        anyhow::ensure!(
            names.len() == 2,
            "direct export requires exactly one other author"
        );
        options.participants = names.into_iter().collect();
    }
    let mut archive = if zip {
        Some(ZipExport::open(path)?)
    } else {
        None
    };
    for record in parser(path, zip, options)? {
        let record = record?;
        let mut attachments = Vec::new();
        if let Some(archive) = &mut archive {
            for (index, attachment) in record["attachments"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(name) = attachment["filename"].as_str()
                    && let Some(mut input) = archive.attachment(name)?
                {
                    let mut temp = tempfile::NamedTempFile::new()?;
                    let n = std::io::copy(
                        &mut input.by_ref().take(monica_core::media::MAX_FILE + 1),
                        &mut temp,
                    )?;
                    anyhow::ensure!(
                        n <= monica_core::media::MAX_FILE,
                        "attachment exceeds storage limit"
                    );
                    temp.flush()?;
                    temp.as_file().sync_all()?;
                    attachments.push((index, temp));
                }
            }
        }
        if send
            .blocking_send(Ok(Parsed {
                record,
                attachments,
            }))
            .is_err()
        {
            return Ok(());
        }
    }
    Ok(())
}
pub async fn import(
    State(s): State<AppState>,
    h: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let directory = s.media_root.join(".staging");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|e| operation(e.into()))?;
    let mut original_file = None;
    let mut filename = String::new();
    let mut content_type = String::new();
    let mut fields = serde_json::Map::new();
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| invalid("invalid multipart upload"))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            if original_file.is_some() {
                return Err(invalid("one export file required"));
            }
            filename = field
                .file_name()
                .unwrap_or("whatsapp-export.txt")
                .to_string();
            content_type = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();
            let temp =
                tempfile::NamedTempFile::new_in(&directory).map_err(|e| operation(e.into()))?;
            let mut file = tokio::fs::File::create(temp.path())
                .await
                .map_err(|e| operation(e.into()))?;
            let mut size = 0u64;
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|_| invalid("invalid export upload"))?
            {
                size += chunk.len() as u64;
                if size > monica_core::media::MAX_FILE {
                    return Err(invalid("export exceeds storage limit"));
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|e| operation(e.into()))?;
            }
            file.sync_all().await.map_err(|e| operation(e.into()))?;
            original_file = Some(temp);
        } else {
            let bytes = field
                .bytes()
                .await
                .map_err(|_| invalid("invalid form field"))?;
            if bytes.len() > 4096 {
                return Err(invalid("form field too long"));
            }
            fields.insert(
                name,
                json!(
                    std::str::from_utf8(&bytes).map_err(|_| invalid("form field must be UTF-8"))?
                ),
            );
        }
    }
    let original_file = original_file.ok_or(invalid("export file required"))?;
    let field = |name: &str, default: &str| {
        fields
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .trim()
            .to_string()
    };
    let options = ExportOptions {
        conversation: field("conversation", ""),
        owner: field("owner", ""),
        timezone: field("timezone", "America/Chicago"),
        date_order: field("date_order", "mdy"),
        conversation_kind: field("conversation_kind", "group"),
        participants: vec![],
    };
    if options.conversation.is_empty() || options.owner.is_empty() {
        return Err(invalid(
            "Conversation and your export display name are required",
        ));
    }
    let account = format!("phone-export:{}", options.owner);
    let cid = connections::connection_id(Provider::Whatsapp, &account);
    let mut tx = s.pool.begin().await?;
    connections::lock_connection(&mut tx, cid)
        .await
        .map_err(operation)?;
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,state) VALUES($1,'whatsapp',$2,$3,'archive',true,'connecting') ON CONFLICT(id) DO UPDATE SET enabled=true,next_sync_at=now(),sync_pending=true").bind(cid).bind(account).bind(format!("WhatsApp 手机导出 · {}",options.owner)).execute(&mut *tx).await?;
    tx.commit().await?;
    let store = MediaStore::new(s.pool.clone(), s.media_root.clone(), s.resources.clone())
        .map_err(operation)?;
    let original = source_blobs::archive_path(&store, "whatsapp", cid, original_file.path())
        .await
        .map_err(operation)?;
    let original = json!({"blob":original.blob,"size":original.size,"filename":filename,"content_type":content_type});
    let mut signature = [0; 4];
    let zip = std::fs::File::open(original_file.path())
        .map_err(|e| operation(e.into()))?
        .read(&mut signature)
        .map_err(|e| operation(e.into()))?
        == 4
        && signature == *b"PK\x03\x04";
    let (send, mut receive) = tokio::sync::mpsc::channel(2);
    let resources = s.resources.clone();
    let permit = resources
        .acquire(monica_core::resources::TaskResources {
            cpu: 1,
            memory_bytes: 32 * 1024 * 1024,
            ..Default::default()
        })
        .await
        .map_err(operation)?;
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let errors = send.clone();
        if let Err(error) = parse(original_file.path(), zip, options, send) {
            let _ = errors.blocking_send(Err(error));
        }
    });
    let mut batch = Vec::new();
    let mut count = 0u64;
    let mut bytes = 0usize;
    while let Some(record) = receive.recv().await {
        let mut parsed=record.map_err(|_|invalid("Export could not be fully parsed. Check the date order, timezone and direct/group selection; already imported pages are safe to retry."))?;
        for (index, file) in parsed.attachments {
            let blob = source_blobs::archive_path(&store, "whatsapp", cid, file.path())
                .await
                .map_err(operation)?;
            let attachment = &mut parsed.record["attachments"][index];
            attachment["blob"] = json!(blob.blob);
            attachment["size"] = json!(blob.size);
            attachment["availability"] = json!("downloaded");
        }
        parsed.record["original"] = original.clone();
        let size = serde_json::to_vec(&parsed.record)
            .map_err(|e| operation(e.into()))?
            .len();
        if size > 4 * 1024 * 1024 {
            return Err(invalid("export record exceeds batch limit"));
        }
        if !batch.is_empty() && bytes + size > 4 * 1024 * 1024 {
            save(&s, cid, &mut batch, count).await?;
            bytes = 0;
        }
        batch.push(parsed.record);
        bytes += size;
        count += 1;
        if batch.len() == 100 {
            save(&s, cid, &mut batch, count).await?;
            bytes = 0;
        }
    }
    task.await.map_err(|e| operation(e.into()))?;
    if count == 0 {
        return Err(invalid("No WhatsApp messages found"));
    }
    save(&s, cid, &mut batch, count).await?;
    Ok(Json(
        json!({"connection_id":cid,"records_read":count,"coverage":"仅此导出文件包含的历史"}),
    ))
}
async fn save(s: &AppState, cid: uuid::Uuid, batch: &mut Vec<Value>, count: u64) -> Result<()> {
    let mut tx = s.pool.begin().await?;
    connections::commit_page(
        &mut tx,
        Provider::Whatsapp,
        cid,
        "phone-export",
        batch,
        &json!({"records_read":count}),
    )
    .await
    .map_err(operation)?;
    tx.commit().await?;
    batch.clear();
    connections::deliver(&s.pool, Provider::Whatsapp, cid, 100)
        .await
        .map_err(operation)?;
    Ok(())
}
