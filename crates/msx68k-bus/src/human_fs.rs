//! Reading and adding files on the Human68k partition of a SCSI HDD image, in place, as a
//! user's disk is changed without being rebuilt.
//!
//! The layout follows the partition's own boot sector: a big-endian BPB, two copies of a
//! big-endian FAT16, the root directory, then clusters of whole records. Directory entries are
//! 32 bytes: an 8-byte name and 3-byte extension, attributes, 10 more name bytes, then time,
//! date, first cluster, and size, little-endian.

use crate::scsi::parse_scsi_partitions;

const ENTRY_BYTES: usize = 32;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const END_OF_CHAIN: u16 = 0xFFFF;

/// An 8-byte name and a 3-byte extension, space-padded, as a directory entry holds them.
pub type ShortName = ([u8; 8], [u8; 3]);

/// Builds a [`ShortName`] from `NAME` and `EXT`.
pub fn short_name(name: &str, ext: &str) -> ShortName {
    let mut n = [b' '; 8];
    let mut e = [b' '; 3];
    n[..name.len().min(8)].copy_from_slice(&name.as_bytes()[..name.len().min(8)]);
    e[..ext.len().min(3)].copy_from_slice(&ext.as_bytes()[..ext.len().min(3)]);
    (n, e)
}

/// Where a directory's entries live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Root,
    Cluster(u16),
}

/// The Human68k partition of an HDD image.
pub struct HumanVolume<'a> {
    image: &'a mut [u8],
    /// Byte offset of the partition's boot sector.
    base: usize,
    record: usize,
    cluster: usize,
    fats: usize,
    fat_records: usize,
    root_entries: usize,
    clusters: usize,
}

impl<'a> HumanVolume<'a> {
    /// Opens the first partition of `image`, or `None` if it is not a Human68k volume.
    pub fn open(image: &'a mut [u8]) -> Option<Self> {
        let partition = parse_scsi_partitions(image)?.into_iter().next()?;
        let base = partition.start_record as usize * 1024;
        let boot = image.get(base..base + 0x26)?;
        if &boot[2..8] != b"X68IPL" {
            return None;
        }
        let record = u16::from_be_bytes([boot[0x12], boot[0x13]]) as usize;
        let cluster = boot[0x14] as usize * record;
        let fats = boot[0x15] as usize;
        let reserved = u16::from_be_bytes([boot[0x16], boot[0x17]]) as usize;
        let root_entries = u16::from_be_bytes([boot[0x18], boot[0x19]]) as usize;
        let fat_records = boot[0x1D] as usize;
        let total = u32::from_be_bytes([boot[0x1E], boot[0x1F], boot[0x20], boot[0x21]]) as usize;
        if record == 0 || cluster == 0 || reserved != 1 || fats == 0 {
            return None;
        }
        let data_records = reserved + fats * fat_records + root_entries * ENTRY_BYTES / record;
        // FAT16 entries the FAT can hold, and clusters the partition and the image can hold.
        let by_fat = fat_records * record / 2;
        let by_size = 2 + total.saturating_sub(data_records) * record / cluster;
        let by_image = 2 + (image.len().saturating_sub(base + data_records * record)) / cluster;
        let clusters = by_fat.min(by_size).min(by_image);
        Some(Self {
            image,
            base,
            record,
            cluster,
            fats,
            fat_records,
            root_entries,
            clusters,
        })
    }

    fn fat_offset(&self, copy: usize) -> usize {
        self.base + self.record + copy * self.fat_records * self.record
    }

    fn root_offset(&self) -> usize {
        self.fat_offset(self.fats)
    }

    fn cluster_offset(&self, cluster: u16) -> usize {
        self.root_offset() + self.root_entries * ENTRY_BYTES + (cluster as usize - 2) * self.cluster
    }

    fn fat(&self, cluster: u16) -> u16 {
        let at = self.fat_offset(0) + cluster as usize * 2;
        u16::from_be_bytes([self.image[at], self.image[at + 1]])
    }

    fn set_fat(&mut self, cluster: u16, value: u16) {
        for copy in 0..self.fats {
            let at = self.fat_offset(copy) + cluster as usize * 2;
            self.image[at..at + 2].copy_from_slice(&value.to_be_bytes());
        }
    }

    /// The clusters of the chain starting at `first`.
    fn chain(&self, first: u16) -> Option<Vec<u16>> {
        let mut chain = Vec::new();
        let mut cluster = first;
        while (2..self.clusters as u16).contains(&cluster) {
            if chain.len() > self.clusters {
                return None;
            }
            chain.push(cluster);
            cluster = self.fat(cluster);
        }
        (cluster >= 0xFFF8 && !chain.is_empty()).then_some(chain)
    }

    /// Byte offsets of every entry slot of `dir`.
    fn slots(&self, dir: Dir) -> Option<Vec<usize>> {
        Some(match dir {
            Dir::Root => (0..self.root_entries)
                .map(|i| self.root_offset() + i * ENTRY_BYTES)
                .collect(),
            Dir::Cluster(first) => self
                .chain(first)?
                .into_iter()
                .flat_map(|c| {
                    let start = self.cluster_offset(c);
                    (0..self.cluster / ENTRY_BYTES).map(move |i| start + i * ENTRY_BYTES)
                })
                .collect(),
        })
    }

