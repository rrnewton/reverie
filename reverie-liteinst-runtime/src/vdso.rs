use std::fs::File;
use std::io;
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;
use std::sync::OnceLock;

mod elf;
pub(crate) mod protection;
pub(crate) mod rng;
#[cfg(test)]
mod tests;

pub(crate) const EXECUTE_ACCESS_ERROR: i32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Time,
    Gettimeofday,
    ClockGettime,
    ClockGetres,
    Getcpu,
}

impl Operation {
    fn from_name(name: &str) -> Option<Self> {
        match name.strip_prefix("__vdso_").unwrap_or(name) {
            "time" => Some(Self::Time),
            "gettimeofday" => Some(Self::Gettimeofday),
            "clock_gettime" => Some(Self::ClockGettime),
            "clock_getres" => Some(Self::ClockGetres),
            "getcpu" => Some(Self::Getcpu),
            _ => None,
        }
    }

    pub(crate) fn number(self) -> i64 {
        match self {
            Self::Time => libc::SYS_time,
            Self::Gettimeofday => libc::SYS_gettimeofday,
            Self::ClockGettime => libc::SYS_clock_gettime,
            Self::ClockGetres => libc::SYS_clock_getres,
            Self::Getcpu => libc::SYS_getcpu,
        }
    }

    pub(crate) fn arguments(self, context: &liteinst2::trampoline::HookContext) -> [u64; 6] {
        match self {
            Self::Time => [context.rdi, 0, 0, 0, 0, 0],
            Self::Getcpu => [context.rdi, context.rsi, context.rdx, 0, 0, 0],
            _ => [context.rdi, context.rsi, 0, 0, 0, 0],
        }
    }

