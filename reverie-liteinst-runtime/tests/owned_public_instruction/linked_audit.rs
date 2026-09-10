//! Symbol-range linked audit for the public guest bracket.
//!
//! The superseded audit extracted `public_entry` only up to the first blank line in
//! `objdump` output. That blank line precedes the nested `public_first` symbol, so
//! the excerpt ended at `mov $0x27,%eax` and contained no `syscall` at all, and the
//! predicate that searched it never required one. It passed vacuously.
//!
//! This auditor takes the range from the symbol table instead: `public_entry` is a
//! sized `FUNC`, so `[address, address + size)` is the whole body including every
//! internal label. Every requirement below is positive — something must be present
//! and in the right place — so a truncated body, a missing endpoint or an inserted
//! call is rejected rather than silently accepted.

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Symbol {
    pub address: u64,
    pub size: u64,
    pub kind: char,
    pub name: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Instruction {
    pub address: u64,
    pub mnemonic: String,
    pub operands: String,
}

impl Instruction {
    fn calls(&self, needle: &str) -> bool {
        self.mnemonic == "call" && self.operands.contains(needle)
    }
}

/// Parse `nm -S --defined-only`: `<addr> <size> <kind> <name>`, plus unsized
/// `<addr> <kind> <name>` lines, which are kept with size 0.
pub fn parse_symbols(text: &str) -> Vec<Symbol> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                [address, size, kind, name] if kind.len() == 1 => Some(Symbol {
                    address: u64::from_str_radix(address, 16).ok()?,
                    size: u64::from_str_radix(size, 16).ok()?,
                    kind: kind.chars().next()?,
                    name: (*name).to_owned(),
                }),
                [address, kind, name] if kind.len() == 1 => Some(Symbol {
                    address: u64::from_str_radix(address, 16).ok()?,
                    size: 0,
                    kind: kind.chars().next()?,
                    name: (*name).to_owned(),
                }),
                _ => None,
            }
        })
        .collect()
}

/// Parse `objdump -d --no-show-raw-insn` instruction lines: `  <addr>:\t<text>`.
/// Symbol headers, blank lines and section banners are ignored, so a nested label
/// cannot end the scan.
pub fn parse_disassembly(text: &str) -> Vec<Instruction> {
    text.lines()
        .filter_map(|line| {
            let (address, rest) = line.split_once(":\t")?;
            let address = u64::from_str_radix(address.trim(), 16).ok()?;
            let rest = rest.trim();
            let (mnemonic, operands) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            Some(Instruction {
                address,
                mnemonic: mnemonic.trim().to_owned(),
                operands: operands.trim().to_owned(),
            })
        })
        .collect()
}

/// One `readelf -SW` section header row we care about.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Section {
    pub name: String,
    pub address: u64,
    pub size: u64,
}

/// Parse `readelf -SW`: `  [nn] <name> <type> <address> <offset> <size> ...`.
pub fn parse_sections(text: &str) -> Vec<Section> {
    text.lines()
        .filter_map(|line| {
            let rest = line.split_once(']')?.1;
            let fields: Vec<&str> = rest.split_whitespace().collect();
            let [name, _kind, address, _offset, size, ..] = fields.as_slice() else {
                return None;
            };
            Some(Section {
                name: (*name).to_owned(),
                address: u64::from_str_radix(address, 16).ok()?,
                size: u64::from_str_radix(size, 16).ok()?,
            })
        })
        .collect()
}

/// One relative relocation: where it is applied and the address it installs.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Relocation {
    pub offset: u64,
    pub addend: u64,
}

/// Parse the `R_X86_64_RELATIVE` rows of `readelf -rW`, whose last field is the
/// hexadecimal addend actually written into the slot.
pub fn parse_relative_relocations(text: &str) -> Vec<Relocation> {
    text.lines()
        .filter(|line| line.contains("R_X86_64_RELATIVE"))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            Some(Relocation {
                offset: u64::from_str_radix(fields.first()?, 16).ok()?,
                addend: u64::from_str_radix(fields.last()?, 16).ok()?,
            })
        })
        .collect()
}

