//! The stream a plugin's state is read from and written to.

use super::com::write_out;
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, PoisonError};
use vst3::Steinberg::IBStream_::IStreamSeekMode_::{kIBSeekCur, kIBSeekEnd, kIBSeekSet};
use vst3::Steinberg::{
    IBStream, IBStreamTrait, ISizeableStream, ISizeableStreamTrait, int32, int64, kInvalidArgument, kResultFalse,
    kResultOk, tresult,
};
use vst3::{Class, ComWrapper};

/// The most a plugin may write into one stream. States are kilobytes,
/// megabytes with samples embedded; anything near this is a plugin gone
/// wrong, and must not take the DAW's memory with it.
const MAX_SIZE: usize = 1 << 30;

/// A stream over bytes in memory: `IBStream`, and `ISizeableStream` for the
/// plugins that ask how much there is before they read.
///
/// The contents are behind a lock, so the object is sound whichever
/// threads a plugin calls it from. State is only ever exchanged on the
/// plugin main thread, where waiting for a lock is harmless.
pub(crate) struct MemoryStream {
    contents: Mutex<Contents>,
}

struct Contents {
    bytes: Vec<u8>,
    /// Where the next read or write happens; never beyond `bytes`.
    position: usize,
}

impl Class for MemoryStream {
    type Interfaces = (IBStream, ISizeableStream);
}

impl MemoryStream {
    /// A stream positioned at the start of `bytes`.
    pub(crate) fn new(bytes: Vec<u8>) -> ComWrapper<Self> {
        ComWrapper::new(Self { contents: Mutex::new(Contents { bytes, position: 0 }) })
    }

    /// Takes everything the stream holds, leaving it empty.
    pub(crate) fn take(&self) -> Vec<u8> {
        let mut contents = self.lock();
        contents.position = 0;
        std::mem::take(&mut contents.bytes)
    }

    fn lock(&self) -> MutexGuard<'_, Contents> {
        self.contents.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl IBStreamTrait for MemoryStream {
    /// Reading at the end is not an error: it succeeds with zero bytes,
    /// which is how plugins that read in blocks find the end.
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: int32, num_bytes_read: *mut int32) -> tresult {
        let Ok(wanted) = usize::try_from(num_bytes) else { return kInvalidArgument };
        if buffer.is_null() {
            return kInvalidArgument;
        }
        let mut contents = self.lock();
        let available = &contents.bytes[contents.position..];
        let count = wanted.min(available.len());
        // SAFETY: the caller provides a buffer of at least `num_bytes`
        // bytes, and `count` is no more than that or than what is left of
        // `bytes`. The buffer is the caller's, so it cannot overlap the
        // stream's own storage.
        unsafe { std::ptr::copy_nonoverlapping(available.as_ptr(), buffer.cast::<u8>(), count) };
        contents.position += count;
        // SAFETY: `num_bytes_read` is null or the caller's `int32`.
        unsafe { write_out(num_bytes_read, count as int32) };
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, num_bytes: int32, num_bytes_written: *mut int32) -> tresult {
        let Ok(count) = usize::try_from(num_bytes) else { return kInvalidArgument };
        if buffer.is_null() {
            return kInvalidArgument;
        }
        let mut contents = self.lock();
        let end = contents.position + count;
        if end > MAX_SIZE {
            return kResultFalse;
        }
        // SAFETY: the caller provides `num_bytes` readable bytes, which
        // stay untouched for the duration of this call.
        let data = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), count) };
        let position = contents.position;
        // Overwrite what is there, then grow by the rest.
        let overlap = count.min(contents.bytes.len() - position);
        contents.bytes[position..position + overlap].copy_from_slice(&data[..overlap]);
        contents.bytes.extend_from_slice(&data[overlap..]);
        contents.position = end;
        // SAFETY: `num_bytes_written` is null or the caller's `int32`.
        unsafe { write_out(num_bytes_written, num_bytes) };
        kResultOk
    }

    /// Positions outside the stream are refused, leaving it where it was.
    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut contents = self.lock();
        let base = match mode {
            mode if mode == kIBSeekSet as int32 => 0,
            mode if mode == kIBSeekCur as int32 => contents.position,
            mode if mode == kIBSeekEnd as int32 => contents.bytes.len(),
            _ => return kInvalidArgument,
        };
        let target = (base as int64).checked_add(pos).and_then(|target| usize::try_from(target).ok());
        let Some(target) = target.filter(|target| *target <= contents.bytes.len()) else { return kResultFalse };
        contents.position = target;
        // SAFETY: `result` is null or the caller's `int64`.
        unsafe { write_out(result, target as int64) };
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `pos` is not null, so it is the caller's `int64`.
        unsafe { write_out(pos, self.lock().position as int64) };
        kResultOk
    }
}

