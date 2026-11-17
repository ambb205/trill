//! A process that does jobs says what took it down, on its way down.
//!
//! A plugin that faults raises an exception nobody handles, and the
//! system ends the process with the exception's code. Before it does, it
//! asks one function of the process's own. This is that function: it
//! finds the module the faulting instruction is in (a plugin's file, as a
//! rule), writes one line with the code and the module's path to the
//! standard output, and ends the process itself.
//!
//! It runs in a process whose memory may be in any state, so it uses
//! nothing but the stack: no allocation, no lock, no formatting.
//!
//! Only Windows is asked. Elsewhere a process that goes down is known by
//! how it ended and by the stage it was at.

/// Has the process say what took it down from now on.
pub(super) fn watch() {
    #[cfg(windows)]
    windows::watch();
}

/// Ends the process now, with `code`, taking nothing down.
pub(super) fn end_now(code: u32) -> ! {
    #[cfg(windows)]
    windows::end_now(code);
    #[allow(unreachable_code)]
    std::process::exit(code as i32)
}

#[cfg(windows)]
mod windows {
    use super::super::MARK;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Storage::FileSystem::WriteFile;
    use windows::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    use windows::Win32::System::Diagnostics::Debug::{
        EXCEPTION_POINTERS, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode, SetUnhandledExceptionFilter,
    };
    use windows::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
        GetModuleHandleExW,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
    use windows::core::PCWSTR;

    pub(super) fn watch() {
        // SAFETY: both calls only set what the process does about a fault:
        // no box is put up for one, and the function below is asked.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
            SetUnhandledExceptionFilter(Some(said_on_the_way_down));
        }
    }

    pub(super) fn end_now(code: u32) -> ! {
        // SAFETY: ends this process, which is always there to end.
        unsafe {
            let _ = TerminateProcess(GetCurrentProcess(), code);
        }
        std::process::exit(code as i32)
    }

    /// Room for the line: a path of 520 UTF-16 units is at most 1,560
    /// bytes, or 3,120 written with every byte escaped.
    const ROOM: usize = 4096;

    /// Adds `bytes` to `line` for as long as there is room.
    fn push(line: &mut [u8; ROOM], used: &mut usize, bytes: &[u8]) {
        for byte in bytes {
            if *used < ROOM {
                line[*used] = *byte;
                *used += 1;
            }
        }
    }

    /// Adds `number` in decimal.
    fn push_number(line: &mut [u8; ROOM], used: &mut usize, mut number: u32) {
        let mut digits = [0u8; 10];
        let mut count = 0;
        loop {
            digits[count] = b'0' + (number % 10) as u8;
            count += 1;
            number /= 10;
            if number == 0 {
                break;
            }
        }
        while count > 0 {
            count -= 1;
            push(line, used, &digits[count..=count]);
        }
    }

    /// Adds a path as the inside of a JSON string: what is not a plain
    /// character is written as an escape, by its UTF-16 units.
    fn push_path(line: &mut [u8; ROOM], used: &mut usize, path: &[u16]) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for unit in path {
            match *unit {
                0x20..=0x7e if *unit != u16::from(b'"') && *unit != u16::from(b'\\') => {
                    push(line, used, &[*unit as u8])
                }
                unit => {
                    let digit = |shift: u16| HEX[usize::from((unit >> shift) & 0xf)];
                    push(line, used, &[b'\\', b'u', digit(12), digit(8), digit(4), digit(0)]);
                }
            }
        }
    }

    /// What the system asks when an exception was handled by nobody.
    unsafe extern "system" fn said_on_the_way_down(info: *const EXCEPTION_POINTERS) -> i32 {
        let (mut code, mut address) = (0u32, std::ptr::null_mut::<core::ffi::c_void>());
        // SAFETY: the system hands in a pointer to the exception's record,
        // which it keeps for as long as this runs; both are looked at for
        // null first.
        unsafe {
            if !info.is_null() && !(*info).ExceptionRecord.is_null() {
                let record = &*(*info).ExceptionRecord;
                (code, address) = (record.ExceptionCode.0 as u32, record.ExceptionAddress);
            }
        }
        let mut path = [0u16; 520];
        let mut length = 0usize;
        let mut module = HMODULE::default();
        let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        // SAFETY: with these flags the "name" is an address to find the
        // module of, which need not be valid to be asked about; the
        // module's path is written into the array above, within its length.
        unsafe {
            if !address.is_null() && GetModuleHandleExW(flags, PCWSTR(address as *const u16), &mut module).is_ok() {
                length = (GetModuleFileNameW(Some(module), &mut path) as usize).min(path.len());
            }
        }
        let mut line = [0u8; ROOM];
        let mut used = 0usize;
        push(&mut line, &mut used, b"\n");
        push(&mut line, &mut used, MARK.as_bytes());
        push(&mut line, &mut used, b"{\"Fault\":{\"code\":");
        push_number(&mut line, &mut used, code);
        push(&mut line, &mut used, b",\"module\":\"");
        push_path(&mut line, &mut used, &path[..length]);
        push(&mut line, &mut used, b"\"}}\n");
        // SAFETY: the handle is the process's own standard output, or
        // nothing, in which case nothing is written; the bytes are the
        // array above.
        unsafe {
            if let Ok(out) = GetStdHandle(STD_OUTPUT_HANDLE) {
                let mut wrote = 0u32;
                let _ = WriteFile(out, Some(&line[..used.min(ROOM)]), Some(&mut wrote), None);
            }
            let _ = TerminateProcess(GetCurrentProcess(), code);
        }
        // Not reached: the process has ended. (Zero would let the system
        // go on looking for someone to handle it.)
        0
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_line_is_written_without_anything_but_the_stack() {
            let mut line = [0u8; ROOM];
            let mut used = 0;
            push_number(&mut line, &mut used, 0xC000_0005);
            push(&mut line, &mut used, b" ");
            push_number(&mut line, &mut used, 0);
            push(&mut line, &mut used, b" ");
            let path: Vec<u16> = "C:\\Plugins\\Amp \"X\"\u{e9}.vst3".encode_utf16().collect();
            push_path(&mut line, &mut used, &path);
            let written = std::str::from_utf8(&line[..used]).unwrap();
            assert_eq!(written, "3221225477 0 C:\\u005cPlugins\\u005cAmp \\u0022X\\u0022\\u00e9.vst3");
            // And it is read back as the path it was.
            let read: String = serde_json::from_str(&format!("\"{}\"", written.rsplit(' ').next().unwrap())).unwrap();
            assert_eq!(read, "\"X\"\u{e9}.vst3");
            // What has no room is left out, and nothing is written past the end.
            let mut used = ROOM - 2;
            push(&mut line, &mut used, b"abcdef");
            assert_eq!(used, ROOM);
        }
    }
}
