//! Host filesystem remote drive backend for Human68k.
//!
//! Mounts one or more host directories as Human68k remote drives (type `$2000`).
//! Receives [`BridgeOp`] requests from [`crate::remote_fs::RemoteFs`] and executes
//! them against host files using standard Rust filesystem APIs, translating between
//! Shift_JIS paths and host paths with case-insensitive component matching.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use crate::remote_fs::RemoteBridge;
use crate::vmx68k::{BridgeOp, BridgeReply};

// Human68k DOS errors
const DOS_FILE_NOT_FOUND: u32 = -2i32 as u32;
const DOS_DIRECTORY_NOT_FOUND: u32 = -3i32 as u32;
const DOS_ALREADY_EXISTS: u32 = -8i32 as u32;
const DOS_INVALID_ACCESS: u32 = -12i32 as u32;
const DOS_CANNOT_WRITE: u32 = -19i32 as u32;
const DOS_SEEK_ERROR: u32 = -25i32 as u32;

// File attributes
const ATTR_READONLY: u8 = 0x01;
const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;

/// Directory entry information discovered during search.
#[derive(Debug, Clone)]
struct DirEntryInfo {
    attr: u8,
    time: u16,
    date: u16,
    size: u32,
    name_sjis: Vec<u8>,
}

/// Active search context for `Files` / `Nfiles`.
#[derive(Debug)]
struct SearchContext {
    entries: Vec<DirEntryInfo>,
    current_index: usize,
}

/// Internal state for the host filesystem bridge.
#[derive(Debug)]
struct HostFsState {
    mounts: Vec<PathBuf>,
    open_files: HashMap<u32, File>,
    next_handle: u32,
    searches: HashMap<u32, SearchContext>,
    next_search_id: u32,
}

/// Host filesystem remote drive bridge implementing [`RemoteBridge`].
#[derive(Debug)]
pub struct HostFsBridge {
    state: Mutex<HostFsState>,
}

impl HostFsBridge {
    /// Creates a new host filesystem bridge serving `mounts`.
    /// Each path corresponds to one drive unit (unit 0 -> mounts[0], unit 1 -> mounts[1], ...).
    pub fn new(mounts: Vec<PathBuf>) -> Self {
        let canonical_mounts = mounts
            .into_iter()
            .map(|p| p.canonicalize().unwrap_or(p))
            .collect();
        Self {
            state: Mutex::new(HostFsState {
                mounts: canonical_mounts,
                open_files: HashMap::new(),
                next_handle: 1,
                searches: HashMap::new(),
                next_search_id: 1,
            }),
        }
    }
}

impl RemoteBridge for HostFsBridge {
    fn ask(&self, op: BridgeOp) -> Option<BridgeReply> {
        let mut state = self.state.lock().ok()?;
        Some(state.handle_op(op))
    }
}