    /// Finds the entry named `name` in `dir`, stopping at the end marker.
    fn find(&self, dir: Dir, name: &ShortName) -> Option<usize> {
        for at in self.slots(dir)? {
            let entry = &self.image[at..at + ENTRY_BYTES];
            match entry[0] {
                0 => return None,
                0xE5 => continue,
                _ if entry[..8] == name.0 && entry[8..11] == name.1 => return Some(at),
                _ => {}
            }
        }
        None
    }

    fn dir_at(&self, path: &[ShortName]) -> Option<Dir> {
        let mut dir = Dir::Root;
        for name in path {
            let at = self.find(dir, name)?;
            if self.image[at + 11] & ATTR_DIRECTORY == 0 {
                return None;
            }
            dir = Dir::Cluster(u16::from_le_bytes([self.image[at + 26], self.image[at + 27]]));
        }
        Some(dir)
    }

    /// Reads the file `name` in the directory `path`.
    pub fn read_file(&self, path: &[ShortName], name: &ShortName) -> Option<Vec<u8>> {
        let at = self.find(self.dir_at(path)?, name)?;
        let entry = &self.image[at..at + ENTRY_BYTES];
        let first = u16::from_le_bytes([entry[26], entry[27]]);
        let size = u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]) as usize;
        if size == 0 {
            return Some(Vec::new());
        }
        let mut data = Vec::with_capacity(size);
        for c in self.chain(first)? {
            let start = self.cluster_offset(c);
            data.extend_from_slice(&self.image[start..start + self.cluster]);
        }
        (data.len() >= size).then(|| {
            data.truncate(size);
            data
        })
    }

    /// Number of free clusters.
    pub fn free_clusters(&self) -> usize {
        (2..self.clusters as u16).filter(|&c| self.fat(c) == 0).count()
    }

    /// Takes `count` free clusters, linked into a chain, or `None` if there are not enough.
    fn allocate(&mut self, count: usize) -> Option<Vec<u16>> {
        let free: Vec<u16> = (2..self.clusters as u16).filter(|&c| self.fat(c) == 0).take(count).collect();
        if free.len() < count {
            return None;
        }
        for (i, &c) in free.iter().enumerate() {
            self.set_fat(c, free.get(i + 1).copied().unwrap_or(END_OF_CHAIN));
        }
        Some(free)
    }

    /// Returns a free entry slot of `dir`, extending a subdirectory by a cluster if it is full.
    fn free_slot(&mut self, dir: Dir) -> Result<usize, String> {
        let slots = self.slots(dir).ok_or("the directory's cluster chain is broken")?;
        if let Some(&at) = slots.iter().find(|&&at| matches!(self.image[at], 0 | 0xE5)) {
            return Ok(at);
        }
        let Dir::Cluster(first) = dir else {
            return Err("the root directory is full".into());
        };
        let last = *self.chain(first).ok_or("the directory's cluster chain is broken")?.last().unwrap();
        let [added] = self.allocate(1).ok_or("the disk is full")?[..] else { unreachable!() };
        self.set_fat(last, added);
        let start = self.cluster_offset(added);
        self.image[start..start + self.cluster].fill(0);
        Ok(start)
    }

    /// Adds the file `name` with `data` to the directory `path`, which must exist and must not
    /// hold `name` yet. Nothing else on the volume changes.
    pub fn add_file(
        &mut self,
        path: &[ShortName],
        name: &ShortName,
        data: &[u8],
        time: u16,
        date: u16,
    ) -> Result<(), String> {
        let dir = self.dir_at(path).ok_or("the directory does not exist")?;
        if self.find(dir, name).is_some() {
            return Err("a file of that name already exists".into());
        }
        let needed = data.len().div_ceil(self.cluster);
        // One more for the directory, which may need to grow.
        if self.free_clusters() < needed + 1 {
            return Err("the disk is full".into());
        }
        let slot = self.free_slot(dir)?;
        let clusters = self.allocate(needed).ok_or("the disk is full")?;
        for (i, &c) in clusters.iter().enumerate() {
            let start = self.cluster_offset(c);
            let chunk = &data[i * self.cluster..((i + 1) * self.cluster).min(data.len())];
            self.image[start..start + self.cluster].fill(0);
            self.image[start..start + chunk.len()].copy_from_slice(chunk);
        }
        let entry = &mut self.image[slot..slot + ENTRY_BYTES];
        entry.fill(0);
        entry[..8].copy_from_slice(&name.0);
        entry[8..11].copy_from_slice(&name.1);
        entry[11] = ATTR_ARCHIVE;
        entry[22..24].copy_from_slice(&time.to_le_bytes());
        entry[24..26].copy_from_slice(&date.to_le_bytes());
        entry[26..28].copy_from_slice(&clusters.first().copied().unwrap_or(0).to_le_bytes());
        entry[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());
        Ok(())
    }
}