/// What the linked `.init_array` actually installs for the default-feature
/// constructor.
#[derive(Debug, Eq, PartialEq)]
pub struct ConstructorBinding {
    pub init_array: (u64, u64),
    pub slot: u64,
    pub installs: u64,
    pub initialize: u64,
}

/// Prove from the ELF, not from a comment, that this binary links the default
/// `preload-constructor` entry and that the slot really points at
/// `reverie_liteinst_initialize`.
///
/// The Rust symbol is mangled, so `REVERIE_LITEINST_INIT` is matched as a suffix.
pub fn constructor_binding(
    symbols: &[Symbol],
    sections: &[Section],
    relocations: &[Relocation],
) -> Result<ConstructorBinding, String> {
    let init_array = sections
        .iter()
        .find(|section| section.name == ".init_array")
        .ok_or("the binary has no .init_array section")?;
    if init_array.size == 0 {
        return Err(".init_array is empty".into());
    }
    let end = init_array
        .address
        .checked_add(init_array.size)
        .ok_or(".init_array range overflows")?;

    let slot = symbols
        .iter()
        .filter(|symbol| symbol.name.ends_with("REVERIE_LITEINST_INIT"))
        .map(|symbol| symbol.address)
        .collect::<Vec<_>>();
    let slot = match slot.as_slice() {
        [only] => *only,
        [] => return Err("REVERIE_LITEINST_INIT is not defined in the binary".into()),
        many => {
            return Err(format!(
                "REVERIE_LITEINST_INIT is defined {} times",
                many.len()
            ));
        }
    };
    if !(init_array.address <= slot && slot < end) {
        return Err(format!(
            "REVERIE_LITEINST_INIT {slot:#x} lies outside .init_array [{:#x},{end:#x})",
            init_array.address
        ));
    }

    let initialize = symbols
        .iter()
        .find(|symbol| symbol.name == "reverie_liteinst_initialize")
        .ok_or("reverie_liteinst_initialize is not defined in the binary")?
        .address;

    let installs = relocations
        .iter()
        .find(|relocation| relocation.offset == slot)
        .ok_or_else(|| format!("no relative relocation applies to the slot at {slot:#x}"))?
        .addend;
    if installs != initialize {
        return Err(format!(
            "the .init_array slot installs {installs:#x}, not reverie_liteinst_initialize {initialize:#x}"
        ));
    }

    Ok(ConstructorBinding {
        init_array: (init_array.address, end),
        slot,
        installs,
        initialize,
    })
}

#[derive(Debug, Eq, PartialEq)]
pub struct Report {
    pub entry: u64,
    pub end: u64,
    pub first: u64,
    pub instructions: usize,
    pub loops: usize,
    pub rdtsc_seed: u64,
}

fn symbol<'a>(symbols: &'a [Symbol], name: &str) -> Result<&'a Symbol, String> {
    symbols
        .iter()
        .find(|symbol| symbol.name == name)
        .ok_or_else(|| format!("symbol {name} is not defined in the binary"))
}