impl HostFsState {
    fn handle_op(&mut self, op: BridgeOp) -> BridgeReply {
        match op {
            BridgeOp::Open { path, mode } => self.op_open(&path, mode),
            BridgeOp::Create {
                path,
                attr,
                replace,
            } => self.op_create(&path, attr, replace),
            BridgeOp::Close { handle } => self.op_close(handle),
            BridgeOp::HandleRead {
                handle,
                offset,
                len,
            } => self.op_read(handle, offset, len),
            BridgeOp::HandleWrite {
                handle,
                offset,
                data,
            } => self.op_write(handle, offset, data),
            BridgeOp::Filedate { handle, datetime } => self.op_filedate(handle, datetime),
            BridgeOp::Files { attr, path } => self.op_files(attr, &path),
            BridgeOp::Nfiles { buffer } => self.op_nfiles(&buffer),
            BridgeOp::Mkdir { path } => self.op_mkdir(&path),
            BridgeOp::Rmdir { path } => self.op_rmdir(&path),
            BridgeOp::Delete { path } => self.op_delete(&path),
            BridgeOp::Rename { from, to } => self.op_rename(&from, &to),
            BridgeOp::Chmod { path, attr } => self.op_chmod(&path, attr),
            BridgeOp::Dskfre { drive } => self.op_dskfre(drive),
            _ => BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            },
        }
    }

    /// Resolves a raw Human68k path (e.g. `b"@1:\\DIR\\FILE.EXT"` or `b"A:\\DIR\\FILE.EXT"`)
    /// into a host [`PathBuf`], performing case-insensitive matching for each existing segment.
    fn resolve_path(&self, raw: &[u8]) -> Result<PathBuf, u32> {
        if raw.len() < 2 || raw[1] != b':' {
            return Err(DOS_INVALID_ACCESS);
        }
        let unit = match raw[0].to_ascii_uppercase() {
            b'@'..=b'Z' if raw[0] >= b'@' => (raw[0] - b'@') as usize,
            _ => return Err(DOS_INVALID_ACCESS),
        };
        // Human68k drive numbers: 1 is A: (unit 0 in mounts), 2 is B: (unit 1), etc.
        let mount_idx = unit.saturating_sub(1);
        let base = self
            .mounts
            .get(mount_idx)
            .ok_or(DOS_DIRECTORY_NOT_FOUND)?;

        let rel_bytes = &raw[2..];
        let (rel_str, _, _) = encoding_rs::SHIFT_JIS.decode(rel_bytes);

        // Normalize and resolve components case-insensitively
        let mut cur = base.clone();
        let segments: Vec<&str> = rel_str
            .split(|c| c == '\\' || c == '/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect();

        for seg in segments {
            if seg == ".." {
                if cur != *base && cur.starts_with(base) {
                    if let Some(parent) = cur.parent() {
                        cur = parent.to_path_buf();
                    }
                }
                continue;
            }

            // Case-insensitive lookup in current directory
            let exact = cur.join(seg);
            if exact.exists() {
                cur = exact;
            } else if let Ok(entries) = fs::read_dir(&cur) {
                let mut matched = None;
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.eq_ignore_ascii_case(seg) {
                        matched = Some(entry.path());
                        break;
                    }
                }
                cur = matched.unwrap_or(exact);
            } else {
                cur = exact;
            }
        }

        Ok(cur)
    }

    fn op_open(&mut self, path: &[u8], mode: u32) -> BridgeReply {
        let Ok(host_path) = self.resolve_path(path) else {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        };

        if !host_path.is_file() {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        }

        let mut opts = OpenOptions::new();
        match mode {
            0 => {
                opts.read(true);
            }
            1 => {
                opts.write(true);
            }
            _ => {
                opts.read(true).write(true);
            }
        }

        match opts.open(&host_path) {
            Ok(file) => {
                let handle = self.next_handle;
                self.next_handle = self.next_handle.wrapping_add(1).max(1);
                self.open_files.insert(handle, file);
                BridgeReply {
                    result: handle,
                    data: Vec::new(),
                }
            }
            Err(_) => BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            },
        }
    }

    fn op_create(&mut self, path: &[u8], _attr: u32, replace: bool) -> BridgeReply {
        let Ok(host_path) = self.resolve_path(path) else {
            return BridgeReply {
                result: DOS_DIRECTORY_NOT_FOUND,
                data: Vec::new(),
            };
        };

        if !replace && host_path.exists() {
            return BridgeReply {
                result: DOS_ALREADY_EXISTS,
                data: Vec::new(),
            };
        }

        // Ensure parent directory exists
        if let Some(parent) = host_path.parent() {
            if !parent.exists() {
                return BridgeReply {
                    result: DOS_DIRECTORY_NOT_FOUND,
                    data: Vec::new(),
                };
            }
        }

        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(replace);

        match opts.open(&host_path) {
            Ok(file) => {
                let handle = self.next_handle;
                self.next_handle = self.next_handle.wrapping_add(1).max(1);
                self.open_files.insert(handle, file);
                BridgeReply {
                    result: handle,
                    data: Vec::new(),
                }
            }
            Err(_) => BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            },
        }
    }

    fn op_close(&mut self, handle: u32) -> BridgeReply {
        self.open_files.remove(&handle);
        BridgeReply {
            result: 0,
            data: Vec::new(),
        }
    }

    fn op_read(&mut self, handle: u32, offset: u32, len: u32) -> BridgeReply {
        let Some(file) = self.open_files.get_mut(&handle) else {
            return BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            };
        };

        if file.seek(SeekFrom::Start(offset as u64)).is_err() {
            return BridgeReply {
                result: DOS_SEEK_ERROR,
                data: Vec::new(),
            };
        }

        let mut buf = vec![0u8; len as usize];
        match file.read(&mut buf) {
            Ok(read_len) => {
                buf.truncate(read_len);
                BridgeReply {
                    result: read_len as u32,
                    data: buf,
                }
            }
            Err(_) => BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            },
        }
    }

    fn op_write(&mut self, handle: u32, offset: u32, data: Vec<u8>) -> BridgeReply {
        let Some(file) = self.open_files.get_mut(&handle) else {
            return BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            };
        };

        if data.is_empty() {
            // Cut file at offset
            if file.set_len(offset as u64).is_ok() {
                return BridgeReply {
                    result: 0,
                    data: Vec::new(),
                };
            } else {
                return BridgeReply {
                    result: DOS_CANNOT_WRITE,
                    data: Vec::new(),
                };
            }
        }

        if file.seek(SeekFrom::Start(offset as u64)).is_err() {
            return BridgeReply {
                result: DOS_SEEK_ERROR,
                data: Vec::new(),
            };
        }

        match file.write_all(&data) {
            Ok(()) => BridgeReply {
                result: data.len() as u32,
                data: Vec::new(),
            },
            Err(_) => BridgeReply {
                result: DOS_CANNOT_WRITE,
                data: Vec::new(),
            },
        }
    }

    fn op_filedate(&mut self, handle: u32, datetime: u32) -> BridgeReply {
        let Some(file) = self.open_files.get_mut(&handle) else {
            return BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            };
        };

        if datetime == 0 {
            // Query filedate
            if let Ok(meta) = file.metadata() {
                if let Ok(mtime) = meta.modified() {
                    let (dos_time, dos_date) = system_time_to_dos(mtime);
                    let result = (dos_date as u32) << 16 | (dos_time as u32);
                    return BridgeReply {
                        result,
                        data: Vec::new(),
                    };
                }
            }
            BridgeReply {
                result: 0,
                data: Vec::new(),
            }
        } else {
            // Updating file modification time is a best-effort operation
            BridgeReply {
                result: 0,
                data: Vec::new(),
            }
        }
    }

    fn op_files(&mut self, search_attr: u32, path: &[u8]) -> BridgeReply {
        if path.len() < 2 || path[1] != b':' {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        }

        let unit = match path[0].to_ascii_uppercase() {
            b'@'..=b'Z' if path[0] >= b'@' => (path[0] - b'@') as usize,
            _ => {
                return BridgeReply {
                    result: DOS_FILE_NOT_FOUND,
                    data: Vec::new(),
                }
            }
        };
        let mount_idx = unit.saturating_sub(1);
        let Some(base) = self.mounts.get(mount_idx).cloned() else {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        };

        let rel_bytes = &path[2..];
        let (rel_str, _, _) = encoding_rs::SHIFT_JIS.decode(rel_bytes);

        // Split into parent directory and pattern
        let (parent_rel, pattern) = match rel_str.rfind(|c| c == '\\' || c == '/') {
            Some(idx) => (&rel_str[..idx], &rel_str[idx + 1..]),
            None => ("", rel_str.as_ref()),
        };

        let parent_dir = if parent_rel.is_empty() {
            base.clone()
        } else {
            // resolve_path accepts Shift_JIS, so do not pass the decoded UTF-8 bytes.
            let (parent_sjis, _, _) = encoding_rs::SHIFT_JIS.encode(parent_rel);
            let mut parent_path = vec![path[0], b':'];
            parent_path.extend_from_slice(&parent_sjis);
            match self.resolve_path(&parent_path) {
                Ok(p) => p,
                Err(_) => {
                    return BridgeReply {
                        result: DOS_DIRECTORY_NOT_FOUND,
                        data: Vec::new(),
                    }
                }
            }
        };

        if !parent_dir.is_dir() {
            return BridgeReply {
                result: DOS_DIRECTORY_NOT_FOUND,
                data: Vec::new(),
            };
        }

        let Ok(read_dir) = fs::read_dir(&parent_dir) else {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        };

        let mut matched_entries = Vec::new();

        // SX-Window asks for attribute 0x08 to obtain a drive's display name.
        // This must never return one of the directory's ordinary files.
        if parent_dir == base && search_attr & ATTR_VOLUME as u32 != 0 {
            let label = base.file_name().and_then(|s| s.to_str()).unwrap_or("HOSTDRIVE");
            let mut name_sjis = Vec::new();
            for c in label.chars() {
                let text = c.to_string();
                let (bytes, _, errors) = encoding_rs::SHIFT_JIS.encode(&text);
                if errors || c.is_control() {
                    continue;
                }
                if name_sjis.len() + bytes.len() > 18 {
                    break;
                }
                name_sjis.extend_from_slice(&bytes);
            }
            if name_sjis.is_empty() {
                name_sjis.extend_from_slice(b"HOSTDRIVE");
            }
            let label_text = encoding_rs::SHIFT_JIS.decode(&name_sjis).0;
            if wildcard_match(pattern, &label_text) {
                matched_entries.push(DirEntryInfo {
                    attr: ATTR_VOLUME, time: 0, date: 0x0021, size: 0, name_sjis,
                });
            }
        }

        // Synthetic . and .. for subdirectories
        if parent_dir != base && (search_attr & (ATTR_DIRECTORY as u32) != 0) {
            if wildcard_match(pattern, ".") {
                matched_entries.push(DirEntryInfo {
                    attr: ATTR_DIRECTORY,
                    time: 0,
                    date: 0x0021,
                    size: 0,
                    name_sjis: b".".to_vec(),
                });
            }
            if wildcard_match(pattern, "..") {
                matched_entries.push(DirEntryInfo {
                    attr: ATTR_DIRECTORY,
                    time: 0,
                    date: 0x0021,
                    size: 0,
                    name_sjis: b"..".to_vec(),
                });
            }
        }

        for entry in read_dir.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if name_str.starts_with('.') {
                continue;
            }
            if !wildcard_match(pattern, &name_str) {
                continue;
            }

            let Ok(meta) = entry.metadata() else {
                continue;
            };

            let is_dir = meta.is_dir();
            let mut attr = if is_dir {
                ATTR_DIRECTORY
            } else {
                ATTR_ARCHIVE
            };
            if meta.permissions().readonly() {
                attr |= ATTR_READONLY;
            }

            // Human68k selects entries whose attributes overlap the search mask.
            if attr as u32 & search_attr == 0 {
                continue;
            }

            let (time, date) = meta
                .modified()
                .map(system_time_to_dos)
                .unwrap_or((0, 0x0021)); // 1980-01-01
            let size = if is_dir { 0 } else { meta.len().min(u32::MAX as u64) as u32 };

            let (name_sjis, _, _) = encoding_rs::SHIFT_JIS.encode(&name_str);

            matched_entries.push(DirEntryInfo {
                attr,
                time,
                date,
                size,
                name_sjis: name_sjis.into_owned(),
            });
        }

        if matched_entries.is_empty() {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        }

        let search_id = self.next_search_id;
        self.next_search_id = self.next_search_id.wrapping_add(1).max(1);

        let buf = build_search_buffer(search_id, 0, &matched_entries[0]);
        self.searches.insert(
            search_id,
            SearchContext {
                entries: matched_entries,
                current_index: 1,
            },
        );

        BridgeReply {
            result: 0,
            data: buf,
        }
    }

    fn op_nfiles(&mut self, buffer: &[u8]) -> BridgeReply {
        if buffer.len() < 8 {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        }

        let search_id = u32::from_be_bytes(buffer[0..4].try_into().unwrap());
        let Some(ctx) = self.searches.get_mut(&search_id) else {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        };

        if ctx.current_index >= ctx.entries.len() {
            self.searches.remove(&search_id);
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        }

        let idx = ctx.current_index;
        let entry = &ctx.entries[idx];
        let buf = build_search_buffer(search_id, idx as u32, entry);
        ctx.current_index += 1;

        BridgeReply {
            result: 0,
            data: buf,
        }
    }

    fn op_mkdir(&mut self, path: &[u8]) -> BridgeReply {
        let Ok(host_path) = self.resolve_path(path) else {
            return BridgeReply {
                result: DOS_DIRECTORY_NOT_FOUND,
                data: Vec::new(),
            };
        };

        match fs::create_dir(&host_path) {
            Ok(()) => BridgeReply {
                result: 0,
                data: Vec::new(),
            },
            Err(_) => BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            },
        }
    }

    fn op_rmdir(&mut self, path: &[u8]) -> BridgeReply {
        let Ok(host_path) = self.resolve_path(path) else {
            return BridgeReply {
                result: DOS_DIRECTORY_NOT_FOUND,
                data: Vec::new(),
            };
        };

        match fs::remove_dir(&host_path) {
            Ok(()) => BridgeReply {
                result: 0,
                data: Vec::new(),
            },
            Err(_) => BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            },
        }
    }

    fn op_delete(&mut self, path: &[u8]) -> BridgeReply {
        let Ok(host_path) = self.resolve_path(path) else {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        };

        match fs::remove_file(&host_path) {
            Ok(()) => BridgeReply {
                result: 0,
                data: Vec::new(),
            },
            Err(_) => BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            },
        }
    }

    fn op_rename(&mut self, from: &[u8], to: &[u8]) -> BridgeReply {
        let Ok(host_from) = self.resolve_path(from) else {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        };
        let Ok(host_to) = self.resolve_path(to) else {
            return BridgeReply {
                result: DOS_DIRECTORY_NOT_FOUND,
                data: Vec::new(),
            };
        };

        match fs::rename(&host_from, &host_to) {
            Ok(()) => BridgeReply {
                result: 0,
                data: Vec::new(),
            },
            Err(_) => BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            },
        }
    }

    fn op_chmod(&mut self, path: &[u8], attr: u32) -> BridgeReply {
        let Ok(host_path) = self.resolve_path(path) else {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        };

        let Ok(meta) = host_path.metadata() else {
            return BridgeReply {
                result: DOS_FILE_NOT_FOUND,
                data: Vec::new(),
            };
        };

        if attr == 0xFFFF {
            // Read attribute
            let mut val = if meta.is_dir() {
                ATTR_DIRECTORY
            } else {
                ATTR_ARCHIVE
            };
            if meta.permissions().readonly() {
                val |= ATTR_READONLY;
            }
            BridgeReply {
                result: val as u32,
                data: Vec::new(),
            }
        } else {
            // Best-effort attribute update
            BridgeReply {
                result: attr,
                data: Vec::new(),
            }
        }
    }

    fn op_dskfre(&mut self, drive: u32) -> BridgeReply {
        let mount_idx = (drive.max(1) - 1) as usize;
        if mount_idx >= self.mounts.len() {
            return BridgeReply {
                result: DOS_INVALID_ACCESS,
                data: Vec::new(),
            };
        }

        // Return a virtual 512MB volume:
        // free_clusters: 60000, total_clusters: 65535, sec_per_clus: 8, bytes_per_sec: 1024
        let mut buf = Vec::with_capacity(8);
        buf.extend_from_slice(&60000u16.to_be_bytes());
        buf.extend_from_slice(&65535u16.to_be_bytes());
        buf.extend_from_slice(&8u16.to_be_bytes());
        buf.extend_from_slice(&1024u16.to_be_bytes());

        BridgeReply {
            result: 0,
            data: buf,
        }
    }
}

