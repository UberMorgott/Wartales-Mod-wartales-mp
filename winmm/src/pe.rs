//! Reads the export table of a PE file (IMAGE_EXPORT_DIRECTORY at data
//! directory 0, walked by hand). Shared by build.rs (the forwarding thunks)
//! and the tests, so it depends on nothing in the crate.

/// One entry of a PE export directory; `name` is None for a NONAME export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: Option<String>,
    pub ordinal: u32,
}

type Res<T> = Result<T, String>;

fn u16_at(b: &[u8], off: usize) -> Res<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or("truncated PE header".into())
}

fn u32_at(b: &[u8], off: usize) -> Res<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or("truncated PE header".into())
}

/// A PE file's sections: (virtual address, virtual size, raw data).
struct Image<'a> {
    sections: Vec<(u32, u32, &'a [u8])>,
}

impl Image<'_> {
    /// The section bytes starting at rva.
    fn at(&self, rva: u32) -> Res<&[u8]> {
        for &(va, vsize, data) in &self.sections {
            if rva < va || rva >= va.wrapping_add(vsize) {
                continue;
            }
            return data
                .get((rva - va) as usize..)
                .filter(|d| !d.is_empty())
                .ok_or(format!("rva {rva:#x} past raw data"));
        }
        Err(format!("rva {rva:#x} is in no section"))
    }
    fn u32(&self, rva: u32) -> Res<u32> {
        u32_at(self.at(rva)?, 0).map_err(|_| format!("truncated read at rva {rva:#x}"))
    }
    fn u16(&self, rva: u32) -> Res<u16> {
        u16_at(self.at(rva)?, 0).map_err(|_| format!("truncated read at rva {rva:#x}"))
    }
    fn cstring(&self, rva: u32) -> Res<String> {
        let b = self.at(rva)?;
        let end = b
            .iter()
            .position(|&c| c == 0)
            .ok_or(format!("unterminated string at rva {rva:#x}"))?;
        String::from_utf8(b[..end].to_vec())
            .map_err(|_| format!("non-UTF-8 export name at rva {rva:#x}"))
    }
}

/// Every export of a PE image (named and NONAME), sorted by ordinal.
pub fn exports(f: &[u8]) -> Res<Vec<Export>> {
    if f.get(..2) != Some(b"MZ") {
        return Err("not a PE file".into());
    }
    let pe = u32_at(f, 0x3c)? as usize;
    if f.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("not a PE file".into());
    }
    let coff = pe + 4;
    let nsections = usize::from(u16_at(f, coff + 2)?);
    let opt_size = usize::from(u16_at(f, coff + 16)?);
    let opt = coff + 20;
    // PE32 / PE32+: NumberOfRvaAndSizes, then the data directories.
    let (nrva_off, dirs) = match u16_at(f, opt)? {
        0x10b => (92, 96),
        0x20b => (108, 112),
        _ => return Err("no optional header".into()),
    };
    let (va, size) = if opt_size >= dirs + 8 && u32_at(f, opt + nrva_off)? > 0 {
        (u32_at(f, opt + dirs)?, u32_at(f, opt + dirs + 4)?)
    } else {
        (0, 0)
    };
    if va == 0 || size == 0 {
        return Err("no export directory".into());
    }
    let mut sections = Vec::with_capacity(nsections);
    for i in 0..nsections {
        let s = opt + opt_size + 40 * i;
        let (vsize, sva) = (u32_at(f, s + 8)?, u32_at(f, s + 12)?);
        let (raw_size, raw_ptr) = (u32_at(f, s + 16)? as usize, u32_at(f, s + 20)? as usize);
        // No raw data (PointerToRawData 0) = uninitialized section.
        let raw = if raw_ptr == 0 {
            0..0
        } else {
            raw_ptr..raw_ptr + raw_size
        };
        let data = f.get(raw).ok_or(format!("section {i} past end of file"))?;
        sections.push((sva, vsize, data));
    }
    let m = Image { sections };
    let base = m.u32(va + 16)?;
    let func_count = m.u32(va + 20)?;
    let name_count = m.u32(va + 24)?;
    let funcs_rva = m.u32(va + 28)?;
    let names_rva = m.u32(va + 32)?;
    let ords_rva = m.u32(va + 36)?;
    let mut names = vec![None; func_count as usize];
    for i in 0..name_count {
        let name = m.cstring(m.u32(names_rva + i * 4)?)?;
        let idx = usize::from(m.u16(ords_rva + i * 2)?);
        let Some(slot) = names.get_mut(idx) else {
            return Err(format!("name {name:?} points past the function table"));
        };
        *slot = Some(name);
    }
    let mut out = Vec::with_capacity(names.len());
    for (i, name) in (0..func_count).zip(names) {
        // An all-zero address is an unused slot in the function table.
        if name.is_some() || m.u32(funcs_rva + i * 4)? != 0 {
            out.push(Export {
                name,
                ordinal: base + i,
            });
        }
    }
    Ok(out)
}
