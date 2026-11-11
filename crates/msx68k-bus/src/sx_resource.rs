//! SX-Window resource libraries (`.LB` files), as read by the Resource Manager.
//!
//! A library starts with a header listing its types, followed by one section per type that
//! lists its resources, followed by the resource data:
//!
//! | Part | Layout |
//! | :--- | :--- |
//! | Header | size of the rest (`16 + 8 × types`), type count, two runtime words, 0, then per type its code and a runtime word |
//! | Section | size of the rest (`16 + 16 × resources`), resource count, type code, runtime word, `$$$$`, then the resources |
//! | Resource | ID (word), attributes (word), data size, runtime word, data offset in the file |
//!
//! Runtime words hold handles of a loaded library and are kept as they are.

/// One resource of a library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub id: u16,
    pub attributes: u16,
    runtime: u32,
    pub data: Vec<u8>,
}

impl Resource {
    pub fn new(id: u16, data: Vec<u8>) -> Self {
        Self {
            id,
            attributes: 0,
            runtime: 0,
            data,
        }
    }
}

/// All resources of one type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceType {
    pub code: [u8; 4],
    header_runtime: u32,
    section_runtime: u32,
    tag: [u8; 4],
    pub resources: Vec<Resource>,
}

/// A parsed resource library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLibrary {
    header_words: [u32; 3],
    pub types: Vec<ResourceType>,
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn code_at(data: &[u8], at: usize) -> Option<[u8; 4]> {
    data.get(at..at + 4)?.try_into().ok()
}

impl Default for ResourceLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceLibrary {
    /// Creates a library without resources.
    pub fn new() -> Self {
        Self {
            header_words: [0; 3],
            types: Vec::new(),
        }
    }