/// Builds a 53-byte directory entry buffer for Human68k `_FILES` / `_NFILES`.
/// Layout matches Human68k `struct _filbuf`:
/// - 0..10: internal search state
/// - 10..18: 8-byte space-padded uppercase stem
/// - 18..21: 3-byte space-padded uppercase extension
/// - 21: attribute byte
/// - 22..24: modification time (u16 be)
/// - 24..26: modification date (u16 be)
/// - 26..30: file size (u32 be)
/// - 30..53: formatted name (null-terminated Shift_JIS, up to 22 bytes + null)
fn build_search_buffer(search_id: u32, index: u32, entry: &DirEntryInfo) -> Vec<u8> {
    let mut buf = vec![0u8; 53];
    buf[0..4].copy_from_slice(&search_id.to_be_bytes());
    buf[4..8].copy_from_slice(&index.to_be_bytes());

    // 10..18: filename (8 bytes, space-padded, uppercase)
    // 18..21: ext (3 bytes, space-padded, uppercase)
    let (stem, ext) = split_stem_ext_bytes(&entry.name_sjis);
    let mut fn8 = [b' '; 8];
    let mut ext3 = [b' '; 3];
    for (i, &b) in stem.iter().take(8).enumerate() {
        fn8[i] = b.to_ascii_uppercase();
    }
    for (i, &b) in ext.iter().take(3).enumerate() {
        ext3[i] = b.to_ascii_uppercase();
    }
    buf[10..18].copy_from_slice(&fn8);
    buf[18..21].copy_from_slice(&ext3);

    buf[21] = entry.attr;
    buf[22..24].copy_from_slice(&entry.time.to_be_bytes());
    buf[24..26].copy_from_slice(&entry.date.to_be_bytes());
    buf[26..30].copy_from_slice(&entry.size.to_be_bytes());

    // File name at 30..53 (null-terminated Shift_JIS, up to 22 bytes + null)
    let name_len = entry.name_sjis.len().min(22);
    buf[30..30 + name_len].copy_from_slice(&entry.name_sjis[..name_len]);
    buf[30 + name_len] = 0;
    buf
}