    fn buffers(self, arguments: [u64; 6]) -> [(u64, u64); 2] {
        match self {
            Self::Time => [(arguments[0], 8), (0, 0)],
            Self::Gettimeofday => [(arguments[0], 16), (arguments[1], 8)],
            Self::ClockGettime | Self::ClockGetres => [(arguments[1], 16), (0, 0)],
            Self::Getcpu => [(arguments[0], 4), (arguments[1], 4)],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mapping {
    range: Range<u64>,
    protection: i32,
    offset: u64,
    device: (u64, u64),
    inode: u64,
    name: String,
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

fn mappings(text: &str) -> io::Result<Vec<Mapping>> {
    let mut result: Vec<Mapping> = Vec::new();
    for line in text.lines() {
        if result.len() == 4096 {
            return Err(unsupported("vDSO mapping inventory limit"));
        }
        let mut fields = line.split_whitespace();
        let mut field = || {
            fields
                .next()
                .ok_or_else(|| unsupported("malformed vDSO mapping inventory"))
        };
        let (begin, end) = field()?
            .split_once('-')
            .ok_or_else(|| unsupported("mapping interval"))?;
        let permissions = field()?.as_bytes();
        let offset = u64::from_str_radix(field()?, 16).map_err(io::Error::other)?;
        let (major, minor) = field()?
            .split_once(':')
            .ok_or_else(|| unsupported("mapping device"))?;
        let device = (
            u64::from_str_radix(major, 16).map_err(io::Error::other)?,
            u64::from_str_radix(minor, 16).map_err(io::Error::other)?,
        );
        let inode = field()?.parse().map_err(io::Error::other)?;
        let begin = u64::from_str_radix(begin, 16).map_err(io::Error::other)?;
        let end = u64::from_str_radix(end, 16).map_err(io::Error::other)?;
        if begin >= end
            || begin % 4096 != 0
            || end % 4096 != 0
            || result
                .last()
                .is_some_and(|previous| previous.range.end > begin)
            || permissions.len() != 4
            || !matches!(permissions[0], b'r' | b'-')
            || !matches!(permissions[1], b'w' | b'-')
            || !matches!(permissions[2], b'x' | b'-')
            || !matches!(permissions[3], b'p' | b's')
        {
            return Err(unsupported("unsupported vDSO mapping inventory"));
        }
        let protection = (i32::from(permissions[0] == b'r') * libc::PROT_READ)
            | (i32::from(permissions[1] == b'w') * libc::PROT_WRITE)
            | (i32::from(permissions[2] == b'x') * libc::PROT_EXEC);
        result.push(Mapping {
            range: begin..end,
            protection,
            offset,
            device,
            inode,
            name: fields.next().unwrap_or("").to_owned(),
        });
    }
    Ok(result)
}

fn auxv_vdso(bytes: &[u8]) -> io::Result<u64> {
    if !bytes.len().is_multiple_of(16) || bytes.len() > 4096 {
        return Err(unsupported("original auxv bounds"));
    }
    let mut vdso = None;
    for pair in bytes.as_chunks::<16>().0 {
        let key = u64::from_ne_bytes(pair[..8].try_into().unwrap());
        let value = u64::from_ne_bytes(pair[8..].try_into().unwrap());
        if key == 0 {
            return vdso
                .filter(|value| *value != 0)
                .ok_or_else(|| unsupported("original auxv has no vDSO"));
        }
        if key == libc::AT_SYSINFO_EHDR && vdso.replace(value).is_some() {
            return Err(unsupported("duplicate original vDSO auxv"));
        }
    }
    Err(unsupported("unterminated original auxv"))
}

fn selected_maps(all: &[Mapping], address: u64) -> io::Result<Vec<Mapping>> {
    let mut selected: Vec<_> = all
        .iter()
        .filter(|map| matches!(map.name.as_str(), "[vdso]" | "[vvar]" | "[vvar_vclock]"))
        .cloned()
        .collect();
    if selected.iter().filter(|map| map.name == "[vdso]").count() != 1
        || selected.iter().filter(|map| map.name == "[vvar]").count() != 1
        || selected
            .iter()
            .filter(|map| map.name == "[vvar_vclock]")
            .count()
            > 1
        || selected.iter().any(|map| {
            map.offset != 0
                || map.device != (0, 0)
                || map.inode != 0
                || map.range.end >= 1 << 47
                || map.protection
                    != if map.name == "[vdso]" {
                        libc::PROT_READ | libc::PROT_EXEC
                    } else {
                        libc::PROT_READ
                    }
        })
        || !selected
            .iter()
            .any(|map| map.name == "[vdso]" && map.range.start == address)
    {
        return Err(unsupported("kernel vDSO/vvar mapping identity"));
    }
    selected.sort_by_key(|map| map.name == "[vdso]");
    Ok(selected)
}

fn read_image(range: &Range<u64>) -> io::Result<Vec<u8>> {
    let length = range
        .end
        .checked_sub(range.start)
        .filter(|length| *length <= 1024 * 1024)
        .ok_or_else(|| unsupported("vDSO image bounds"))?;
    let mut bytes = vec![0; length as usize];
    File::open("/proc/self/mem")?.read_exact_at(&mut bytes, range.start)?;
    Ok(bytes)
}

struct Prepared {
    tid: i64,
    mappings: Vec<Mapping>,
    bytes: Vec<u8>,
    exports: Vec<elf::Export>,
    writable: Vec<Range<u64>>,
}

static PREPARED: OnceLock<Mutex<Option<Prepared>>> = OnceLock::new();

/// Bind the original kernel vDSO before owned installation, without changing protections.
///
/// # Safety
/// The controlled private CRT owns the sole thread, has established its private TLS,
/// and has disabled private libc vDSO lookup before libc initialization. Guest mapping
/// contents, permissions and private GNU lookup state must remain stable through
/// activation and the owned runtime lifetime. This is not an admission capability.
pub unsafe fn prepare_private() -> io::Result<()> {
    if unsafe { libc::getauxval(libc::AT_SYSINFO_EHDR) } != 0 {
        return Err(unsupported(
            "private GNU auxv must disable private vDSO resolution",
        ));
    }
    let address = auxv_vdso(&std::fs::read("/proc/self/auxv")?)?;
    let all = mappings(&std::fs::read_to_string("/proc/self/maps")?)?;
    let mappings = selected_maps(&all, address)?;
    let range = mappings
        .iter()
        .find(|map| map.name == "[vdso]")
        .unwrap()
        .range
        .clone();
    let bytes = read_image(&range)?;
    let exports = elf::exports(&bytes, range)?;
    let executable = File::open("/proc/self/exe")?.metadata()?;
    let device = (libc::major(executable.dev()), libc::minor(executable.dev()));
    let writable = all
        .iter()
        .filter(|map| {
            map.protection & libc::PROT_WRITE != 0
                && (map.name == "[stack]"
                    || (map.inode == executable.ino()
                        && map.device == (u64::from(device.0), u64::from(device.1))))
        })
        .map(|map| map.range.clone())
        .collect();
    let tid = unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6]) };
    if tid <= 0 {
        return Err(unsupported("private vDSO owner TID"));
    }
    PREPARED
        .set(Mutex::new(Some(Prepared {
            tid,
            mappings,
            bytes,
            exports,
            writable,
        })))
        .map_err(|_| io::Error::other("private vDSO owner already prepared"))
}

pub(crate) struct Active {
    owner: i64,
    kernel_mappings: Vec<Mapping>,
    pub(crate) range: Range<u64>,
    vvar: Vec<Range<u64>>,
    exports: Vec<elf::Export>,
    writable: Vec<Range<u64>>,
    changes: Vec<protection::Change>,
    _image: Vec<u8>,
}

pub(crate) fn activate_prepared() -> io::Result<Option<Active>> {
    let Some(prepared) = PREPARED.get() else {
        return Ok(None);
    };
    let mut lock = prepared
        .lock()
        .map_err(|_| io::Error::other("private vDSO owner poisoned"))?;
    let prepared = lock
        .take()
        .ok_or_else(|| io::Error::other("private vDSO owner consumed"))?;
    if prepared.tid != unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6]) }
        || unsafe { libc::getauxval(libc::AT_SYSINFO_EHDR) } != 0
    {
        return Err(unsupported("private vDSO owner changed"));
    }
    let range = prepared
        .mappings
        .iter()
        .find(|map| map.name == "[vdso]")
        .unwrap()
        .range
        .clone();
    let current = selected_maps(
        &mappings(&std::fs::read_to_string("/proc/self/maps")?)?,
        range.start,
    )?;
    if current != prepared.mappings || read_image(&range)? != prepared.bytes {
        return Err(unsupported("retained vDSO mapping or bytes changed"));
    }
    let changes: Vec<_> = prepared
        .mappings
        .iter()
        .map(|map| protection::Change {
            range: map.range.clone(),
            before: map.protection,
            after: if map.name == "[vdso]" {
                libc::PROT_READ
            } else {
                libc::PROT_NONE
            },
        })
        .collect();
    let active = Active {
        owner: prepared.tid,
        kernel_mappings: prepared.mappings.clone(),
        range,
        vvar: prepared
            .mappings
            .iter()
            .filter(|map| map.name != "[vdso]")
            .map(|map| map.range.clone())
            .collect(),
        exports: prepared.exports,
        writable: prepared.writable,
        changes,
        _image: prepared.bytes,
    };
    protection::apply(&active.changes, |range, protection| unsafe {
        reverie_preload::trap::raw_syscall6(
            libc::SYS_mprotect,
            [
                range.start,
                range.end - range.start,
                protection as u64,
                0,
                0,
                0,
            ],
        )
    })
    .map_err(io::Error::other)?;
    Ok(Some(active))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) entry: u64,
    pub(crate) stack: u64,
    pub(crate) target: u64,
    pub(crate) operation: Operation,
}

