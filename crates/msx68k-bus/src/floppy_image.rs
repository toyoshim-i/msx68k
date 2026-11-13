//! Floppy image formats shared by local mounts and VMX68K's file bridge.
//! Detection asks for a prefix rather than a whole image. Format implementations
//! translate sector ranges and persist metadata here; transports know no formats.

use crate::fdc::FloppyGeometry;

pub trait ImageStorage {
    fn read_at(&self, offset: usize, len: usize) -> Option<Vec<u8>>;
    /// May extend the file. A failed write can have written an earlier range.
    fn write_at(&self, offset: usize, data: Vec<u8>) -> bool;
    /// Commit file-length or container metadata before completing a layout change.
    fn flush_layout(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloppyImage {
    pub geometry: FloppyGeometry,
    format: Format,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Format {
    Raw,
    Dim {
        tracks: Vec<Option<usize>>,
        packed: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Probe {
    Need(usize),
    Image(FloppyImage),
    Unsupported,
}

impl FloppyImage {
    /// The single format registry, used by both command-line and VMX mounts.
    pub fn probe(len: usize, prefix: &[u8]) -> Probe {
        if let Some(geometry) = FloppyGeometry::from_image_len(len) {
            return Probe::Image(Self {
                geometry,
                format: Format::Raw,
            });
        }
        if len < 256 {
            return Probe::Unsupported;
        }
        if prefix.len() < 256 {
            return Probe::Need(256);
        }
        let raw_len = match prefix[0] {
            0 => 1_261_568,
            2 => 1_228_800,
            9 => 1_474_560,
            _ => return Probe::Unsupported,
        };
        let geometry = FloppyGeometry::from_image_len(raw_len).unwrap();
        let count = geometry.cylinders as usize * geometry.heads as usize;
        let size = geometry.sectors_per_track as usize * geometry.sector_bytes();
        let flags = &prefix[1..1 + count];
        let packed = if len == 256 + raw_len {
            false
        } else if len == 256 + flags.iter().filter(|&&f| f != 0).count() * size {
            true
        } else {
            return Probe::Unsupported;
        };
        let mut offset = 256;
        let tracks = flags
            .iter()
            .map(|&flag| {
                let position = (flag != 0).then_some(offset);
                if flag != 0 || !packed {
                    offset += size;
                }
                position
            })
            .collect();
        Probe::Image(Self {
            geometry,
            format: Format::Dim { tracks, packed },
        })
    }

    pub fn is_raw(&self) -> bool {
        matches!(self.format, Format::Raw)
    }

    pub fn len(&self) -> usize {
        self.geometry.cylinders as usize
            * self.geometry.heads as usize
            * self.geometry.sectors_per_track as usize
            * self.geometry.sector_bytes()
    }

    pub fn read(&self, storage: &impl ImageStorage, offset: usize, len: usize) -> Option<Vec<u8>> {
        if offset.checked_add(len)? > self.len() {
            return None;
        }
        let Format::Dim { tracks, .. } = &self.format else {
            return storage
                .read_at(offset, len)
                .filter(|bytes| bytes.len() == len);
        };
        let track_size = self.geometry.sectors_per_track as usize * self.geometry.sector_bytes();
        let mut data = Vec::with_capacity(len);
        while data.len() < len {
            let at = offset + data.len();
            let track = at / track_size;
            let within = at % track_size;
            let count = (len - data.len()).min(track_size - within);
            if let Some(base) = tracks[track] {
                let bytes = storage.read_at(base + within, count)?;
                if bytes.len() != count {
                    return None;
                }
                data.extend(bytes);
            } else {
                data.resize(data.len() + count, 0xE5);
            }
        }
        Some(data)
    }

    pub fn write(&mut self, storage: &impl ImageStorage, offset: usize, data: Vec<u8>) -> bool {
        if !offset
            .checked_add(data.len())
            .is_some_and(|end| end <= self.len())
        {
            return false;
        }
        let track_size = self.geometry.sectors_per_track as usize * self.geometry.sector_bytes();
        if let Format::Dim { tracks, .. } = &self.format {
            let end = offset + data.len();
            if !data.is_empty()
                && tracks[offset / track_size..=(end - 1) / track_size]
                    .iter()
                    .any(Option::is_none)
                && !self.materialize_dim(storage)
            {
                return false;
            }
        }
        let Format::Dim { tracks, .. } = &self.format else {
            return storage.write_at(offset, data);
        };
        let mut done = 0;
        while done < data.len() {
            let at = offset + done;
            let within = at % track_size;
            let count = (data.len() - done).min(track_size - within);
            if !storage.write_at(
                tracks[at / track_size].unwrap() + within,
                data[done..done + count].to_vec(),
            ) {
                return false;
            }
            done += count;
        }
        true
    }

    /// Expand omitted tracks in place, backwards, before writing a previously
    /// absent track. Keep the DIM header and never cache the whole image.
    fn materialize_dim(&mut self, storage: &impl ImageStorage) -> bool {
        let Format::Dim { tracks, .. } = &mut self.format else {
            return false;
        };
        let size = self.geometry.sectors_per_track as usize * self.geometry.sector_bytes();
        for (track, source) in tracks.iter().enumerate().rev() {
            let target = 256 + track * size;
            if let Some(source) = source {
                if *source == target {
                    continue;
                }
                let Some(data) = storage.read_at(*source, size) else {
                    return false;
                };
                if data.len() != size || !storage.write_at(target, data) {
                    return false;
                }
            }
        }
        for (track, source) in tracks.iter().enumerate() {
            if source.is_none() && !storage.write_at(256 + track * size, vec![0xE5; size]) {
                return false;
            }
        }
        if !storage.write_at(1, vec![1; tracks.len()]) {
            return false;
        }
        for (track, source) in tracks.iter_mut().enumerate() {
            *source = Some(256 + track * size);
        }
        self.format = Format::Dim {
            tracks: tracks.clone(),
            packed: false,
        };
        storage.flush_layout()
    }

    pub fn decode(data: &[u8]) -> Option<(FloppyGeometry, Vec<u8>)> {
        let Probe::Image(image) = Self::probe(data.len(), data) else {
            return None;
        };
        Some((image.geometry, image.read(&Slice(data), 0, image.len())?))
    }
}

struct Slice<'a>(&'a [u8]);
impl ImageStorage for Slice<'_> {
    fn read_at(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        Some(self.0.get(offset..offset.checked_add(len)?)?.to_vec())
    }
    fn write_at(&self, _: usize, _: Vec<u8>) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Memory(RefCell<Vec<u8>>);
    impl ImageStorage for Memory {
        fn read_at(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
            Some(
                self.0
                    .borrow()
                    .get(offset..offset.checked_add(len)?)?
                    .to_vec(),
            )
        }
        fn write_at(&self, offset: usize, data: Vec<u8>) -> bool {
            let mut bytes = self.0.borrow_mut();
            let size = bytes.len().max(offset + data.len());
            bytes.resize(size, 0);
            bytes[offset..offset + data.len()].copy_from_slice(&data);
            true
        }
    }

    #[test]
    fn local_and_remote_formats_have_identical_sector_contents() {
        for (kind, len) in [(0, 1_261_568), (2, 1_228_800), (9, 1_474_560)] {
            let geometry = FloppyGeometry::from_image_len(len).unwrap();
            let track_size = geometry.sector_bytes() * geometry.sectors_per_track as usize;
            for packed in [false, true] {
                let mut bytes = vec![0; 256];
                bytes[0] = kind;
                bytes[1] = 1;
                bytes[3] = 1;
                let tracks = geometry.cylinders as usize * geometry.heads as usize;
                for track in 0..tracks {
                    if track == 0 || track == 2 || !packed {
                        bytes.extend(vec![track as u8; track_size]);
                    }
                }
                assert_eq!(FloppyImage::probe(bytes.len(), &[]), Probe::Need(256));
                let Probe::Image(image) = FloppyImage::probe(bytes.len(), &bytes[..256]) else {
                    panic!("DIM {kind}")
                };
                let (_, local) = FloppyImage::decode(&bytes).unwrap();
                assert_eq!(
                    image.read(&Memory(RefCell::new(bytes)), 0, len).unwrap(),
                    local
                );
                assert_eq!(&local[track_size..2 * track_size], vec![0xE5; track_size]);
            }
        }
    }

    #[test]
    fn writes_to_omitted_tracks_preserve_dim_and_survive_reopening() {
        for packed in [false, true] {
            let geometry = FloppyGeometry::XDF_2HD;
            let size = geometry.sector_bytes() * geometry.sectors_per_track as usize;
            let mut bytes = vec![0; 256];
            bytes[1] = 1;
            bytes[3] = 1;
            bytes[200] = 0xAB; // preserve unrelated header bytes
            bytes.extend(vec![0x11; size]);
            if !packed {
                bytes.extend(vec![0x99; size]);
            }
            bytes.extend(vec![0x22; size]);
            if !packed {
                bytes.resize(256 + 1_261_568, 0x99);
            }
            let Probe::Image(mut image) = FloppyImage::probe(bytes.len(), &bytes) else {
                panic!()
            };
            let storage = Memory(RefCell::new(bytes));
            assert!(image.write(&storage, size + 1024, vec![0x7A; 1024]));
            let bytes = storage.0.borrow();
            assert_eq!(bytes.len(), 256 + 1_261_568);
            assert_eq!(bytes[200], 0xAB);
            assert!(bytes[1..155].iter().all(|&f| f == 1));
            let (_, raw) = FloppyImage::decode(&bytes).unwrap();
            assert_eq!(&raw[..size], vec![0x11; size]);
            assert_eq!(&raw[2 * size..3 * size], vec![0x22; size]);
            assert_eq!(&raw[size..size + 1024], vec![0xE5; 1024]);
            assert_eq!(&raw[size + 1024..size + 2048], vec![0x7A; 1024]);
            assert_eq!(&raw[3 * size..], vec![0xE5; raw.len() - 3 * size]);
        }
    }
}