fn split_stem_ext_bytes(s: &[u8]) -> (&[u8], &[u8]) {
    match s.iter().rposition(|&b| b == b'.') {
        Some(idx) => (&s[..idx], &s[idx + 1..]),
        None => (s, b""),
    }
}

/// Human68k 8.3-aware wildcard matching (case-insensitive).
///
/// Handles `*.*`, `*`, `????????.???`, and standard `?` expansion where trailing
/// question marks match empty space when target stem or extension is shorter than the pattern.
fn wildcard_match(pattern: &str, target: &str) -> bool {
    let p = pattern.trim();
    if p.is_empty() || p == "*.*" || p == "*" || p == "????????.???" {
        return true;
    }

    let (p_stem, p_ext) = split_stem_ext(p);
    let (t_stem, t_ext) = split_stem_ext(target);

    match_component(p_stem, t_stem) && match_ext(p_ext, t_ext)
}

fn split_stem_ext(s: &str) -> (&str, &str) {
    match s.rfind('.') {
        Some(idx) => (&s[..idx], &s[idx + 1..]),
        None => (s, ""),
    }
}

fn match_ext(p_ext: &str, t_ext: &str) -> bool {
    if p_ext == "*" || p_ext == "???" || p_ext == "*.*" {
        return true;
    }
    match_component(p_ext, t_ext)
}