/// Audit the complete `public_entry` body. `Err` is a rejection with its reason.
pub fn audit(symbols: &[Symbol], disassembly: &[Instruction]) -> Result<Report, String> {
    let entry = symbol(symbols, "public_entry")?;
    if entry.size == 0 {
        return Err("public_entry has no symbol size; cannot bound the body".into());
    }
    let end = entry
        .address
        .checked_add(entry.size)
        .ok_or("public_entry range overflows")?;
    let first = symbol(symbols, "public_first")?;
    if !(entry.address < first.address && first.address < end) {
        return Err(format!(
            "public_first {:#x} is not an internal label of public_entry [{:#x},{:#x})",
            first.address, entry.address, end
        ));
    }

    let body: Vec<&Instruction> = disassembly
        .iter()
        .filter(|insn| insn.address >= entry.address && insn.address < end)
        .collect();
    if body.is_empty() {
        return Err("no instruction decoded inside the public_entry range".into());
    }
    if body[0].address != entry.address {
        return Err(format!(
            "no instruction at the public_entry address {:#x}",
            entry.address
        ));
    }

    let at_first = body
        .iter()
        .find(|insn| insn.address == first.address)
        .ok_or_else(|| {
            format!(
                "body is truncated: no instruction at public_first {:#x}",
                first.address
            )
        })?;
    if at_first.mnemonic != "syscall" {
        return Err(format!(
            "public_first must bind the first guest syscall, found {}",
            at_first.mnemonic
        ));
    }

    let position = |needle: &str| -> Result<usize, String> {
        let found: Vec<usize> = body
            .iter()
            .enumerate()
            .filter(|(_, insn)| insn.calls(needle))
            .map(|(index, _)| index)
            .collect();
        match found.as_slice() {
            [only] => Ok(*only),
            [] => Err(format!("no call to {needle} inside public_entry")),
            many => Err(format!(
                "expected one call to {needle}, found {}",
                many.len()
            )),
        }
    };
    let begin = position("clock_constructor_begin")?;
    let initialize = position("initialize")?;
    let finish = position("clock_constructor_finish")?;
    if !(begin < initialize && initialize < finish) {
        return Err("begin, initializer and finish are not in that order".into());
    }
    let first_index = body
        .iter()
        .position(|insn| insn.address == first.address)
        .expect("checked above");
    if finish >= first_index {
        return Err("the finish call does not precede the first guest syscall".into());
    }
    if let Some(intervening) = body[finish + 1..first_index]
        .iter()
        .find(|insn| insn.mnemonic == "call")
    {
        return Err(format!(
            "call {} executes between the physical enable and the first guest syscall",
            intervening.operands
        ));
    }

    for (mnemonic, expected) in [("syscall", 3), ("cpuid", 1), ("rdtsc", 1), ("rdtscp", 1)] {
        let actual = body.iter().filter(|insn| insn.mnemonic == mnemonic).count();
        if actual != expected {
            return Err(format!("expected {expected} {mnemonic}, found {actual}"));
        }
    }

    let mut loops = 0;
    for window in body.windows(3) {
        let [load, decrement, branch] = window else {
            continue;
        };
        if load.mnemonic == "mov"
            && load.operands.starts_with("$0x4,%ecx")
            && decrement.mnemonic == "dec"
            && decrement.operands.starts_with("%ecx")
            && branch.mnemonic == "jne"
            && branch
                .operands
                .split_whitespace()
                .next()
                .and_then(|target| u64::from_str_radix(target.trim_start_matches("0x"), 16).ok())
                == Some(decrement.address)
        {
            loops += 1;
        }
    }
    if loops != 5 {
        return Err(format!(
            "expected five four-iteration dec/jne loops, found {loops}"
        ));
    }

    let trailing: Vec<&&Instruction> = body[finish + 1..]
        .iter()
        .filter(|insn| insn.mnemonic == "call")
        .collect();
    match trailing.as_slice() {
        [enter, verify]
            if enter.operands.contains("reverie_liteinst_clock_enter")
                && verify.operands.contains("verify") =>
        {
            if enter.address <= at_first.address {
                return Err("clock_enter must close the guest region, not precede it".into());
            }
        }
        other => {
            return Err(format!(
                "expected exactly clock_enter then verify after the enable, found {:?}",
                other
                    .iter()
                    .map(|insn| insn.operands.as_str())
                    .collect::<Vec<_>>()
            ));
        }
    }

    let last = body.last().expect("non-empty");
    if last.mnemonic != "ud2" {
        return Err(format!(
            "body does not reach its terminal ud2; last instruction is {} at {:#x}",
            last.mnemonic, last.address
        ));
    }

    let rdtsc_index = body
        .iter()
        .position(|insn| insn.mnemonic == "rdtsc")
        .ok_or("no rdtsc inside public_entry")?;
    if rdtsc_index == 0 {
        return Err("rdtsc has no predecessor inside public_entry".into());
    }
    let seed = body[rdtsc_index - 1];
    if !(seed.mnemonic == "mov"
        && seed.operands.contains("0x33333333")
        && seed.operands.ends_with("%ecx"))
    {
        return Err(format!(
            "RDTSC ECX liveness: the instruction before rdtsc must seed ECX with the sentinel, found {} {} at {:#x}",
            seed.mnemonic, seed.operands, seed.address
        ));
    }

    Ok(Report {
        entry: entry.address,
        end,
        first: first.address,
        instructions: body.len(),
        loops,
        rdtsc_seed: seed.address,
    })
}
