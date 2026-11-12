//! Strict PostgreSQL pgoutput protocol v1 decoder. Unknown/truncated input is
//! rejected before any slot advancement. JSON types keep their JSON values.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
#[derive(Debug)]
pub struct Change {
    pub schema: String,
    pub relation: String,
    pub operation: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}
#[derive(Debug)]
pub struct Batch {
    pub commit_lsn: String,
    pub end_lsn: String,
    pub committed_at: chrono::DateTime<chrono::Utc>,
    pub changes: Vec<Change>,
}
fn lsn(n: u64) -> String {
    format!("{:X}/{:X}", n >> 32, n & 0xffff_ffff)
}
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).context("pgoutput length overflow")?;
        let value = self
            .data
            .get(self.at..end)
            .context("truncated pgoutput message")?;
        self.at = end;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into()?))
    }
    fn string(&mut self) -> Result<String> {
        let n = self.data[self.at..]
            .iter()
            .position(|b| *b == 0)
            .context("unterminated pgoutput string")?;
        let bytes = self.take(n)?;
        self.take(1)?;
        Ok(std::str::from_utf8(bytes)?.into())
    }
    fn tuple(&mut self, columns: &[(String, u32)]) -> Result<Map<String, Value>> {
        ensure!(
            self.u16()? as usize == columns.len(),
            "unexpected pgoutput tuple width"
        );
        let mut value = Map::new();
        let mut toast = vec![];
        for (name, oid) in columns {
            match self.byte()? {
                b'n' => {
                    value.insert(name.clone(), Value::Null);
                }
                b'u' => toast.push(name.clone()),
                b't' => {
                    let n = self.u32()? as usize;
                    let raw = std::str::from_utf8(self.take(n)?)?;
                    value.insert(
                        name.clone(),
                        if [114, 3802].contains(oid) {
                            serde_json::from_str(raw)?
                        } else {
                            json!(raw)
                        },
                    );
                }
                _ => bail!("unsupported pgoutput tuple encoding"),
            }
        }
        if !toast.is_empty() {
            value.insert("_unchanged_toast_columns".into(), json!(toast));
        }
        Ok(value)
    }
}
type Relation = (String, String, Vec<(String, u32)>);
#[derive(Default)]
pub struct Decoder {
    relations: HashMap<u32, Relation>,
    active: bool,
    changes: Vec<Change>,
}
impl Decoder {
    pub fn feed(&mut self, data: &[u8]) -> Result<Option<Batch>> {
        let mut r = Reader { data, at: 0 };
        let kind = r.byte()?;
        let mut batch = None;
        match kind {
            b'B' => {
                ensure!(!self.active, "nested pgoutput transaction");
                r.take(20)?;
                self.active = true;
                self.changes.clear();
            }
            b'R' => {
                let id = r.u32()?;
                let schema = r.string()?;
                let name = r.string()?;
                r.take(1)?;
                let n = r.u16()?;
                let mut columns = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    r.take(1)?;
                    let column = r.string()?;
                    let oid = r.u32()?;
                    r.take(4)?;
                    columns.push((column, oid));
                }
                self.relations.insert(id, (schema, name, columns));
            }
            b'I' | b'U' | b'D' => {
                ensure!(self.active, "row outside pgoutput transaction");
                let id = r.u32()?;
                let (schema, name, columns) = self
                    .relations
                    .get(&id)
                    .context("unknown pgoutput relation")?;
                let mut old = None;
                let mut new = None;
                let mut marker = Some(r.byte()?);
                if matches!(marker, Some(b'K' | b'O')) {
                    old = Some(r.tuple(columns)?);
                    marker = if kind == b'U' { Some(r.byte()?) } else { None };
                }
                if marker == Some(b'N') {
                    new = Some(r.tuple(columns)?);
                } else {
                    ensure!(kind == b'D', "missing pgoutput new tuple");
                }
                if let Some(new) = &mut new
                    && let Some(Value::Array(toast)) = new.remove("_unchanged_toast_columns")
                {
                    for name in toast {
                        let name = name.as_str().context("invalid toast column")?;
                        let previous = old
                            .as_ref()
                            .and_then(|o| o.get(name))
                            .context("unchanged TOAST lacks full before image")?;
                        new.insert(name.into(), previous.clone());
                    }
                }
                self.changes.push(Change {
                    schema: schema.clone(),
                    relation: name.clone(),
                    operation: (kind as char).to_string(),
                    before: old.map(Value::Object),
                    after: new.map(Value::Object),
                });
            }
            b'T' => {
                ensure!(self.active, "truncate outside pgoutput transaction");
                let count = r.u32()?;
                r.take(1)?;
                for _ in 0..count {
                    let id = r.u32()?;
                    let (schema, name, _) = self
                        .relations
                        .get(&id)
                        .context("unknown truncated relation")?;
                    self.changes.push(Change {
                        schema: schema.clone(),
                        relation: name.clone(),
                        operation: "T".into(),
                        before: None,
                        after: None,
                    });
                }
            }
            b'C' => {
                ensure!(self.active, "commit outside pgoutput transaction");
                r.take(1)?;
                let commit = r.u64()?;
                let end = r.u64()?;
                let micros = r.u64()? as i64;
                let epoch = chrono::DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z")?
                    .with_timezone(&chrono::Utc);
                let committed_at = epoch
                    .checked_add_signed(chrono::Duration::microseconds(micros))
                    .context("invalid commit timestamp")?;
                batch = Some(Batch {
                    commit_lsn: lsn(commit),
                    end_lsn: lsn(end),
                    committed_at,
                    changes: std::mem::take(&mut self.changes),
                });
                self.active = false;
            }
            b'Y' => {
                r.take(4)?;
                r.string()?;
                r.string()?;
            }
            b'O' => {
                r.take(8)?;
                r.string()?;
            }
            _ => bail!("unsupported pgoutput message"),
        }
        ensure!(r.at == data.len(), "trailing pgoutput bytes");
        Ok(batch)
    }
    pub fn finish(self) -> Result<()> {
        ensure!(!self.active, "incomplete pgoutput transaction");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_or_incomplete_protocol_never_acknowledges() {
        for data in [
            vec![],
            vec![b'R'],
            vec![b'I', 0, 0, 0, 1, b'N'],
            vec![b'X'],
            vec![b'C', 0],
        ] {
            assert!(Decoder::default().feed(&data).is_err());
        }
        let mut decoder = Decoder::default();
        let mut begin = vec![b'B'];
        begin.extend([0; 20]);
        decoder.feed(&begin).unwrap();
        assert!(decoder.finish().is_err());
        let mut decoder = Decoder::default();
        decoder.feed(&begin).unwrap();
        assert!(decoder.feed(&begin).is_err());
    }
}
