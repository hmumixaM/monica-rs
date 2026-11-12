//! Exact duplicate occurrence counters with a bounded in-memory front table.
//! Large exports spill to an ephemeral open-addressed file, never a CRM database.
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
};
const WIDTH: u64 = 40;
#[derive(Default)]
pub(super) struct Counter {
    memory: HashMap<[u8; 32], u32>,
    disk: Option<Disk>,
}
impl Counter {
    pub fn next(&mut self, key: [u8; 32]) -> Result<u32> {
        if self.disk.is_none() && self.memory.len() >= 8192 {
            let mut disk = Disk::new(32768)?;
            for (key, count) in self.memory.drain() {
                disk.put(key, count)?;
            }
            self.disk = Some(disk);
        }
        if let Some(disk) = &mut self.disk {
            return disk.next(key);
        }
        let count = self.memory.entry(key).or_default();
        let previous = *count;
        *count = count
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("too many duplicate messages"))?;
        Ok(previous)
    }
}
struct Disk {
    file: File,
    capacity: u64,
    used: u64,
}
impl Disk {
    fn new(capacity: u64) -> Result<Self> {
        let file = tempfile::tempfile()?;
        file.set_len(capacity * WIDTH)?;
        Ok(Self {
            file,
            capacity,
            used: 0,
        })
    }
    fn find(&mut self, key: &[u8; 32]) -> Result<(u64, Option<u32>)> {
        let mut slot = u64::from_le_bytes(key[..8].try_into().unwrap()) & (self.capacity - 1);
        for _ in 0..self.capacity {
            self.file.seek(SeekFrom::Start(slot * WIDTH))?;
            let mut entry = [0; 40];
            self.file.read_exact(&mut entry)?;
            if entry[36] == 0 {
                return Ok((slot, None));
            }
            if entry[..32] == *key {
                return Ok((
                    slot,
                    Some(u32::from_le_bytes(entry[32..36].try_into().unwrap())),
                ));
            }
            slot = (slot + 1) & (self.capacity - 1);
        }
        anyhow::bail!("export occurrence table full")
    }
    fn put(&mut self, key: [u8; 32], count: u32) -> Result<()> {
        let (slot, old) = self.find(&key)?;
        let mut entry = [0; 40];
        entry[..32].copy_from_slice(&key);
        entry[32..36].copy_from_slice(&count.to_le_bytes());
        entry[36] = 1;
        self.file.seek(SeekFrom::Start(slot * WIDTH))?;
        self.file.write_all(&entry)?;
        if old.is_none() {
            self.used += 1
        }
        Ok(())
    }
    fn next(&mut self, key: [u8; 32]) -> Result<u32> {
        if self.used * 2 >= self.capacity {
            self.grow()?
        }
        let (_, count) = self.find(&key)?;
        let count = count.unwrap_or(0);
        ensure!(count < u32::MAX, "too many duplicate messages");
        self.put(key, count + 1)?;
        Ok(count)
    }
    fn grow(&mut self) -> Result<()> {
        let mut next = Self::new(
            self.capacity
                .checked_mul(2)
                .ok_or_else(|| anyhow::anyhow!("export occurrence table overflow"))?,
        )?;
        self.file.seek(SeekFrom::Start(0))?;
        for _ in 0..self.capacity {
            let mut entry = [0; 40];
            self.file.read_exact(&mut entry)?;
            if entry[36] != 0 {
                next.put(
                    entry[..32].try_into().unwrap(),
                    u32::from_le_bytes(entry[32..36].try_into().unwrap()),
                )?
            }
        }
        *self = next;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn spill_preserves_counts_after_resize() {
        let mut counter = Counter::default();
        for i in 0..20_000u32 {
            let key = Sha256::digest(i.to_le_bytes()).into();
            assert_eq!(counter.next(key).unwrap(), 0)
        }
        assert!(counter.disk.is_some());
        assert!(counter.memory.is_empty());
        for i in [0, 8191, 8192, 19999] {
            let key = Sha256::digest((i as u32).to_le_bytes()).into();
            assert_eq!(counter.next(key).unwrap(), 1);
            assert_eq!(counter.next(key).unwrap(), 2);
        }
    }
}