    /// Parses a library, returning `None` if it is malformed.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let header_size = u32_at(data, 0)? as usize;
        let type_count = u32_at(data, 4)? as usize;
        if header_size != 16 + 8 * type_count {
            return None;
        }
        let header_words = [u32_at(data, 8)?, u32_at(data, 12)?, u32_at(data, 16)?];
        let mut types = Vec::with_capacity(type_count);
        let mut section = 4 + header_size;
        for t in 0..type_count {
            let code = code_at(data, 20 + 8 * t)?;
            let header_runtime = u32_at(data, 24 + 8 * t)?;
            let size = u32_at(data, section)? as usize;
            let count = u32_at(data, section + 4)? as usize;
            if size != 16 + 16 * count || code_at(data, section + 8)? != code {
                return None;
            }
            let mut resources = Vec::with_capacity(count);
            for r in 0..count {
                let entry = section + 20 + 16 * r;
                let length = u32_at(data, entry + 4)? as usize;
                let offset = u32_at(data, entry + 12)? as usize;
                resources.push(Resource {
                    id: u16_at(data, entry)?,
                    attributes: u16_at(data, entry + 2)?,
                    runtime: u32_at(data, entry + 8)?,
                    data: data.get(offset..offset + length)?.to_vec(),
                });
            }
            types.push(ResourceType {
                code,
                header_runtime,
                section_runtime: u32_at(data, section + 12)?,
                tag: code_at(data, section + 16)?,
                resources,
            });
            section += 4 + size;
        }
        Some(Self {
            header_words,
            types,
        })
    }

    /// Returns the resource of type `code` with `id`.
    pub fn get(&self, code: &[u8; 4], id: u16) -> Option<&Resource> {
        let resources = &self.types.iter().find(|t| &t.code == code)?.resources;
        resources.iter().find(|r| r.id == id)
    }

    /// Adds or replaces the resource of type `code` with `resource.id`. Resources stay in
    /// descending ID order, as the libraries Sharp shipped keep them.
    pub fn insert(&mut self, code: &[u8; 4], resource: Resource) {
        let index = match self.types.iter().position(|t| &t.code == code) {
            Some(index) => index,
            None => {
                let at = self.types.partition_point(|t| t.code < *code);
                self.types.insert(
                    at,
                    ResourceType {
                        code: *code,
                        header_runtime: 0,
                        section_runtime: 0,
                        tag: *b"$$$$",
                        resources: Vec::new(),
                    },
                );
                at
            }
        };
        let resources = &mut self.types[index].resources;
        resources.retain(|r| r.id != resource.id);
        let at = resources.partition_point(|r| r.id > resource.id);
        resources.insert(at, resource);
    }

    /// Writes the library, placing the data of each resource at an even offset.
    pub fn to_bytes(&self) -> Vec<u8> {
        let header_size = 16 + 8 * self.types.len();
        let directory_end: usize = 4
            + header_size
            + self.types.iter().map(|t| 20 + 16 * t.resources.len()).sum::<usize>();
        let mut out = Vec::with_capacity(directory_end);
        let push = |out: &mut Vec<u8>, value: u32| out.extend_from_slice(&value.to_be_bytes());
        push(&mut out, header_size as u32);
        push(&mut out, self.types.len() as u32);
        for word in self.header_words {
            push(&mut out, word);
        }
        for t in &self.types {
            out.extend_from_slice(&t.code);
            push(&mut out, t.header_runtime);
        }

        let mut data = Vec::new();
        let mut data_offset = directory_end;
        for t in &self.types {
            push(&mut out, (16 + 16 * t.resources.len()) as u32);
            push(&mut out, t.resources.len() as u32);
            out.extend_from_slice(&t.code);
            push(&mut out, t.section_runtime);
            out.extend_from_slice(&t.tag);
            for r in &t.resources {
                out.extend_from_slice(&r.id.to_be_bytes());
                out.extend_from_slice(&r.attributes.to_be_bytes());
                push(&mut out, r.data.len() as u32);
                push(&mut out, r.runtime);
                push(&mut out, data_offset as u32);
                data.extend_from_slice(&r.data);
                data_offset += r.data.len();
                if !data_offset.is_multiple_of(2) {
                    data.push(0);
                    data_offset += 1;
                }
            }
        }
        out.extend_from_slice(&data);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut library = ResourceLibrary::new();
        library.insert(b"CODE", Resource::new(0xF3AD, b"OBJR".to_vec()));
        library.insert(b"CPNM", Resource::new(0xF3AD, b"PANEL.X\0".to_vec()));
        library.to_bytes()
    }

    #[test]
    fn writes_what_it_parses() {
        let bytes = sample();
        let library = ResourceLibrary::parse(&bytes).unwrap();
        assert_eq!(library.to_bytes(), bytes);
        assert_eq!(library.get(b"CPNM", 0xF3AD).unwrap().data, b"PANEL.X\0");
        // Header (16 + 8 × 2), two sections of one resource, then the data.
        assert_eq!(u32_at(&bytes, 0), Some(32));
        assert_eq!(bytes.len(), 4 + 32 + 2 * 36 + 4 + 8);
    }

    #[test]
    fn inserts_in_descending_id_order_and_replaces() {
        let mut library = ResourceLibrary::parse(&sample()).unwrap();
        library.insert(b"CODE", Resource::new(0xF3B0, vec![1, 2, 3]));
        library.insert(b"CODE", Resource::new(0xF3A0, vec![4]));
        library.insert(b"CODE", Resource::new(0xF3B0, vec![5]));
        let ids: Vec<u16> = library.types[0].resources.iter().map(|r| r.id).collect();
        assert_eq!(ids, [0xF3B0, 0xF3AD, 0xF3A0]);
        let reparsed = ResourceLibrary::parse(&library.to_bytes()).unwrap();
        assert_eq!(reparsed.get(b"CODE", 0xF3B0).unwrap().data, [5]);
        assert_eq!(reparsed.get(b"CODE", 0xF3A0).unwrap().data, [4]);
    }
}
