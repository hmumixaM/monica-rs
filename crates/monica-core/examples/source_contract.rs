//! Offline contract probe, used only with synthetic JSON fixtures.
use anyhow::Result;
use monica_core::{
    mail,
    whatsapp::{ExportOptions, PhoneExport},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, BufRead},
};
fn run(v: Value) -> Result<Value> {
    match v["op"].as_str().unwrap_or("") {
        "classify" => {
            let d = mail::classify(&v["record"], v["rule"].as_str(), v["reply"] == true)?;
            Ok(json!({"category":d.category,"reason":d.reason}))
        }
        "normalize" => mail::normalize(
            v["record"].clone(),
            &v["owners"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>(),
            &mail::Decision {
                category: v["category"].as_str().unwrap_or("pending").into(),
                reason: v["reason"]
                    .as_str()
                    .unwrap_or("needs_classification")
                    .into(),
            },
        ),
        "fingerprint" => Ok(json!(mail::fingerprint(&v["record"]))),
        "phone" => {
            let o = &v["options"];
            let options = ExportOptions {
                conversation: o["conversation"].as_str().unwrap().into(),
                owner: o["owner"].as_str().unwrap().into(),
                timezone: o["timezone"].as_str().unwrap().into(),
                date_order: o["date_order"].as_str().unwrap().into(),
                conversation_kind: o["conversation_kind"].as_str().unwrap_or("group").into(),
                participants: o["participants"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            };
            Ok(json!(
                PhoneExport::new(io::Cursor::new(v["text"].as_str().unwrap()), options)?
                    .collect::<Result<Vec<_>>>()?
            ))
        }
        _ => anyhow::bail!("unknown operation"),
    }
}
fn main() -> Result<()> {
    for line in io::stdin().lock().lines() {
        let v = serde_json::from_str(&line?)?;
        let result = run(v)?;
        println!("{}", serde_json::to_string(&result)?);
    }
    Ok(())
}
