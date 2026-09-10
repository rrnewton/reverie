use std::io;
use std::ops::Range;

use super::Operation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Export {
    pub address: u64,
    pub name: String,
    pub version: String,
    pub operation: Option<Operation>,
}

fn invalid() -> io::Error {
    io::Error::other("unsupported or malformed retained vDSO ELF")
}

fn span(bytes: &[u8], offset: u64, size: u64) -> io::Result<&[u8]> {
    let end = offset.checked_add(size).ok_or_else(invalid)?;
    bytes
        .get(
            usize::try_from(offset).map_err(|_| invalid())?
                ..usize::try_from(end).map_err(|_| invalid())?,
        )
        .ok_or_else(invalid)
}

fn word(bytes: &[u8], offset: u64, size: u64) -> io::Result<u64> {
    let input = span(bytes, offset, size)?;
    let mut value = [0; 8];
    value[..input.len()].copy_from_slice(input);
    Ok(u64::from_le_bytes(value))
}

fn text(bytes: &[u8], offset: u64) -> io::Result<&str> {
    let tail = bytes
        .get(usize::try_from(offset).map_err(|_| invalid())?..)
        .ok_or_else(invalid)?;
    let end = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(invalid)?;
    std::str::from_utf8(&tail[..end]).map_err(|_| invalid())
}

pub(super) fn elf_hash(name: &str) -> u64 {
    let mut value = 0u32;
    for byte in name.bytes() {
        value = (value << 4).wrapping_add(u32::from(byte));
        let high = value & 0xf0000000;
        value = (value ^ (high >> 24)) & !high;
    }
    u64::from(value)
}