pub(crate) struct Fault<'a> {
    pub(crate) signal: i32,
    pub(crate) code: i32,
    pub(crate) address: u64,
    pub(crate) registers: &'a [libc::greg_t; 23],
}

pub(crate) fn covered(ranges: &[Range<u64>], address: u64, length: u64) -> bool {
    let Some(end) = address.checked_add(length).filter(|end| *end < 1 << 47) else {
        return false;
    };
    if address == 0 {
        return false;
    }
    let mut cursor = address;
    while cursor < end {
        let Some(range) = ranges.iter().find(|range| range.contains(&cursor)) else {
            return false;
        };
        cursor = range.end.min(end);
    }
    true
}

impl Active {
    pub(crate) fn rng_binding(&self) -> Result<rng::RngBinding<'_>, rng::Refusal> {
        rng::RngBinding::bind(self)
    }

    #[cfg(test)]
    pub(crate) fn stepping_model(range: Range<u64>, recognized: u64) -> Self {
        Self {
            owner: 0,
            kernel_mappings: Vec::new(),
            vvar: vec![range.start - 4096..range.start],
            exports: vec![elf::Export {
                address: recognized,
                name: "__vdso_time".into(),
                version: "LINUX_2.6".into(),
                operation: Some(Operation::Time),
            }],
            range,
            writable: Vec::new(),
            changes: Vec::new(),
            _image: Vec::new(),
        }
    }

    pub(crate) fn native_pc(&self, pc: u64) -> bool {
        self.range.contains(&pc) && self.operation(pc).is_none()
    }

    pub(crate) fn native_entry(&self, fault: &Fault<'_>) -> bool {
        let pc = fault.registers[libc::REG_RIP as usize] as u64;
        self.native_pc(pc)
            && fault.signal == libc::SIGSEGV
            && fault.code == EXECUTE_ACCESS_ERROR
            && fault.address == pc
            && fault.registers[libc::REG_TRAPNO as usize] == 14
            && fault.registers[libc::REG_ERR as usize] == 0x15
            && fault.registers[libc::REG_EFL as usize] as u64 & 0x10000 != 0
    }

    pub(crate) fn enable_execution(&self, pc: u64) -> Result<(), i64> {
        if !self.native_pc(pc) {
            return Err(-i64::from(libc::EINVAL));
        }
        protection::enable_execution(&self.range)
    }

    pub(crate) fn report_exclusion_with(
        &self,
        pc: u64,
        mut emit: impl FnMut(&str, &str, Option<i64>),
    ) {
        let interval = if self.range.contains(&pc) {
            Some(("vdso", &self.range))
        } else {
            self.vvar
                .iter()
                .find(|range| range.contains(&pc))
                .map(|range| ("vvar", range))
        };
        if let Some((kind, range)) = interval {
            emit("step/vdso-interval", kind, None);
            for (high, low, value) in [
                ("start-high32", "start-low32", range.start),
                ("end-high32", "end-low32", range.end),
                ("offset-high32", "offset-low32", pc - range.start),
            ] {
                emit("step/vdso-interval", high, Some((value >> 32) as i64));
                emit("step/vdso-interval", low, Some(i64::from(value as u32)));
            }
        } else {
            emit("step/vdso-interval", "no-match", None);
        }
        let mut matches = 0;
        for export in self.exports.iter().filter(|export| export.address == pc) {
            emit("step/vdso-export", "match-index", Some(matches));
            emit(
                "step/vdso-export-name",
                &export.name,
                Some(export.name.len() as i64),
            );
            emit(
                "step/vdso-export-version",
                &export.version,
                Some(export.version.len() as i64),
            );
            matches += 1;
        }
        emit("step/vdso-export", "match-count", Some(matches));
        if matches == 0 {
            emit("step/vdso-export", "no-match", None);
        }
    }

    pub(crate) fn report_data_fault_with(
        &self,
        pc: u64,
        address: u64,
        mut emit: impl FnMut(&str, &str, Option<i64>),
    ) {
        for (high, low, value) in [
            ("base-high32", "base-low32", self.range.start),
            ("end-high32", "end-low32", self.range.end),
        ] {
            emit("vdso/retained-image", high, Some((value >> 32) as i64));
            emit("vdso/retained-image", low, Some(i64::from(value as u32)));
        }
        let interval = if self.range.contains(&address) {
            Some(("vdso", &self.range))
        } else {
            self.vvar
                .iter()
                .find(|range| range.contains(&address))
                .map(|range| ("vvar-family", range))
        };
        if let Some((kind, range)) = interval {
            emit("vdso/fault-interval", kind, None);
            for (high, low, value) in [
                ("start-high32", "start-low32", range.start),
                ("end-high32", "end-low32", range.end),
            ] {
                emit("vdso/fault-interval", high, Some((value >> 32) as i64));
                emit("vdso/fault-interval", low, Some(i64::from(value as u32)));
            }
        } else {
            emit("vdso/fault-interval", "no-match", None);
        }
        if !self.range.contains(&pc) {
            emit("vdso/retained-pc-bytes", "pc-outside-image", None);
        } else if let Some(bytes) = usize::try_from(pc - self.range.start)
            .ok()
            .and_then(|offset| self._image.get(offset..))
            .filter(|bytes| !bytes.is_empty())
        {
            let length = bytes.len().min(15).min((self.range.end - pc) as usize);
            emit("vdso/retained-pc-bytes", "length", Some(length as i64));
            for (index, byte) in bytes[..length].iter().enumerate() {
                emit("vdso/retained-pc-byte", "index", Some(index as i64));
                emit("vdso/retained-pc-byte", "value", Some(i64::from(*byte)));
            }
        } else {
            emit("vdso/retained-pc-bytes", "image-bytes-unavailable", None);
        }
        let mut count = 0;
        for export in self
            .exports
            .iter()
            .filter(|export| matches!(export.name.as_str(), "__vdso_getrandom" | "getrandom"))
        {
            emit("vdso/retained-getrandom", "index", Some(count));
            emit("vdso/retained-getrandom-name", &export.name, None);
            emit("vdso/retained-getrandom-version", &export.version, None);
            emit(
                "vdso/retained-getrandom",
                "entry-high32",
                Some((export.address >> 32) as i64),
            );
            emit(
                "vdso/retained-getrandom",
                "entry-low32",
                Some(i64::from(export.address as u32)),
            );
            count += 1;
        }
        emit("vdso/retained-getrandom", "count", Some(count));
        if count == 0 {
            emit("vdso/retained-getrandom", "no-match", None);
        }
    }

    pub(crate) fn update_outputs(&mut self, ranges: &[Range<u64>]) {
        self.writable = ranges.to_vec();
    }
    pub(crate) fn accessible(&self, range: &Range<u64>) -> bool {
        !self
            .vvar
            .iter()
            .any(|protected| protected.start < range.end && range.start < protected.end)
    }
    pub(crate) fn concerns(&self, pc: u64, address: u64) -> bool {
        self.range.contains(&pc)
            || self.range.contains(&address)
            || self.vvar.iter().any(|range| range.contains(&address))
    }

    pub(crate) fn operation(&self, pc: u64) -> Option<Operation> {
        self.exports
            .iter()
            .find(|export| export.address == pc)
            .and_then(|export| export.operation)
    }

    pub(crate) fn call(
        &self,
        fault: Fault<'_>,
        target: u64,
        executable: &[Range<u64>],
    ) -> Option<Call> {
        let Fault {
            signal,
            code,
            address,
            registers,
        } = fault;
        let entry = registers[libc::REG_RIP as usize] as u64;
        let stack = registers[libc::REG_RSP as usize] as u64;
        if signal != libc::SIGSEGV
            || code != EXECUTE_ACCESS_ERROR
            || address != entry
            || registers[libc::REG_TRAPNO as usize] != 14
            || registers[libc::REG_ERR as usize] != 0x15
            || registers[libc::REG_EFL as usize] as u64 & 0x10000 == 0
            || registers[libc::REG_EFL as usize] as u64 & 0x20000 != 0
            || !covered(executable, target, 1)
            || stack % 16 != 8
            || stack.checked_add(8).is_none_or(|end| end >= 1 << 47)
        {
            return None;
        }
        Some(Call {
            entry,
            stack,
            target,
            operation: self.operation(entry)?,
        })
    }

    pub(crate) fn buffers_supported(&self, operation: Operation, arguments: [u64; 6]) -> bool {
        operation
            .buffers(arguments)
            .iter()
            .all(|(address, length)| *address == 0 || covered(&self.writable, *address, *length))
    }

    pub(crate) fn call_buffers_supported(&self, call: Call, arguments: [u64; 6]) -> bool {
        self.buffers_supported(call.operation, arguments)
            && call
                .operation
                .buffers(arguments)
                .iter()
                .all(|(address, length)| {
                    *address == 0
                        || address
                            .checked_add(*length)
                            .is_some_and(|end| end <= call.stack || *address >= call.stack + 8)
                })
    }
}

pub(crate) fn register_values(context: &liteinst2::trampoline::HookContext) -> [u64; 18] {
    [
        context.r8,
        context.r9,
        context.r10,
        context.r11,
        context.r12,
        context.r13,
        context.r14,
        context.r15,
        context.rdi,
        context.rsi,
        context.rbp,
        context.rbx,
        context.rdx,
        context.rax,
        context.rcx,
        context.stack_pointer,
        context.instruction_pointer,
        context.rflags,
    ]
}