impl ISizeableStreamTrait for MemoryStream {
    unsafe fn getStreamSize(&self, size: *mut int64) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `size` is not null, so it is the caller's `int64`.
        unsafe { write_out(size, self.lock().bytes.len() as int64) };
        kResultOk
    }

    /// Cuts the stream short, or fills it up with zeros.
    unsafe fn setStreamSize(&self, size: int64) -> tresult {
        let Some(size) = usize::try_from(size).ok().filter(|size| *size <= MAX_SIZE) else { return kInvalidArgument };
        let mut contents = self.lock();
        contents.bytes.resize(size, 0);
        contents.position = contents.position.min(size);
        kResultOk
    }
}

// The SDK's enums are a C `int` on Windows and `unsigned` elsewhere, so a
// cast that is needed there is to the same type here.
#[cfg_attr(not(windows), allow(clippy::unnecessary_cast))]
#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;
    use vst3::ComPtr;

    /// The stream the way a plugin sees it: through its COM interfaces.
    fn stream(bytes: &[u8]) -> (ComWrapper<MemoryStream>, ComPtr<IBStream>) {
        let stream = MemoryStream::new(bytes.to_vec());
        let interface = stream.to_com_ptr::<IBStream>().expect("the class implements IBStream");
        (stream, interface)
    }

    fn read(stream: &ComPtr<IBStream>, count: usize) -> (tresult, Vec<u8>) {
        let mut buffer = vec![0xEEu8; count];
        let mut got = -1;
        // SAFETY: `buffer` holds `count` bytes and `got` is an `int32`.
        let result = unsafe { stream.read(buffer.as_mut_ptr().cast(), count as int32, &mut got) };
        buffer.truncate(got.max(0) as usize);
        (result, buffer)
    }

    fn write(stream: &ComPtr<IBStream>, bytes: &[u8]) -> tresult {
        let mut written = -1;
        // SAFETY: `bytes` is readable for its length and `written` is an `int32`.
        let result = unsafe { stream.write(bytes.as_ptr().cast_mut().cast(), bytes.len() as int32, &mut written) };
        assert!(result != kResultOk || written == bytes.len() as int32);
        result
    }

    fn seek(stream: &ComPtr<IBStream>, pos: int64, mode: u32) -> (tresult, int64) {
        let mut at = -1;
        // SAFETY: `at` is an `int64`.
        let result = unsafe { stream.seek(pos, mode as int32, &mut at) };
        (result, at)
    }

    fn tell(stream: &ComPtr<IBStream>) -> int64 {
        let mut at = -1;
        // SAFETY: `at` is an `int64`.
        assert_eq!(unsafe { stream.tell(&mut at) }, kResultOk);
        at
    }

    #[test]
    fn reading_runs_to_the_end_and_then_yields_nothing() {
        let (_owner, stream) = stream(b"hello world");
        assert_eq!(read(&stream, 5), (kResultOk, b"hello".to_vec()));
        assert_eq!(read(&stream, 100), (kResultOk, b" world".to_vec()));
        assert_eq!(read(&stream, 100), (kResultOk, Vec::new()), "the end is not an error");
        assert_eq!(tell(&stream), 11);
    }

    #[test]
    fn writing_overwrites_then_grows() {
        let (owner, stream) = stream(b"");
        assert_eq!(write(&stream, b"abcdef"), kResultOk);
        assert_eq!(seek(&stream, 2, kIBSeekSet as u32), (kResultOk, 2));
        assert_eq!(write(&stream, b"XYZ12345"), kResultOk);
        assert_eq!(tell(&stream), 10);
        assert_eq!(owner.take(), b"abXYZ12345");
        assert_eq!(owner.take(), b"", "taking empties the stream");
        assert_eq!(tell(&stream), 0);
    }

    #[test]
    fn seeking_stays_inside_the_stream() {
        let (_owner, stream) = stream(b"0123456789");
        assert_eq!(seek(&stream, 4, kIBSeekSet as u32), (kResultOk, 4));
        assert_eq!(seek(&stream, 3, kIBSeekCur as u32), (kResultOk, 7));
        assert_eq!(seek(&stream, -2, kIBSeekEnd as u32), (kResultOk, 8));
        assert_eq!(read(&stream, 10).1, b"89");
        assert_eq!(seek(&stream, 0, kIBSeekEnd as u32), (kResultOk, 10));

        for (pos, mode) in
            [(-1, kIBSeekSet), (11, kIBSeekSet), (1, kIBSeekEnd), (-11, kIBSeekCur), (i64::MAX, kIBSeekCur)]
        {
            assert_eq!(seek(&stream, pos, mode as u32).0, kResultFalse, "{pos} from {mode}");
        }
        assert_eq!(seek(&stream, 0, 99).0, kInvalidArgument);
        assert_eq!(tell(&stream), 10, "a refused seek leaves the position alone");
    }

    #[test]
    fn the_size_can_be_asked_and_set() {
        let (owner, stream) = stream(b"0123456789");
        let sizeable = stream.cast::<ISizeableStream>().expect("the class implements ISizeableStream");
        let mut size = -1;
        // SAFETY: `size` is an `int64`.
        unsafe {
            assert_eq!(sizeable.getStreamSize(&mut size), kResultOk);
            assert_eq!(size, 10);
            assert_eq!(seek(&stream, 0, kIBSeekEnd as u32), (kResultOk, 10));
            assert_eq!(sizeable.setStreamSize(4), kResultOk);
            assert_eq!(tell(&stream), 4, "the position is pulled back into the stream");
            assert_eq!(sizeable.setStreamSize(6), kResultOk);
            assert_eq!(sizeable.setStreamSize(-1), kInvalidArgument);
        }
        assert_eq!(owner.take(), b"0123\0\0");
    }

    #[test]
    fn nonsense_arguments_are_refused_not_dereferenced() {
        let (owner, stream) = stream(b"abc");
        let mut byte = 0u8;
        let buffer: *mut c_void = (&raw mut byte).cast();
        // SAFETY: every pointer passed is null or points to `byte`, and
        // the byte counts never exceed one.
        unsafe {
            assert_eq!(stream.read(null_mut(), 1, null_mut()), kInvalidArgument);
            assert_eq!(stream.read(buffer, -1, null_mut()), kInvalidArgument);
            assert_eq!(stream.write(null_mut(), 1, null_mut()), kInvalidArgument);
            assert_eq!(stream.write(buffer, -1, null_mut()), kInvalidArgument);
            assert_eq!(stream.tell(null_mut()), kInvalidArgument);
            // The results a caller does not want are optional.
            assert_eq!(stream.read(buffer, 1, null_mut()), kResultOk);
            assert_eq!(stream.write(buffer, 1, null_mut()), kResultOk);
            assert_eq!(stream.seek(0, kIBSeekSet as int32, null_mut()), kResultOk);
        }
        assert_eq!(owner.take(), b"aac");
    }

    #[test]
    fn a_reference_the_plugin_keeps_outlives_the_hosts() {
        let (owner, stream) = stream(b"kept");
        drop(owner);
        assert_eq!(read(&stream, 4).1, b"kept");
    }
}