pub(super) fn exports(bytes: &[u8], mapping: Range<u64>) -> io::Result<Vec<Export>> {
    if bytes.len() < 64
        || bytes.len() > 1024 * 1024
        || mapping.start == 0
        || mapping.end >= 1 << 47
        || mapping.end.checked_sub(mapping.start) != Some(bytes.len() as u64)
        || bytes[..7] != *b"\x7fELF\x02\x01\x01"
        || word(bytes, 16, 2)? != 3
        || word(bytes, 18, 2)? != 62
        || word(bytes, 20, 4)? != 1
        || word(bytes, 52, 2)? != 64
        || word(bytes, 54, 2)? != 56
    {
        return Err(invalid());
    }
    let phoff = word(bytes, 32, 8)?;
    let phnum = word(bytes, 56, 2)?;
    if phnum == 0 || phnum > 32 {
        return Err(invalid());
    }
    span(bytes, phoff, phnum * 56)?;
    let mut load = None;
    let mut dynamic = None;
    for index in 0..phnum {
        let header = phoff + index * 56;
        let kind = word(bytes, header, 4)?;
        let flags = word(bytes, header + 4, 4)?;
        let offset = word(bytes, header + 8, 8)?;
        let address = word(bytes, header + 16, 8)?;
        let filesz = word(bytes, header + 32, 8)?;
        let memsz = word(bytes, header + 40, 8)?;
        if kind == 1 {
            if load.is_some()
                || offset != 0
                || address != 0
                || flags != 5
                || filesz != memsz
                || memsz == 0
                || memsz > bytes.len() as u64
                || word(bytes, header + 48, 8)? != 4096
            {
                return Err(invalid());
            }
            load = Some(memsz);
        } else if kind == 2 {
            if dynamic.is_some()
                || offset != address
                || filesz != memsz
                || memsz == 0
                || memsz % 16 != 0
            {
                return Err(invalid());
            }
            dynamic = Some((address, memsz));
        } else if kind == 3 || kind == 7 {
            return Err(invalid());
        }
    }
    let image = span(bytes, 0, load.ok_or_else(invalid)?)?;
    let (dynamic_start, dynamic_size) = dynamic.ok_or_else(invalid)?;
    span(image, dynamic_start, dynamic_size)?;
    let mut tags = std::collections::BTreeMap::new();
    let mut terminated = false;
    for index in 0..dynamic_size / 16 {
        let offset = dynamic_start + index * 16;
        let tag = word(image, offset, 8)?;
        let value = word(image, offset + 8, 8)?;
        if tag == 0 {
            terminated = true;
            break;
        }
        if matches!(
            tag,
            4 | 5 | 6 | 10 | 11 | 0x6ffffff0 | 0x6ffffffc | 0x6ffffffd
        ) && tags.insert(tag, value).is_some()
        {
            return Err(invalid());
        }
        if matches!(tag, 1 | 7 | 17 | 23) {
            return Err(invalid());
        }
    }
    if !terminated {
        return Err(invalid());
    }
    let get = |tag| tags.get(&tag).copied().ok_or_else(invalid);
    if get(11)? != 24 {
        return Err(invalid());
    }
    let strings = span(image, get(5)?, get(10)?)?;
    let hash = get(4)?;
    let buckets = word(image, hash, 4)?;
    let symbols = word(image, hash + 4, 4)?;
    if buckets == 0 || buckets > 4096 || symbols == 0 || symbols > 4096 {
        return Err(invalid());
    }
    let table = span(image, hash, (2 + buckets + symbols) * 4)?;
    for offset in (8..table.len()).step_by(4) {
        if word(table, offset as u64, 4)? >= symbols {
            return Err(invalid());
        }
    }
    for bucket in 0..buckets {
        let mut symbol = word(table, 8 + bucket * 4, 4)?;
        let mut remaining = symbols;
        while symbol != 0 {
            if remaining == 0 {
                return Err(invalid());
            }
            remaining -= 1;
            symbol = word(table, 8 + (buckets + symbol) * 4, 4)?;
        }
    }
    let symtab = span(image, get(6)?, symbols * 24)?;
    let versym = span(image, get(0x6ffffff0)?, symbols * 2)?;
    let mut versions = std::collections::BTreeMap::new();
    let mut definition = get(0x6ffffffc)?;
    let version_count = get(0x6ffffffd)?;
    if version_count == 0 || version_count > 32 {
        return Err(invalid());
    }
    for index in 0..version_count {
        span(image, definition, 20)?;
        if word(image, definition, 2)? != 1 || word(image, definition + 6, 2)? != 1 {
            return Err(invalid());
        }
        let key = word(image, definition + 4, 2)?;
        let aux = definition
            .checked_add(word(image, definition + 12, 4)?)
            .ok_or_else(invalid)?;
        span(image, aux, 8)?;
        if word(image, aux + 4, 4)? != 0 {
            return Err(invalid());
        }
        let name = text(strings, word(image, aux, 4)?)?.to_owned();
        if word(image, definition + 8, 4)? != elf_hash(&name) {
            return Err(invalid());
        }
        if key == 0 || key >= 0x8000 || versions.insert(key, name).is_some() {
            return Err(invalid());
        }
        let next = word(image, definition + 16, 4)?;
        if index + 1 == version_count {
            if next != 0 {
                return Err(invalid());
            }
        } else {
            if next < 20 {
                return Err(invalid());
            }
            definition = definition.checked_add(next).ok_or_else(invalid)?;
        }
    }
    let mut output: Vec<Export> = Vec::new();
    for index in 1..symbols {
        let symbol = index * 24;
        let info = word(symtab, symbol + 4, 1)?;
        let section = word(symtab, symbol + 6, 2)?;
        if section == 0 || info >> 4 == 0 {
            continue;
        }
        if info & 15 != 2 {
            if section == 0xfff1 {
                continue;
            }
            return Err(invalid());
        }
        if !matches!(info >> 4, 1 | 2) || word(symtab, symbol + 5, 1)? != 0 || section >= 0xff00 {
            return Err(invalid());
        }
        let address = word(symtab, symbol + 8, 8)?;
        let size = word(symtab, symbol + 16, 8)?;
        if size == 0 {
            return Err(invalid());
        }
        span(image, address, size)?;
        let name = text(strings, word(symtab, symbol, 4)?)?.to_owned();
        let version = versions
            .get(&word(versym, index * 2, 2)?)
            .ok_or_else(invalid)?
            .clone();
        let operation = if version == "LINUX_2.6" {
            Operation::from_name(&name)
        } else {
            None
        };
        let address = mapping.start.checked_add(address).ok_or_else(invalid)?;
        if output
            .iter()
            .any(|old| old.name == name || (old.address == address && old.operation != operation))
        {
            return Err(invalid());
        }
        output.push(Export {
            address,
            name,
            version,
            operation,
        });
    }
    if output.is_empty() {
        return Err(invalid());
    }
    Ok(output)
}
