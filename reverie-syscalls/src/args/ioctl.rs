/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Everything related to ioctl arguments.

use serde::Deserialize;
use serde::Serialize;

use crate::Addr;
use crate::AddrMut;
use crate::Errno;
use crate::FromToRaw;
use crate::MemoryAccess;

/// The type of ioctl from the perspective of userspace. That is, whether
/// userspace is reading, writing, or doing nothing.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Direction {
    /// Userspace is reading.
    Read,
    /// Userspace is writing.
    Write,
    /// There is neither reading nor writing. This is the case for ioctls that
    /// have value parameters instead of pointer parameters, and for requests
    /// whose memory effects cannot be decoded: unknown requests without an
    /// encoded size, and `TIOCLINUX`, whose effect depends on its subcommand.
    None,
}

command_enum! {
    /// An `ioctl` request.
    ///
    /// The `ioctl` syscall is a dumping ground for sending a request to a file
    /// descriptor. This is not a complete list of all the possible requests, but
    /// we try to have the most-commonly used requests listed.
    ///
    /// See [`ioctl_list(2)`][ioctl_list] for a more complete list.
    ///
    /// [ioctl_list]: http://man7.org/linux/man-pages/man2/ioctl_list.2.html
    pub enum Request<'a>: usize {
        // <include/asm-i386/socket.h>
        FIOSETOWN(Option<Addr<'a, libc::c_int>>) = 0x00008901,
        SIOCSPGRP(Option<Addr<'a, libc::c_int>>) = 0x00008902,
        FIOGETOWN(Option<AddrMut<'a, libc::c_int>>) = 0x00008903,
        SIOCGPGRP(Option<AddrMut<'a, libc::c_int>>) = 0x00008904,
        SIOCATMAR(Option<AddrMut<'a, libc::c_int>>) = 0x00008905,
        SIOCGSTAMP(Option<AddrMut<'a, libc::timeval>>) = 0x00008906,
        SIOCGIFINDEX(Option<AddrMut<'a, Ifreq>>) = 0x00008933,
        SIOCETHTOOL(Option<AddrMut<'a, u8>>) = 0x00008946,

        // <include/asm-i386/termios.h>
        TCGETS(Option<AddrMut<'a, Termios>>) = 0x00005401,
        TCSETS(Option<Addr<'a, Termios>>) = 0x00005402,
        TCSETSW(Option<Addr<'a, Termios>>) = 0x00005403,
        TCSETSF(Option<Addr<'a, Termios>>) = 0x00005404,
        TCGETA(Option<AddrMut<'a, Termios>>) = 0x00005405,
        TCSETA(Option<Addr<'a, Termios>>) = 0x00005406,
        TCSETAW(Option<Addr<'a, Termios>>) = 0x00005407,
        TCSETAF(Option<Addr<'a, Termios>>) = 0x00005408,
        TCSBRK(libc::c_int) = 0x00005409,
        TCXONC(libc::c_int) = 0x0000540A,
        TCFLSH(libc::c_int) = 0x0000540B,
        TIOCEXCL = 0x0000540C,
        TIOCNXCL = 0x0000540D,
        TIOCSCTTY(libc::c_int) = 0x0000540E,
        TIOCGPGRP(Option<AddrMut<'a, libc::pid_t>>) = 0x0000540F,
        TIOCSPGRP(Option<Addr<'a, libc::pid_t>>) = 0x00005410,
        TIOCOUTQ(Option<AddrMut<'a, libc::c_int>>) = 0x00005411,
        TIOCSTI(Option<Addr<'a, libc::c_char>>) = 0x00005412,
        TIOCGWINSZ(Option<AddrMut<'a, Winsize>>) = 0x00005413,
        TIOCSWINSZ(Option<Addr<'a, Winsize>>) = 0x00005414,
        TIOCMGET(Option<AddrMut<'a, libc::c_int>>) = 0x00005415,
        TIOCMBIS(Option<Addr<'a, libc::c_int>>) = 0x00005416,
        TIOCMBIC(Option<Addr<'a, libc::c_int>>) = 0x00005417,
        TIOCMSET(Option<Addr<'a, libc::c_int>>) = 0x00005418,
        TIOCGSOFTCAR(Option<AddrMut<'a, libc::c_int>>) = 0x00005419,
        TIOCSSOFTCAR(Option<Addr<'a, libc::c_int>>) = 0x0000541A,
        FIONREAD(Option<AddrMut<'a, libc::c_int>>) = 0x0000541B,
        // Duplicate of FIONREAD; can't properly match the ID.
        #[cfg(any())]
        TIOCINQ(Option<AddrMut<'a, libc::c_int>>) = 0x0000541B,
        TIOCLINUX(Option<Addr<'a, libc::c_char>>) = 0x0000541C,
        TIOCCONS = 0x0000541D,
        // Disabled because `libc::serial_struct` isn't defined.
        #[cfg(any())]
        TIOCGSERIAL(Option<AddrMut<'a, libc::serial_struct>>) = 0x0000541E,
        // Disabled because `libc::serial_struct` isn't defined.
        #[cfg(any())]
        TIOCSSERIAL(Option<Addr<'a, libc::serial_struct>>) = 0x0000541F,
        TIOCPKT(Option<Addr<'a, libc::c_int>>) = 0x00005420,
        FIONBIO(Option<Addr<'a, libc::c_int>>) = 0x00005421,
        TIOCNOTTY = 0x00005422,
        TIOCSETD(Option<Addr<'a, libc::c_int>>) = 0x00005423,
        TIOCGETD(Option<AddrMut<'a, libc::c_int>>) = 0x00005424,
        TCSBRKP(libc::c_int) = 0x00005425,
        // Disabled because `libc::tty_struct` isn't defined.
        #[cfg(any())]
        TIOCTTYGSTRUCT(Option<AddrMut<'a, libc::tty_struct>>) = 0x00005426,
        TIOCGPTPEER(libc::c_int) = 0x00005441,
        FIONCLEX = 0x00005450,
        FIOCLEX = 0x00005451,
        FIOASYNC(Option<Addr<'a, libc::c_int>>) = 0x00005452,
        TIOCSERCONFIG = 0x00005453,
        TIOCSERGWILD(Option<AddrMut<'a, libc::c_int>>) = 0x00005454,
        TIOCSERSWILD(Option<Addr<'a, libc::c_int>>) = 0x00005455,
        TIOCGLCKTRMIOS(Option<AddrMut<'a, Termios>>) = 0x00005456,
        TIOCSLCKTRMIOS(Option<Addr<'a, Termios>>) = 0x00005457,
        // Disabled because `libc::async_struct` isn't defined.
        #[cfg(any())]
        TIOCSERGSTRUCT(Option<AddrMut<'a, libc::async_struct>>) = 0x00005458,
        TIOCSERGETLSR(Option<AddrMut<'a, libc::c_int>>) = 0x00005459,

        FICLONE(libc::c_int) = 0x40049409,
        FICLONERANGE(Option<Addr<'a, libc::file_clone_range>>) = 0x4020940D,
    }
}

const IFNAMSIZ: usize = 16;
const ETHTOOL_GSET: u32 = 0x0000_0001;
const ETHTOOL_GLINK: u32 = 0x0000_000a;
const IOC_SIZE_SHIFT: usize = 16;
const IOC_SIZE_MASK: usize = (1 << 14) - 1;
const IOC_DIR_SHIFT: usize = 30;
const IOC_READ: usize = 2;

// The sizes Linux copies out for the legacy requests below, whose numbers
// predate the `_IOC` direction and size fields and so encode neither.
/// `int`, copied by `FIOGETOWN`, `TIOCOUTQ` and the other `int` getters.
const INT_OUTPUT_SIZE: usize = core::mem::size_of::<libc::c_int>();
/// `struct termio` (`include/uapi/asm-generic/termios.h`): four `unsigned
/// short` flags, `c_line` and `c_cc[8]`, padded to its 2-byte alignment.
/// `TCGETA` copies this, not the larger `struct termios` its variant names.
const TERMIO_OUTPUT_SIZE: usize = 18;
/// The kernel's `struct termios`, which [`Termios`] matches.
const TERMIOS_OUTPUT_SIZE: usize = core::mem::size_of::<Termios>();
/// Seconds and microseconds as two 64-bit fields, the 64-bit layout of
/// `struct timeval`, which the legacy `SIOCGSTAMP` copies.
const OLD_TIMEVAL_OUTPUT_SIZE: usize = core::mem::size_of::<libc::timeval>();

/// Reads exactly `size` bytes through a request's output pointer.
fn read_fixed_output<M: MemoryAccess, T>(
    m: &M,
    pointer: Option<AddrMut<T>>,
    size: usize,
) -> Result<Output, Errno> {
    let addr = pointer.ok_or(Errno::EFAULT)?.cast::<u8>();
    let mut bytes = vec![0; size];
    m.read_exact(addr, &mut bytes)?;
    Ok(Output::Other(bytes))
}

/// Writes a fixed-size output back through a request's output pointer.
fn write_fixed_output<M: MemoryAccess, T>(
    m: &mut M,
    pointer: Option<AddrMut<T>>,
    bytes: &[u8],
) -> Result<(), Errno> {
    m.write_exact(pointer.ok_or(Errno::EFAULT)?.cast::<u8>(), bytes)
}

fn ioctl_output_size(request: usize) -> usize {
    if (request >> IOC_DIR_SHIFT) & IOC_READ != 0 {
        (request >> IOC_SIZE_SHIFT) & IOC_SIZE_MASK
    } else {
        0
    }
}

fn ethtool_data<'a, M: MemoryAccess>(
    ifreq: Option<AddrMut<'a, u8>>,
    memory: &M,
) -> Result<AddrMut<'a, u8>, Errno> {
    let ifreq = ifreq.ok_or(Errno::EFAULT)?;
    let data_field = unsafe { ifreq.add(IFNAMSIZ) }.cast::<usize>();
    let data = memory.read_value(data_field)?;
    AddrMut::from_raw(data).ok_or(Errno::EFAULT)
}

impl<'a> Request<'a> {
    /// Returns the direction of the request. That is, whether it is a read or
    /// write request.
    ///
    /// Every request has a direction. It is [`Direction::Read`] exactly when
    /// [`Self::read_output`] captures an output from readable memory.
    pub fn direction(&self) -> Direction {
        // TODO: Generate this with a macro.
        match self {
            Self::TCGETS(_) => Direction::Read,
            Self::TCSETS(_) => Direction::Write,
            Self::TIOCGWINSZ(_) => Direction::Read,
            Self::TIOCSWINSZ(_) => Direction::Write,
            Self::TIOCSPGRP(_) => Direction::Write,
            Self::TIOCGPGRP(_) => Direction::Read,
            Self::FIONREAD(_) => Direction::Read,
            Self::SIOCGIFINDEX(_) => Direction::Read,
            Self::SIOCETHTOOL(_) => Direction::Read,
            // The kernel writes a fixed-size value through the pointer.
            Self::FIOGETOWN(_)
            | Self::SIOCGPGRP(_)
            | Self::SIOCATMAR(_)
            | Self::TIOCOUTQ(_)
            | Self::TIOCMGET(_)
            | Self::TIOCGSOFTCAR(_)
            | Self::TIOCGETD(_)
            | Self::TIOCSERGWILD(_)
            | Self::TIOCSERGETLSR(_)
            | Self::TCGETA(_)
            | Self::TIOCGLCKTRMIOS(_)
            | Self::SIOCGSTAMP(_) => Direction::Read,
            // The kernel only reads through the pointer.
            Self::FIOSETOWN(_)
            | Self::SIOCSPGRP(_)
            | Self::TCSETSW(_)
            | Self::TCSETSF(_)
            | Self::TCSETA(_)
            | Self::TCSETAW(_)
            | Self::TCSETAF(_)
            | Self::TIOCSTI(_)
            | Self::TIOCMBIS(_)
            | Self::TIOCMBIC(_)
            | Self::TIOCMSET(_)
            | Self::TIOCSSOFTCAR(_)
            | Self::TIOCPKT(_)
            | Self::FIONBIO(_)
            | Self::TIOCSETD(_)
            | Self::FIOASYNC(_)
            | Self::TIOCSERSWILD(_)
            | Self::TIOCSLCKTRMIOS(_)
            | Self::FICLONERANGE(_) => Direction::Write,
            // A value argument, or none.
            Self::TCSBRK(_)
            | Self::TCXONC(_)
            | Self::TCFLSH(_)
            | Self::TIOCEXCL
            | Self::TIOCNXCL
            | Self::TIOCSCTTY(_)
            | Self::TIOCCONS
            | Self::TIOCNOTTY
            | Self::TCSBRKP(_)
            | Self::TIOCGPTPEER(_)
            | Self::FIONCLEX
            | Self::FIOCLEX
            | Self::TIOCSERCONFIG
            | Self::FICLONE(_) => Direction::None,
            // Opaque: see `read_output`.
            Self::TIOCLINUX(_) => Direction::None,
            Self::Other(request, _) => {
                if ioctl_output_size(*request) > 0 {
                    Direction::Read
                } else {
                    Direction::None
                }
            }
        }
    }

    /// Reads the output associated with this request. If the request has no
    /// outputs, returns `Ok(None)` without reading memory.
    ///
    /// Typed requests whose output is a fixed-size value without its own
    /// [`Output`] variant capture exactly that many bytes as
    /// [`Output::Other`]. Unknown requests with an encoded read size capture
    /// that many bytes. Legacy requests without an encoded size, and
    /// `TIOCLINUX`, are treated as opaque.
    ///
    /// # Panics
    ///
    /// Panics on a `SIOCETHTOOL` request whose nested command is neither
    /// `ETHTOOL_GSET` nor `ETHTOOL_GLINK`.
    pub fn read_output<M: MemoryAccess>(&self, m: &M) -> Result<Option<Output>, Errno> {
        // TODO: Generate this with a macro.
        Ok(Some(match self {
            Self::TCGETS(p) => Output::TCGETS(m.read_value(p.ok_or(Errno::EFAULT)?)?),
            Self::TCSETS(_) => return Ok(None),
            Self::TIOCGWINSZ(p) => Output::TIOCGWINSZ(m.read_value(p.ok_or(Errno::EFAULT)?)?),
            Self::TIOCSWINSZ(_) => return Ok(None),
            Self::TIOCGPGRP(p) => Output::TIOCGPGRP(m.read_value(p.ok_or(Errno::EFAULT)?)?),
            Self::TIOCSPGRP(_) => return Ok(None),
            Self::FIONREAD(p) => Output::FIONREAD(m.read_value(p.ok_or(Errno::EFAULT)?)?),
            Self::SIOCGIFINDEX(p) => Output::SIOCGIFINDEX(m.read_value(p.ok_or(Errno::EFAULT)?)?),
            Self::SIOCETHTOOL(ifreq) => {
                let data = ethtool_data(*ifreq, m)?;
                let command: u32 = m.read_value(data.cast())?;
                let output = match command {
                    ETHTOOL_GSET => EthtoolOutput::Gset(m.read_value(data.cast())?),
                    ETHTOOL_GLINK => EthtoolOutput::Glink(m.read_value(data.cast())?),
                    other => panic!("ioctl: unsupported SIOCETHTOOL command: {other:#x}"),
                };
                Output::SIOCETHTOOL(output)
            }
            Self::FIOGETOWN(p)
            | Self::SIOCGPGRP(p)
            | Self::SIOCATMAR(p)
            | Self::TIOCOUTQ(p)
            | Self::TIOCMGET(p)
            | Self::TIOCGSOFTCAR(p)
            | Self::TIOCGETD(p)
            | Self::TIOCSERGWILD(p)
            | Self::TIOCSERGETLSR(p) => read_fixed_output(m, *p, INT_OUTPUT_SIZE)?,
            Self::TCGETA(p) => read_fixed_output(m, *p, TERMIO_OUTPUT_SIZE)?,
            Self::TIOCGLCKTRMIOS(p) => read_fixed_output(m, *p, TERMIOS_OUTPUT_SIZE)?,
            Self::SIOCGSTAMP(p) => read_fixed_output(m, *p, OLD_TIMEVAL_OUTPUT_SIZE)?,
            Self::FIOSETOWN(_)
            | Self::SIOCSPGRP(_)
            | Self::TCSETSW(_)
            | Self::TCSETSF(_)
            | Self::TCSETA(_)
            | Self::TCSETAW(_)
            | Self::TCSETAF(_)
            | Self::TCSBRK(_)
            | Self::TCXONC(_)
            | Self::TCFLSH(_)
            | Self::TIOCEXCL
            | Self::TIOCNXCL
            | Self::TIOCSCTTY(_)
            | Self::TIOCSTI(_)
            | Self::TIOCMBIS(_)
            | Self::TIOCMBIC(_)
            | Self::TIOCMSET(_)
            | Self::TIOCSSOFTCAR(_)
            | Self::TIOCCONS
            | Self::TIOCPKT(_)
            | Self::FIONBIO(_)
            | Self::TIOCNOTTY
            | Self::TIOCSETD(_)
            | Self::TCSBRKP(_)
            | Self::TIOCGPTPEER(_)
            | Self::FIONCLEX
            | Self::FIOCLEX
            | Self::FIOASYNC(_)
            | Self::TIOCSERCONFIG
            | Self::TIOCSERSWILD(_)
            | Self::TIOCSLCKTRMIOS(_)
            | Self::FICLONE(_)
            | Self::FICLONERANGE(_) => return Ok(None),
            // `TIOCLINUX`'s effect depends on its subcommand, the byte its
            // argument points to (`tioclinux()` in drivers/tty/vt/vt.c): some
            // subcommands store one byte back through the argument, some
            // return a value, and the rest store nothing through it. One
            // model cannot be right for all of them, so it is opaque: nothing
            // is read, and `write_output` stores nothing.
            Self::TIOCLINUX(_) => return Ok(None),
            Self::Other(request, arg) => {
                let size = ioctl_output_size(*request);
                if size == 0 {
                    return Ok(None);
                }
                let addr = Addr::<u8>::from_raw(*arg).ok_or(Errno::EFAULT)?;
                let mut bytes = vec![0; size];
                m.read_exact(addr, &mut bytes)?;
                Output::Other(bytes)
            }
        }))
    }

    /// Writes the output associated with this request to the provided address
    /// (if any). If the request has no outputs, returns `Ok(())` without
    /// writing memory.
    ///
    /// # Panics
    ///
    /// Panics if the request has an output and `output` is not one that
    /// [`Self::read_output`] produces for it: another variant, or an
    /// [`Output::Other`] of another length.
    pub fn write_output<M: MemoryAccess>(&self, m: &mut M, output: &Output) -> Result<(), Errno> {
        match (self, output) {
            (Self::TCGETS(p), Output::TCGETS(output)) => {
                m.write_value(p.ok_or(Errno::EFAULT)?, output)
            }
            (Self::TCSETS(_), _) => Ok(()),
            (Self::TIOCGWINSZ(p), Output::TIOCGWINSZ(output)) => {
                m.write_value(p.ok_or(Errno::EFAULT)?, output)
            }
            (Self::TIOCSWINSZ(_), _) => Ok(()),
            (Self::TIOCGPGRP(p), Output::TIOCGPGRP(output)) => {
                m.write_value(p.ok_or(Errno::EFAULT)?, output)
            }
            (Self::TIOCSPGRP(_), _) => Ok(()),
            (Self::FIONREAD(p), Output::FIONREAD(output)) => {
                m.write_value(p.ok_or(Errno::EFAULT)?, output)
            }
            (Self::SIOCGIFINDEX(p), Output::SIOCGIFINDEX(output)) => {
                m.write_value(p.ok_or(Errno::EFAULT)?, output)
            }
            (Self::SIOCETHTOOL(ifreq), Output::SIOCETHTOOL(output)) => {
                let data = ethtool_data(*ifreq, m)?;
                match output {
                    EthtoolOutput::Gset(output) => m.write_value(data.cast(), output),
                    EthtoolOutput::Glink(output) => m.write_value(data.cast(), output),
                }
            }
            (
                Self::FIOGETOWN(p)
                | Self::SIOCGPGRP(p)
                | Self::SIOCATMAR(p)
                | Self::TIOCOUTQ(p)
                | Self::TIOCMGET(p)
                | Self::TIOCGSOFTCAR(p)
                | Self::TIOCGETD(p)
                | Self::TIOCSERGWILD(p)
                | Self::TIOCSERGETLSR(p),
                Output::Other(bytes),
            ) if bytes.len() == INT_OUTPUT_SIZE => write_fixed_output(m, *p, bytes),
            (Self::TCGETA(p), Output::Other(bytes)) if bytes.len() == TERMIO_OUTPUT_SIZE => {
                write_fixed_output(m, *p, bytes)
            }
            (Self::TIOCGLCKTRMIOS(p), Output::Other(bytes))
                if bytes.len() == TERMIOS_OUTPUT_SIZE =>
            {
                write_fixed_output(m, *p, bytes)
            }
            (Self::SIOCGSTAMP(p), Output::Other(bytes))
                if bytes.len() == OLD_TIMEVAL_OUTPUT_SIZE =>
            {
                write_fixed_output(m, *p, bytes)
            }
            (
                Self::FIOSETOWN(_)
                | Self::SIOCSPGRP(_)
                | Self::TCSETSW(_)
                | Self::TCSETSF(_)
                | Self::TCSETA(_)
                | Self::TCSETAW(_)
                | Self::TCSETAF(_)
                | Self::TCSBRK(_)
                | Self::TCXONC(_)
                | Self::TCFLSH(_)
                | Self::TIOCEXCL
                | Self::TIOCNXCL
                | Self::TIOCSCTTY(_)
                | Self::TIOCSTI(_)
                | Self::TIOCMBIS(_)
                | Self::TIOCMBIC(_)
                | Self::TIOCMSET(_)
                | Self::TIOCSSOFTCAR(_)
                | Self::TIOCLINUX(_)
                | Self::TIOCCONS
                | Self::TIOCPKT(_)
                | Self::FIONBIO(_)
                | Self::TIOCNOTTY
                | Self::TIOCSETD(_)
                | Self::TCSBRKP(_)
                | Self::TIOCGPTPEER(_)
                | Self::FIONCLEX
                | Self::FIOCLEX
                | Self::FIOASYNC(_)
                | Self::TIOCSERCONFIG
                | Self::TIOCSERSWILD(_)
                | Self::TIOCSLCKTRMIOS(_)
                | Self::FICLONE(_)
                | Self::FICLONERANGE(_),
                _,
            ) => Ok(()),
            (Self::Other(request, arg), Output::Other(output))
                if output.len() == ioctl_output_size(*request) =>
            {
                let addr = AddrMut::<u8>::from_raw(*arg).ok_or(Errno::EFAULT)?;
                m.write_exact(addr, output)
            }
            // Every request with an output, given an output its arm above
            // does not accept. They are listed rather than matched by a
            // wildcard, so a request added without an arm here fails to
            // compile, as it does in `direction` and `read_output`.
            (
                Self::TCGETS(_)
                | Self::TIOCGWINSZ(_)
                | Self::TIOCGPGRP(_)
                | Self::FIONREAD(_)
                | Self::SIOCGIFINDEX(_)
                | Self::SIOCETHTOOL(_)
                | Self::FIOGETOWN(_)
                | Self::SIOCGPGRP(_)
                | Self::SIOCATMAR(_)
                | Self::TIOCOUTQ(_)
                | Self::TIOCMGET(_)
                | Self::TIOCGSOFTCAR(_)
                | Self::TIOCGETD(_)
                | Self::TIOCSERGWILD(_)
                | Self::TIOCSERGETLSR(_)
                | Self::TCGETA(_)
                | Self::TIOCGLCKTRMIOS(_)
                | Self::SIOCGSTAMP(_)
                | Self::Other(..),
                output,
            ) => {
                panic!(
                    "ioctl: unsupported request/output pair: {:?}, {:?}",
                    self, output
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalMemory;

    #[test]
    fn unknown_request_is_opaque_and_round_trips() {
        let request = Request::from_raw(0xdead, 0x1234);

        assert_eq!(request, Request::Other(0xdead, 0x1234));
        assert_eq!(request.direction(), Direction::None);
        assert_eq!(request.read_output(&LocalMemory::new()), Ok(None));
        assert_eq!(request.into_raw(), (0xdead, 0x1234));
    }

    #[test]
    fn unknown_read_request_captures_encoded_output_size() {
        const UNKNOWN_READ: usize =
            (IOC_READ << IOC_DIR_SHIFT) | (4 << IOC_SIZE_SHIFT) | (b'x' as usize) << 8 | 0x42;
        let recorded = 0x1234_5678_u32;
        let request = Request::from_raw(UNKNOWN_READ, &recorded as *const u32 as usize);

        assert_eq!(request.direction(), Direction::Read);
        assert_eq!(
            request.read_output(&LocalMemory::new()),
            Ok(Some(Output::Other(recorded.to_ne_bytes().to_vec())))
        );

        let mut replayed = 0_u32;
        let request = Request::from_raw(UNKNOWN_READ, &mut replayed as *mut u32 as usize);
        request
            .write_output(
                &mut LocalMemory::new(),
                &Output::Other(recorded.to_ne_bytes().to_vec()),
            )
            .unwrap();
        assert_eq!(replayed, recorded);
    }

    #[test]
    fn siocgifindex_reads_and_writes_ifreq_output() {
        let mut recorded = Ifreq::default();
        recorded.name[..5].copy_from_slice(b"eth0\0");
        recorded.data[..4].copy_from_slice(&7_i32.to_ne_bytes());
        let request = Request::from_raw(0x8933, &mut recorded as *mut Ifreq as usize);

        assert_eq!(request.direction(), Direction::Read);
        assert_eq!(
            request.read_output(&LocalMemory::new()),
            Ok(Some(Output::SIOCGIFINDEX(recorded)))
        );

        let mut replayed = Ifreq::default();
        let request = Request::from_raw(0x8933, &mut replayed as *mut Ifreq as usize);
        request
            .write_output(&mut LocalMemory::new(), &Output::SIOCGIFINDEX(recorded))
            .unwrap();
        assert_eq!(replayed, recorded);
    }

    #[test]
    fn ethtool_gset_reads_and_writes_nested_output() {
        let recorded = EthtoolCmd {
            cmd: ETHTOOL_GSET,
            supported: 0x8000_6440,
            advertising: 0x8000_0440,
            speed: 1000,
            duplex: 1,
            port: 5,
            ..EthtoolCmd::default()
        };
        let mut recorded_ifreq = [0usize; 5];
        recorded_ifreq[2] = &recorded as *const EthtoolCmd as usize;
        let request = Request::from_raw(0x8946, recorded_ifreq.as_mut_ptr() as usize);

        assert_eq!(std::mem::size_of::<Ifreq>(), 40);
        assert_eq!(std::mem::size_of::<EthtoolCmd>(), 44);
        assert_eq!(request.direction(), Direction::Read);
        assert_eq!(
            request.read_output(&LocalMemory::new()),
            Ok(Some(Output::SIOCETHTOOL(EthtoolOutput::Gset(recorded))))
        );

        let mut replayed = EthtoolCmd {
            cmd: ETHTOOL_GSET,
            ..EthtoolCmd::default()
        };
        let mut replayed_ifreq = [0usize; 5];
        replayed_ifreq[2] = &mut replayed as *mut EthtoolCmd as usize;
        let request = Request::from_raw(0x8946, replayed_ifreq.as_mut_ptr() as usize);

        request
            .write_output(
                &mut LocalMemory::new(),
                &Output::SIOCETHTOOL(EthtoolOutput::Gset(recorded)),
            )
            .unwrap();
        assert_eq!(replayed, recorded);
    }

    #[test]
    fn ethtool_glink_reads_and_writes_nested_output() {
        let recorded = EthtoolValue {
            cmd: ETHTOOL_GLINK,
            data: 1,
        };
        let mut recorded_ifreq = [0usize; 5];
        recorded_ifreq[2] = &recorded as *const EthtoolValue as usize;
        let request = Request::from_raw(0x8946, recorded_ifreq.as_mut_ptr() as usize);

        assert_eq!(std::mem::size_of::<EthtoolValue>(), 8);
        assert_eq!(
            request.read_output(&LocalMemory::new()),
            Ok(Some(Output::SIOCETHTOOL(EthtoolOutput::Glink(recorded))))
        );

        let mut replayed = EthtoolValue {
            cmd: ETHTOOL_GLINK,
            data: 0,
        };
        let mut replayed_ifreq = [0usize; 5];
        replayed_ifreq[2] = &mut replayed as *mut EthtoolValue as usize;
        let request = Request::from_raw(0x8946, replayed_ifreq.as_mut_ptr() as usize);
        request
            .write_output(
                &mut LocalMemory::new(),
                &Output::SIOCETHTOOL(EthtoolOutput::Glink(recorded)),
            )
            .unwrap();
        assert_eq!(replayed, recorded);
    }

    /// How Linux uses the argument of a typed request.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Class {
        /// A pointer the kernel only reads.
        Input,
        /// A value argument, or none.
        Value,
        /// A pointer through which the kernel writes exactly this many bytes.
        Output(usize),
        /// An effect that depends on a subcommand, so it is not decoded.
        Opaque,
    }

    /// The 46 typed requests without their own `Output` variant, by number.
    const TYPED_WITHOUT_OUTPUT_VARIANT: [(usize, Class); 46] = [
        (0x8901, Class::Input),      // FIOSETOWN
        (0x8902, Class::Input),      // SIOCSPGRP
        (0x8903, Class::Output(4)),  // FIOGETOWN: int
        (0x8904, Class::Output(4)),  // SIOCGPGRP: int
        (0x8905, Class::Output(4)),  // SIOCATMARK: int
        (0x8906, Class::Output(16)), // SIOCGSTAMP: 64-bit struct timeval
        (0x5403, Class::Input),      // TCSETSW
        (0x5404, Class::Input),      // TCSETSF
        (0x5405, Class::Output(18)), // TCGETA: struct termio
        (0x5406, Class::Input),      // TCSETA
        (0x5407, Class::Input),      // TCSETAW
        (0x5408, Class::Input),      // TCSETAF
        (0x5409, Class::Value),      // TCSBRK
        (0x540A, Class::Value),      // TCXONC
        (0x540B, Class::Value),      // TCFLSH
        (0x540C, Class::Value),      // TIOCEXCL
        (0x540D, Class::Value),      // TIOCNXCL
        (0x540E, Class::Value),      // TIOCSCTTY
        (0x5411, Class::Output(4)),  // TIOCOUTQ: int
        (0x5412, Class::Input),      // TIOCSTI
        (0x5415, Class::Output(4)),  // TIOCMGET: int
        (0x5416, Class::Input),      // TIOCMBIS
        (0x5417, Class::Input),      // TIOCMBIC
        (0x5418, Class::Input),      // TIOCMSET
        (0x5419, Class::Output(4)),  // TIOCGSOFTCAR: int
        (0x541A, Class::Input),      // TIOCSSOFTCAR
        (0x541C, Class::Opaque),     // TIOCLINUX
        (0x541D, Class::Value),      // TIOCCONS
        (0x5420, Class::Input),      // TIOCPKT
        (0x5421, Class::Input),      // FIONBIO
        (0x5422, Class::Value),      // TIOCNOTTY
        (0x5423, Class::Input),      // TIOCSETD
        (0x5424, Class::Output(4)),  // TIOCGETD: int
        (0x5425, Class::Value),      // TCSBRKP
        (0x5441, Class::Value),      // TIOCGPTPEER
        (0x5450, Class::Value),      // FIONCLEX
        (0x5451, Class::Value),      // FIOCLEX
        (0x5452, Class::Input),      // FIOASYNC
        (0x5453, Class::Value),      // TIOCSERCONFIG
        (0x5454, Class::Output(4)),  // TIOCSERGWILD: int, the historical ABI
        (0x5455, Class::Input),      // TIOCSERSWILD: the historical ABI
        (0x5456, Class::Output(36)), // TIOCGLCKTRMIOS: struct termios
        (0x5457, Class::Input),      // TIOCSLCKTRMIOS
        (0x5459, Class::Output(4)),  // TIOCSERGETLSR: int
        (0x40049409, Class::Value),  // FICLONE
        (0x4020940D, Class::Input),  // FICLONERANGE
    ];

    /// The 9 typed requests with their own `Output` variant: TCGETS, TCSETS,
    /// TIOCGPGRP, TIOCSPGRP, TIOCGWINSZ, TIOCSWINSZ, FIONREAD, SIOCGIFINDEX
    /// and SIOCETHTOOL.
    const TYPED_WITH_OUTPUT_VARIANT: [usize; 9] = [
        0x5401, 0x5402, 0x540F, 0x5410, 0x5413, 0x5414, 0x541B, 0x8933, 0x8946,
    ];

    /// One memory access: its address and length.
    type Access = (usize, usize);

    /// Memory in this process that records every access it performs.
    #[derive(Default)]
    struct Spy {
        memory: LocalMemory,
        reads: std::cell::RefCell<Vec<Access>>,
        writes: Vec<Access>,
    }

    impl MemoryAccess for Spy {
        fn read_vectored(
            &self,
            read_from: &[std::io::IoSlice],
            write_to: &mut [std::io::IoSliceMut],
        ) -> Result<usize, Errno> {
            let accesses = read_from.iter().map(|s| (s.as_ptr() as usize, s.len()));
            self.reads.borrow_mut().extend(accesses);
            self.memory.read_vectored(read_from, write_to)
        }

        fn write_vectored(
            &mut self,
            read_from: &[std::io::IoSlice],
            write_to: &mut [std::io::IoSliceMut],
        ) -> Result<usize, Errno> {
            let accesses = write_to.iter().map(|s| (s.as_ptr() as usize, s.len()));
            self.writes.extend(accesses);
            self.memory.write_vectored(read_from, write_to)
        }

        fn read<'a, A>(&self, addr: A, buf: &mut [u8]) -> Result<usize, Errno>
        where
            A: Into<Addr<'a, u8>>,
        {
            let addr = addr.into();
            self.reads.borrow_mut().push((addr.as_raw(), buf.len()));
            self.memory.read(addr, buf)
        }

        fn write(&mut self, addr: AddrMut<u8>, buf: &[u8]) -> Result<usize, Errno> {
            self.writes.push((addr.as_raw(), buf.len()));
            self.memory.write(addr, buf)
        }
    }

    /// Asserts that `accesses` cover exactly `len` bytes from `start`, each
    /// byte once.
    fn assert_covers(accesses: &[Access], start: usize, len: usize, what: &str) {
        let mut sorted = accesses.to_vec();
        sorted.sort_unstable();
        let mut next = start;
        for (addr, n) in sorted {
            assert_eq!(addr, next, "{what}: accesses {accesses:x?}");
            next += n;
        }
        assert_eq!(next, start + len, "{what}: accesses {accesses:x?}");
    }

    const CANARY: u8 = 0xc5;
    /// Canary bytes before the output region; at least as many follow it.
    const PREFIX: usize = 16;

    /// A buffer of canaries, with room for the largest output plus a trailing
    /// run of canaries.
    #[repr(C, align(8))]
    struct Canaried([u8; 96]);

    impl Canaried {
        fn new() -> Self {
            Self([CANARY; 96])
        }

        fn region(&mut self) -> usize {
            self.0[PREFIX..].as_mut_ptr() as usize
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| 0x10 + i as u8).collect()
    }

    #[test]
    fn typed_tables_cover_every_typed_request() {
        let listed: std::collections::BTreeSet<usize> = TYPED_WITHOUT_OUTPUT_VARIANT
            .iter()
            .map(|&(number, _)| number)
            .chain(TYPED_WITH_OUTPUT_VARIANT)
            .collect();
        assert_eq!(listed.len(), 55, "a number is listed twice");

        let typed: std::collections::BTreeSet<usize> = (0x5400..=0x54ff)
            .chain(0x8900..=0x89ff)
            .chain(listed.iter().copied())
            .filter(|&number| !matches!(Request::from_raw(number, 0), Request::Other(..)))
            .collect();
        assert_eq!(typed, listed);

        // Every variant's number, generated from the enum's own entries, so a
        // variant added with a number outside the ranges scanned above has to
        // be listed too.
        let declared: std::collections::BTreeSet<usize> =
            Request::TYPED_COMMANDS.iter().copied().collect();
        assert_eq!(
            declared.len(),
            Request::TYPED_COMMANDS.len(),
            "two variants share a number"
        );
        assert_eq!(
            declared,
            listed,
            "declared but not listed: {:x?}; listed but not declared: {:x?}",
            declared.difference(&listed).collect::<Vec<_>>(),
            listed.difference(&declared).collect::<Vec<_>>()
        );
    }

    #[test]
    fn every_typed_request_without_an_output_variant_is_decoded() {
        for (number, class) in TYPED_WITHOUT_OUTPUT_VARIANT {
            let mut recorded = Canaried::new();
            let region = recorded.region();
            recorded.0[PREFIX..PREFIX + 36].copy_from_slice(&pattern(36));
            let request = Request::from_raw(number, region);
            let name = format!("{request:?}");

            let expected = match class {
                Class::Output(_) => Direction::Read,
                Class::Input => Direction::Write,
                Class::Value | Class::Opaque => Direction::None,
            };
            assert_eq!(request.direction(), expected, "{name}");

            let spy = Spy::default();
            let output = request.read_output(&spy);
            let mut replayed = Canaried::new();
            let target = Request::from_raw(number, replayed.region());
            let mut spy_writes = Spy::default();
            match class {
                Class::Output(len) => {
                    assert_eq!(output, Ok(Some(Output::Other(pattern(len)))), "{name}");
                    assert_covers(&spy.reads.borrow(), region, len, &name);

                    target
                        .write_output(&mut spy_writes, &Output::Other(pattern(len)))
                        .unwrap();
                    assert_covers(&spy_writes.writes, replayed.region(), len, &name);
                    assert_eq!(replayed.0[PREFIX..PREFIX + len], pattern(len), "{name}");
                    assert!(replayed.0[..PREFIX].iter().all(|&b| b == CANARY), "{name}");
                    assert!(
                        replayed.0[PREFIX + len..].iter().all(|&b| b == CANARY),
                        "{name}"
                    );
                }
                Class::Input | Class::Value | Class::Opaque => {
                    assert_eq!(output, Ok(None), "{name}");
                    let reads = spy.reads.borrow();
                    assert!(reads.is_empty(), "{name} read memory: {reads:x?}");

                    for unrelated in [
                        Output::Other(Vec::new()),
                        Output::Other(pattern(4)),
                        Output::FIONREAD(7),
                    ] {
                        assert_eq!(target.write_output(&mut spy_writes, &unrelated), Ok(()));
                    }
                    let writes = &spy_writes.writes;
                    assert!(writes.is_empty(), "{name} stored: {writes:x?}");
                    assert!(replayed.0.iter().all(|&b| b == CANARY), "{name}");
                }
            }
        }
    }

    #[test]
    fn tioclinux_reads_and_stores_nothing_through_its_subcommand_byte() {
        // TIOCL_GETSHIFTSTATE, a subcommand that stores a byte back in Linux.
        const TIOCL_GETSHIFTSTATE: u8 = 6;
        let mut argument = [TIOCL_GETSHIFTSTATE, CANARY, CANARY, CANARY];
        let request = Request::from_raw(0x541C, argument.as_mut_ptr() as usize);
        assert!(matches!(request, Request::TIOCLINUX(Some(_))));

        let spy = Spy::default();
        assert_eq!(request.direction(), Direction::None);
        assert_eq!(request.read_output(&spy), Ok(None));
        assert!(spy.reads.borrow().is_empty());

        let mut spy = Spy::default();
        let stored = Output::Other(vec![0; 1]);
        assert_eq!(request.write_output(&mut spy, &stored), Ok(()));
        assert!(spy.writes.is_empty());
        assert_eq!(argument, [TIOCL_GETSHIFTSTATE, CANARY, CANARY, CANARY]);
    }

    #[test]
    #[should_panic(expected = "unsupported request/output pair")]
    fn tcgeta_refuses_an_output_of_the_termios_size() {
        let mut replayed = Canaried::new();
        let request = Request::from_raw(0x5405, replayed.region());
        let _ = request.write_output(&mut LocalMemory::new(), &Output::Other(pattern(36)));
    }

    /// Every fixed-size output, one byte shorter or one byte longer than its
    /// request's output, is refused and nothing is written. A wrong-length
    /// output means the output does not belong to the request, as after a
    /// desynchronized replay; writing it would store past the end of, for
    /// example, a 4-byte `int`.
    #[test]
    fn every_fixed_size_output_refuses_a_wrong_length_and_writes_nothing() {
        let mut failures = Vec::new();
        let mut cases = 0;
        for (number, class) in TYPED_WITHOUT_OUTPUT_VARIANT {
            let Class::Output(len) = class else {
                continue;
            };
            for wrong_len in [len - 1, len + 1] {
                cases += 1;
                let mut replayed = Canaried::new();
                let request = Request::from_raw(number, replayed.region());
                let name = format!("{request:?} given {wrong_len} bytes");
                let mut spy = Spy::default();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    request.write_output(&mut spy, &Output::Other(pattern(wrong_len)))
                }));
                match outcome {
                    Ok(result) => failures.push(format!("{name}: accepted, {result:?}")),
                    Err(payload) => {
                        let message = payload
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| payload.downcast_ref::<&str>().copied())
                            .unwrap_or_default();
                        if !message.contains("unsupported request/output pair") {
                            failures.push(format!("{name}: panicked with {message:?}"));
                        }
                    }
                }
                if !spy.writes.is_empty() {
                    failures.push(format!("{name}: stored {:x?}", spy.writes));
                }
                if !replayed.0.iter().all(|&b| b == CANARY) {
                    failures.push(format!("{name}: memory changed"));
                }
            }
        }
        // Two lengths for each of the 12 fixed-size outputs.
        assert_eq!(cases, 24);
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn read_output_is_some_exactly_when_direction_is_read() {
        const IOR_4: usize = (IOC_READ << IOC_DIR_SHIFT) | (4 << IOC_SIZE_SHIFT) | 0x7801;
        const IOW_4: usize = (1 << IOC_DIR_SHIFT) | (4 << IOC_SIZE_SHIFT) | 0x7802;
        const IOWR_8: usize = (3 << IOC_DIR_SHIFT) | (8 << IOC_SIZE_SHIFT) | 0x7803;
        const IO: usize = 0x7804;
        const LEGACY_UNKNOWN: usize = 0xdead;

        // Readable memory for every request, including SIOCETHTOOL's nested
        // command, which its ifreq's data field points to.
        let glink = EthtoolValue {
            cmd: ETHTOOL_GLINK,
            data: 1,
        };
        let mut memory = [0usize; 32];
        memory[2] = &glink as *const EthtoolValue as usize;
        let readable = memory.as_mut_ptr() as usize;

        let numbers = TYPED_WITHOUT_OUTPUT_VARIANT
            .iter()
            .map(|&(number, _)| number)
            .chain(TYPED_WITH_OUTPUT_VARIANT)
            .chain([IOR_4, IOW_4, IOWR_8, IO, LEGACY_UNKNOWN]);
        for number in numbers {
            let request = Request::from_raw(number, readable);
            let name = format!("{request:?}");
            let output = request.read_output(&LocalMemory::new());
            if request.direction() == Direction::Read {
                assert!(matches!(output, Ok(Some(_))), "{name}: {output:?}");
                // A null pointer is an error, never an output or `None`.
                let null = Request::from_raw(number, 0);
                assert_eq!(null.read_output(&LocalMemory::new()), Err(Errno::EFAULT));
            } else {
                assert_eq!(output, Ok(None), "{name}");
            }
        }
    }
}

/// The output after a successful call to `ioctl`. This is only relavent for
/// requests with outputs.
///
/// Note that this is a `union`. The descriminator is the [`Request`] type.
#[allow(missing_docs, non_camel_case_types, clippy::upper_case_acronyms)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Output {
    TCGETS(Termios),
    TIOCGWINSZ(Winsize),
    TIOCGPGRP(libc::pid_t),
    FIONREAD(libc::c_int),
    SIOCGIFINDEX(Ifreq),
    SIOCETHTOOL(EthtoolOutput),
    Other(Vec<u8>),
}

#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EthtoolOutput {
    Gset(EthtoolCmd),
    Glink(EthtoolValue),
}

/// The payload used by simple ethtool get/set commands.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EthtoolValue {
    pub cmd: u32,
    pub data: u32,
}

/// An interface request with a 24-byte data union on x86-64 Linux.
#[allow(missing_docs)]
#[repr(C, align(8))]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Ifreq {
    pub name: [u8; IFNAMSIZ],
    pub data: [u8; 24],
}

/// The payload for the legacy `ETHTOOL_GSET` command.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EthtoolCmd {
    pub cmd: u32,
    pub supported: u32,
    pub advertising: u32,
    pub speed: u16,
    pub duplex: u8,
    pub port: u8,
    pub phy_address: u8,
    pub transceiver: u8,
    pub autoneg: u8,
    pub mdio_support: u8,
    pub maxtxpkt: u32,
    pub maxrxpkt: u32,
    pub speed_hi: u16,
    pub eth_tp_mdix: u8,
    pub eth_tp_mdix_ctrl: u8,
    pub lp_advertising: u32,
    pub reserved: [u32; 2],
}

/// Terminal I/O. This is the same as `termios` as defined in
/// `include/uapi/asm-generic/termbits.h`. Note that this is *different* from the
/// struct exposed by libc (which maps the smaller kernel-defined struct onto a
/// larger libc-defined struct).
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Termios {
    /// input mode flags
    pub c_iflag: libc::tcflag_t,
    /// output mode flags
    pub c_oflag: libc::tcflag_t,
    /// control mode flags
    pub c_cflag: libc::tcflag_t,
    /// local mode flags
    pub c_lflag: libc::tcflag_t,
    /// line discipline
    pub c_line: libc::cc_t,
    /// control characters
    pub c_cc: [libc::cc_t; 19],
}

#[allow(missing_docs)]
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Winsize {
    pub ws_row: libc::c_ushort,
    pub ws_col: libc::c_ushort,
    pub ws_xpixel: libc::c_ushort,
    pub ws_ypixel: libc::c_ushort,
}
