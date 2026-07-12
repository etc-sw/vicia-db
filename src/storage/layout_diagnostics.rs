#![allow(missing_docs)]

use crate::storage::btree_v6::{PAGE_TYPE_INTERNAL, PAGE_TYPE_LEAF};
use crate::storage::header_extension::HeaderExtension;
use crate::storage::packed_pages::{PACKED_HEADER_SIZE, PAGE_TYPE_PACKED};
use crate::storage::{FileHeader, PAGE_SIZE};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoragePageLayout {
    pub pages: u64,
    pub entries: u64,
    pub payload_bytes: u64,
    pub structural_bytes: u64,
    pub unused_bytes: u64,
}

impl StoragePageLayout {
    pub fn allocated_bytes(&self) -> u64 {
        self.pages.saturating_mul(PAGE_SIZE as u64)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefixEstimate {
    pub restart_interval: u64,
    pub estimated_saved_bytes: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageIndexLayout {
    pub root_page: u64,
    pub height: u64,
    pub leaf: StoragePageLayout,
    pub internal: StoragePageLayout,
    pub prefix_restart_10: PrefixEstimate,
    pub prefix_restart_16: PrefixEstimate,
}

impl StorageIndexLayout {
    pub fn allocated_bytes(&self) -> u64 {
        self.leaf
            .allocated_bytes()
            .saturating_add(self.internal.allocated_bytes())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLayoutDiagnostics {
    pub format_version: u32,
    pub published_pages: u64,
    pub published_bytes: u64,
    pub fact_page_start: u64,
    pub facts: StoragePageLayout,
    pub eavt: StorageIndexLayout,
    pub aevt: StorageIndexLayout,
    pub avet: StorageIndexLayout,
    pub vaet: StorageIndexLayout,
    pub header_bytes: u64,
    pub other_published_bytes: u64,
}

pub fn inspect_storage_layout(path: impl AsRef<Path>) -> Result<StorageLayoutDiagnostics> {
    let mut file = File::open(path.as_ref())?;
    let page0 = read_page(&mut file, 0)?;
    let header = FileHeader::from_bytes(&page0)?;
    let extension = HeaderExtension::read_from_page0(header.version, &page0)?
        .context("current storage layout requires a header extension")?;
    let fact_page_start = extension.base_fact_page_start();
    let facts = inspect_facts(
        &mut file,
        fact_page_start,
        header.fact_page_count,
        header.page_count,
    )?;

    let mut owners = HashMap::new();
    let eavt = inspect_index(
        &mut file,
        header.eavt_root_page,
        header.page_count,
        "eavt",
        &mut owners,
    )?;
    let aevt = inspect_index(
        &mut file,
        header.aevt_root_page,
        header.page_count,
        "aevt",
        &mut owners,
    )?;
    let avet = inspect_index(
        &mut file,
        header.avet_root_page,
        header.page_count,
        "avet",
        &mut owners,
    )?;
    let vaet = inspect_index(
        &mut file,
        header.vaet_root_page,
        header.page_count,
        "vaet",
        &mut owners,
    )?;
    let classified_pages = 1_u64
        .saturating_add(facts.pages)
        .saturating_add(u64::try_from(owners.len()).unwrap_or(u64::MAX));
    let other_published_bytes = header
        .page_count
        .saturating_sub(classified_pages)
        .saturating_mul(PAGE_SIZE as u64);
    Ok(StorageLayoutDiagnostics {
        format_version: header.version,
        published_pages: header.page_count,
        published_bytes: header.page_count.saturating_mul(PAGE_SIZE as u64),
        fact_page_start,
        facts,
        eavt,
        aevt,
        avet,
        vaet,
        header_bytes: PAGE_SIZE as u64,
        other_published_bytes,
    })
}

fn inspect_facts(
    file: &mut File,
    start: u64,
    count: u64,
    published: u64,
) -> Result<StoragePageLayout> {
    let end = start
        .checked_add(count)
        .context("fact page range overflow")?;
    if start == 0 || end > published {
        anyhow::bail!("fact page range is outside the published image")
    }
    let mut result = StoragePageLayout::default();
    for page_id in start..end {
        let page = read_page(file, page_id)?;
        if page.first().copied() != Some(PAGE_TYPE_PACKED) {
            anyhow::bail!("fact page {page_id} has the wrong page type")
        }
        let entries = read_u16(&page, 2)? as usize;
        let mut payload = 0usize;
        for slot in 0..entries {
            payload = payload
                .saturating_add(read_u16(&page, PACKED_HEADER_SIZE + slot * 4 + 2)? as usize);
        }
        let structural = PACKED_HEADER_SIZE.saturating_add(entries.saturating_mul(4));
        add_page(&mut result, entries, payload, structural)?;
    }
    Ok(result)
}

fn inspect_index(
    file: &mut File,
    root: u64,
    published: u64,
    name: &'static str,
    owners: &mut HashMap<u64, &'static str>,
) -> Result<StorageIndexLayout> {
    if root == 0 || root >= published {
        anyhow::bail!("{name} root is outside the published image")
    }
    let mut result = StorageIndexLayout {
        root_page: root,
        prefix_restart_10: PrefixEstimate {
            restart_interval: 10,
            estimated_saved_bytes: 0,
        },
        prefix_restart_16: PrefixEstimate {
            restart_interval: 16,
            estimated_saved_bytes: 0,
        },
        ..StorageIndexLayout::default()
    };
    let mut visiting = HashSet::new();
    result.height = inspect_index_page(
        file,
        root,
        published,
        name,
        owners,
        &mut visiting,
        &mut result,
    )?;
    Ok(result)
}

fn inspect_index_page(
    file: &mut File,
    page_id: u64,
    published: u64,
    name: &'static str,
    owners: &mut HashMap<u64, &'static str>,
    visiting: &mut HashSet<u64>,
    result: &mut StorageIndexLayout,
) -> Result<u64> {
    if page_id == 0 || page_id >= published {
        anyhow::bail!("{name} child page {page_id} is outside the published image")
    }
    if !visiting.insert(page_id) {
        anyhow::bail!("{name} index contains a page cycle")
    }
    if let Some(previous) = owners.insert(page_id, name) {
        anyhow::bail!("index page {page_id} is shared by {previous} and {name}")
    }
    let page = read_page(file, page_id)?;
    let count = read_u16(&page, 2)? as usize;
    let height = match page.first().copied() {
        Some(PAGE_TYPE_LEAF) => {
            let entries = read_entries(&page, 12, count)?;
            let payload = entries.iter().map(Vec::len).sum();
            add_page(&mut result.leaf, count, payload, 12 + count * 4)?;
            result.prefix_restart_10.estimated_saved_bytes = result
                .prefix_restart_10
                .estimated_saved_bytes
                .saturating_add(prefix_savings(&entries, 10));
            result.prefix_restart_16.estimated_saved_bytes = result
                .prefix_restart_16
                .estimated_saved_bytes
                .saturating_add(prefix_savings(&entries, 16));
            1
        }
        Some(PAGE_TYPE_INTERNAL) => {
            let slots = 12 + count * 8;
            let entries = read_entries(&page, slots, count)?;
            let payload = entries.iter().map(Vec::len).sum();
            add_page(&mut result.internal, count, payload, slots + count * 4)?;
            let mut child_height = None;
            for index in 0..=count {
                let child = if index == count {
                    read_u64(&page, 4)?
                } else {
                    read_u64(&page, 12 + index * 8)?
                };
                let height =
                    inspect_index_page(file, child, published, name, owners, visiting, result)?;
                if child_height
                    .replace(height)
                    .is_some_and(|prior| prior != height)
                {
                    anyhow::bail!("{name} index has inconsistent child heights")
                }
            }
            child_height.unwrap_or(0).saturating_add(1)
        }
        Some(other) => anyhow::bail!("{name} page {page_id} has unknown type 0x{other:02x}"),
        None => anyhow::bail!("{name} page {page_id} is empty"),
    };
    visiting.remove(&page_id);
    Ok(height)
}

fn read_entries(page: &[u8], slots: usize, count: usize) -> Result<Vec<Vec<u8>>> {
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let offset = read_u16(page, slots + index * 4)? as usize;
        let length = read_u16(page, slots + index * 4 + 2)? as usize;
        entries.push(
            page.get(offset..offset.saturating_add(length))
                .context("index entry is outside its page")?
                .to_vec(),
        );
    }
    Ok(entries)
}

fn prefix_savings(entries: &[Vec<u8>], restart: usize) -> u64 {
    entries
        .iter()
        .enumerate()
        .fold(
            (0_u64, None::<&[u8]>),
            |(saved, previous), (index, entry)| {
                if index % restart == 0 {
                    return (saved, Some(entry));
                }
                let common = previous
                    .unwrap_or_default()
                    .iter()
                    .zip(entry)
                    .take_while(|(left, right)| left == right)
                    .count();
                (
                    saved.saturating_add(
                        u64::try_from(common.saturating_sub(4)).unwrap_or(u64::MAX),
                    ),
                    Some(entry),
                )
            },
        )
        .0
}

fn add_page(
    target: &mut StoragePageLayout,
    entries: usize,
    payload: usize,
    structural: usize,
) -> Result<()> {
    let used = payload
        .checked_add(structural)
        .context("page byte accounting overflow")?;
    if used > PAGE_SIZE {
        anyhow::bail!("page accounting exceeds page size")
    }
    target.pages = target.pages.saturating_add(1);
    target.entries = target.entries.saturating_add(u64::try_from(entries)?);
    target.payload_bytes = target.payload_bytes.saturating_add(u64::try_from(payload)?);
    target.structural_bytes = target
        .structural_bytes
        .saturating_add(u64::try_from(structural)?);
    target.unused_bytes = target
        .unused_bytes
        .saturating_add(u64::try_from(PAGE_SIZE - used)?);
    Ok(())
}

fn read_page(file: &mut File, page_id: u64) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(page_id.saturating_mul(PAGE_SIZE as u64)))?;
    let mut page = vec![0; PAGE_SIZE];
    file.read_exact(&mut page)?;
    Ok(page)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .context("u16 is outside page")?
            .try_into()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .context("u64 is outside page")?
            .try_into()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Minigraf, OpenOptions};

    #[test]
    fn accounts_for_every_published_page() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("layout.graph");
        let db = Minigraf::open_with_options(
            &path,
            OpenOptions::default().benchmark_btree_fill_percent(100),
        )?;
        db.execute("(transact [[:layout/a :layout/value 1] [:layout/b :layout/value 2]])")?;
        db.checkpoint()?;
        drop(db);

        let layout = inspect_storage_layout(&path)?;
        let indexes = [&layout.eavt, &layout.aevt, &layout.avet, &layout.vaet];
        let classified = layout
            .header_bytes
            .saturating_add(layout.facts.allocated_bytes())
            .saturating_add(
                indexes
                    .iter()
                    .map(|index| index.allocated_bytes())
                    .sum::<u64>(),
            )
            .saturating_add(layout.other_published_bytes);
        assert_eq!(classified, layout.published_bytes);
        assert_eq!(layout.facts.entries, 2);
        assert_eq!(layout.eavt.leaf.entries, 2);
        assert_eq!(layout.aevt.leaf.entries, 2);
        assert_eq!(layout.avet.leaf.entries, 2);
        Ok(())
    }
}