fn match_component(pat: &str, target: &str) -> bool {
    let p = pat.as_bytes();
    let t = target.as_bytes();

    let mut pi = 0;
    let mut ti = 0;

    while pi < p.len() {
        match p[pi] {
            b'*' => return true,
            b'?' => {
                if ti < t.len() {
                    ti += 1;
                }
                pi += 1;
            }
            c => {
                if ti >= t.len() {
                    return false;
                }
                if !c.eq_ignore_ascii_case(&t[ti]) {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }

    ti == t.len()
}

/// Converts a [`SystemTime`] into Human68k FAT `(time, date)` words.
fn system_time_to_dos(time: SystemTime) -> (u16, u16) {
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let (year, month, day, hour, min, sec) = seconds_to_datetime(secs);
    let dos_year = if year >= 1980 {
        (year - 1980).min(127) as u16
    } else {
        0
    };
    let dos_date = (dos_year << 9) | ((month as u16) << 5) | (day as u16);
    let dos_time = ((hour as u16) << 11) | ((min as u16) << 5) | ((sec as u16) / 2);
    (dos_time, dos_date)
}

/// Converts unix timestamp seconds to UTC `(year, month, day, hour, min, sec)`.
fn seconds_to_datetime(mut secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let sec = (secs % 60) as u32;
    secs /= 60;
    let min = (secs % 60) as u32;
    secs /= 60;
    let hour = (secs % 24) as u32;
    let mut days = (secs / 24) as u32;

    // Epoch 1970-01-01 was a Thursday
    let mut year = 1970;
    loop {
        let leap = is_leap_year(year);
        let year_days = if leap { 366 } else { 365 };
        if days < year_days {
            break;
        }
        days -= year_days;
        year += 1;
    }

    let leap = is_leap_year(year);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month = 1;
    for &d in &month_days {
        if days < d {
            break;
        }
        days -= d;
        month += 1;
    }
    let day = days + 1;

    (year, month, day, hour, min, sec)
}

fn is_leap_year(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_search_does_not_return_a_regular_file() {
        let dir = std::env::temp_dir().join(format!("msx68k_volume_test_{}", std::process::id()));
        let mount = dir.join("HOSTSHARE");
        fs::create_dir_all(mount.join("SUBDIR")).unwrap();
        fs::write(mount.join("SXWIN31.HDS"), b"disk").unwrap();
        let bridge = HostFsBridge::new(vec![mount]);
        let volume = bridge.ask(BridgeOp::Files {
            attr: 0x08, path: b"A:\\????????.???".to_vec(),
        }).unwrap();
        let next = bridge.ask(BridgeOp::Nfiles { buffer: volume.data.clone() }).unwrap();
        let subdir = bridge.ask(BridgeOp::Files {
            attr: 0x08, path: b"A:\\SUBDIR\\*.*".to_vec(),
        }).unwrap();
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(volume.result, 0);
        assert_eq!(volume.data[21], 0x08);
        assert_eq!(&volume.data[30..40], b"HOSTSHARE\0");
        assert_eq!(next.result, DOS_FILE_NOT_FOUND);
        assert_eq!(subdir.result, DOS_FILE_NOT_FOUND);
    }

    #[test]
    fn wildcard_matches_correctly() {
        assert!(wildcard_match("*.*", "test.txt"));
        assert!(wildcard_match("*.TXT", "test.txt"));
        assert!(wildcard_match("T*.*", "Test.dat"));
        assert!(wildcard_match("T?ST.DAT", "test.dat"));
        assert!(!wildcard_match("*.DAT", "test.txt"));

        // Human68k _NAMESTS expanded wildcards
        assert!(wildcard_match("????????.???", "SRAM.DAT"));
        assert!(wildcard_match("????????.???", "DIRDTOP.SX"));
        assert!(wildcard_match("????????.???", "HUMAN302.XDF"));
        assert!(wildcard_match("????????.???", "README"));
        assert!(wildcard_match("SRAM????.???", "SRAM.DAT"));
        assert!(wildcard_match("????????.SX", "DIRDTOP.SX"));
        assert!(!wildcard_match("????????.DAT", "DIRDTOP.SX"));
    }

    #[test]
    fn dos_datetime_converts_back_and_forth() {
        let (time, date) = system_time_to_dos(SystemTime::UNIX_EPOCH);
        // 1970 is before 1980, clamps to 1980
        assert_eq!(date & 0x1F, 1); // day 1
        assert_eq!((date >> 5) & 0x0F, 1); // month 1
        assert_eq!(time, 0);
    }

    #[test]
    fn lists_and_opens_files_in_shift_jis_directories() {
        let dir = std::env::temp_dir().join(format!("msx68k_sjis_test_{}", std::process::id()));
        fs::create_dir_all(dir.join("日本語")).unwrap();
        fs::write(dir.join("日本語/TEST.TXT"), b"test").unwrap();
        let bridge = HostFsBridge::new(vec![dir.clone()]);
        let (path, _, errors) = encoding_rs::SHIFT_JIS.encode("A:\\日本語\\TEST.TXT");
        assert!(!errors);
        let found = bridge.ask(BridgeOp::Files { attr: 0x27, path: path.to_vec() }).unwrap();
        let opened = bridge.ask(BridgeOp::Open { mode: 0, path: path.to_vec() }).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found.result, 0, "FILES must resolve the same path as OPEN");
        assert!((opened.result as i32) > 0);
        bridge.ask(BridgeOp::Close { handle: opened.result }).unwrap();
    }

    #[test]
    fn host_fs_bridge_file_and_directory_lifecycle() {
        let dir = std::env::temp_dir().join(format!("msx68k_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let bridge = HostFsBridge::new(vec![dir.clone()]);

        // 1. Create and write a file
        let create_reply = bridge
            .ask(BridgeOp::Create {
                path: b"A:\\HELLO.TXT".to_vec(),
                attr: 0x20,
                replace: true,
            })
            .unwrap();
        assert!(create_reply.result > 0);
        let handle = create_reply.result;

        let write_reply = bridge
            .ask(BridgeOp::HandleWrite {
                handle,
                offset: 0,
                data: b"Hello X68000 Remote Drive!".to_vec(),
            })
            .unwrap();
        assert_eq!(write_reply.result, 26);

        bridge.ask(BridgeOp::Close { handle }).unwrap();

        // 2. Open case-insensitively and read
        let open_reply = bridge
            .ask(BridgeOp::Open {
                path: b"A:\\hello.txt".to_vec(),
                mode: 0,
            })
            .unwrap();
        assert!(open_reply.result > 0);
        let read_handle = open_reply.result;

        let read_reply = bridge
            .ask(BridgeOp::HandleRead {
                handle: read_handle,
                offset: 6,
                len: 6,
            })
            .unwrap();
        assert_eq!(read_reply.result, 6);
        assert_eq!(read_reply.data, b"X68000");

        bridge.ask(BridgeOp::Close { handle: read_handle }).unwrap();

        // 3. Directory listing with Files and Nfiles
        let files_reply = bridge
            .ask(BridgeOp::Files {
                attr: 0x27,
                path: b"A:\\*.TXT".to_vec(),
            })
            .unwrap();
        assert_eq!(files_reply.result, 0);
        assert_eq!(files_reply.data.len(), 53);
        assert_eq!(&files_reply.data[10..18], b"HELLO   ");
        assert_eq!(&files_reply.data[18..21], b"TXT");
        assert_eq!(&files_reply.data[30..39], b"HELLO.TXT");

        let nfiles_reply = bridge
            .ask(BridgeOp::Nfiles {
                buffer: files_reply.data,
            })
            .unwrap();
        assert_eq!(nfiles_reply.result, DOS_FILE_NOT_FOUND);

        // Also test _NAMESTS expanded form ????????.???
        let files_reply2 = bridge
            .ask(BridgeOp::Files {
                attr: 0x27,
                path: b"A:\\????????.???".to_vec(),
            })
            .unwrap();
        assert_eq!(files_reply2.result, 0);
        assert_eq!(&files_reply2.data[30..39], b"HELLO.TXT");

        // 4. Mkdir, Rename, Delete
        let mkdir_reply = bridge
            .ask(BridgeOp::Mkdir {
                path: b"A:\\SUBDIR".to_vec(),
            })
            .unwrap();
        assert_eq!(mkdir_reply.result, 0);

        let rename_reply = bridge
            .ask(BridgeOp::Rename {
                from: b"A:\\HELLO.TXT".to_vec(),
                to: b"A:\\SUBDIR\\MOVED.TXT".to_vec(),
            })
            .unwrap();
        assert_eq!(rename_reply.result, 0);

        assert!(dir.join("SUBDIR").join("MOVED.TXT").exists());

        let delete_reply = bridge
            .ask(BridgeOp::Delete {
                path: b"A:\\SUBDIR\\MOVED.TXT".to_vec(),
            })
            .unwrap();
        assert_eq!(delete_reply.result, 0);
        assert!(!dir.join("SUBDIR").join("MOVED.TXT").exists());

        let rmdir_reply = bridge
            .ask(BridgeOp::Rmdir {
                path: b"A:\\SUBDIR".to_vec(),
            })
            .unwrap();
        assert_eq!(rmdir_reply.result, 0);

        // 5. Dskfre
        let dskfre_reply = bridge.ask(BridgeOp::Dskfre { drive: 1 }).unwrap();
        assert_eq!(dskfre_reply.result, 0);
        assert_eq!(dskfre_reply.data.len(), 8);

        let _ = fs::remove_dir_all(&dir);
    }
}
