//! Where a recording goes that Trill removes: never out of existence.
//!
//! A recording is somebody's voice, and what says it is unused is a list a
//! program made. So a recording that is removed is **never unlinked**. It
//! goes to the system's Recycle Bin, from where the system gives it back;
//! and where there is no bin for it (a system without one, a drive whose
//! files the bin does not take, a file too big for it), into a folder
//! called `removed` beside the takes, from where anybody does.
//!
//! The bin is asked through the shell's own file operation, with undo
//! allowed. The shell deletes for good what it cannot recycle unless it is
//! told otherwise; it is told otherwise here, by a sink that refuses every
//! deletion that is not into the bin. What is left where it was after that
//! is moved to the folder.

use std::path::{Path, PathBuf};

/// The folder beside the takes that holds what was removed and could not
/// go to the system's bin.
pub const REMOVED_FOLDER: &str = "removed";

/// Where recordings that are removed go.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Bin {
    /// A folder called `removed` beside the takes. What a run that keeps
    /// nothing of the user's uses, a test among them: nothing of theirs
    /// is put in the user's own bin.
    #[default]
    Folder,
    /// The system's Recycle Bin where it has one that takes the file, and
    /// the folder otherwise.
    System,
}

/// What became of recordings that were removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    /// How much room they took where they were, in bytes.
    pub bytes: u64,
    /// How many went to the system's Recycle Bin.
    pub recycled: usize,
    /// The ones that were moved into the `removed` folder, where they are
    /// now.
    pub set_aside: Vec<PathBuf>,
}

impl Removed {
    /// Where they went, for a sentence: "to the Recycle Bin", "to
    /// C:\…\removed".
    pub fn went(&self) -> String {
        let folder = self.set_aside.first().and_then(|path| path.parent()).map(|folder| folder.display().to_string());
        match (self.recycled, folder) {
            (0, Some(folder)) => format!("to {folder}"),
            (_, Some(folder)) => format!("to the Recycle Bin, and {} to {folder}", self.set_aside.len()),
            (_, None) => "to the Recycle Bin".to_string(),
        }
    }
}

impl Bin {
    /// Where a recording in `folder` would go, for the question that is
    /// asked before one does.
    pub fn goes(self, folder: &Path) -> String {
        match self {
            Bin::System if cfg!(windows) => "to the Recycle Bin".to_string(),
            _ => format!("to {}", folder.join(REMOVED_FOLDER).display()),
        }
    }

    /// Removes `paths`, each with its size in bytes, and says where they
    /// went. One that could be put nowhere stays where it is, and is the
    /// error; what went before it has gone.
    pub(super) fn remove(self, paths: &[(PathBuf, u64)]) -> Result<Removed, (Removed, PathBuf, std::io::Error)> {
        let mut removed = Removed::default();
        if self == Bin::System {
            let asked: Vec<PathBuf> = paths.iter().map(|(path, _)| path.clone()).collect();
            // What it took is gone from its place; what it did not is not.
            let _ = recycle(&asked);
        }
        for (path, bytes) in paths {
            if path.exists() {
                // The bin did not take it, or was not asked.
                match set_aside(path) {
                    Ok(now) => removed.set_aside.push(now),
                    Err(error) => return Err((removed, path.clone(), error)),
                }
            }
            removed.bytes += bytes;
        }
        // The count is of what is gone from its place and in no folder.
        removed.recycled = paths.len() - removed.set_aside.len();
        Ok(removed)
    }
}

/// Moves `path` into the `removed` folder beside it, under a name nothing
/// there has, and returns where it is now.
fn set_aside(path: &Path) -> std::io::Result<PathBuf> {
    let folder = path.parent().unwrap_or(Path::new(".")).join(REMOVED_FOLDER);
    std::fs::create_dir_all(&folder)?;
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "recording".into());
    let extension = path.extension().map(|extension| format!(".{}", extension.to_string_lossy())).unwrap_or_default();
    for number in 1u32.. {
        let name = if number == 1 { format!("{stem}{extension}") } else { format!("{stem} ({number}){extension}") };
        let to = folder.join(name);
        if !to.exists() {
            std::fs::rename(path, &to)?;
            return Ok(to);
        }
    }
    unreachable!("some number is free")
}

/// Asks the system to take `paths` into its Recycle Bin, and returns how
/// many it says it took. What it did not take is where it was.
#[cfg(not(windows))]
fn recycle(_paths: &[PathBuf]) -> usize {
    0
}

#[cfg(windows)]
fn recycle(paths: &[PathBuf]) -> usize {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (asked, taken) = (paths.to_vec(), Arc::new(AtomicUsize::new(0)));
    let counted = taken.clone();
    // The shell's objects live in an apartment of their own: a thread
    // that is made for this and ends with it.
    let worked = std::thread::Builder::new().name("recycle".into()).spawn(move || shell::recycle(&asked, counted));
    match worked.map(std::thread::JoinHandle::join) {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => log::warn!("the Recycle Bin did not take the recordings: {error}"),
        _ => log::warn!("the Recycle Bin could not be asked"),
    }
    taken.load(Ordering::SeqCst)
}

#[cfg(windows)]
mod shell {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows::Win32::Foundation::E_ABORT;
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOFX_RECYCLEONDELETE, FileOperation,
        IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
        SHCreateItemFromParsingName, TSF_DELETE_RECYCLE_IF_POSSIBLE,
    };
    use windows::core::{HRESULT, HSTRING, PCWSTR, Ref, Result, implement};

    /// Watches the shell's deletions: refuses one that would not go to
    /// the bin, and counts the ones that did.
    #[implement(IFileOperationProgressSink)]
    struct Sink {
        taken: Arc<AtomicUsize>,
    }

    #[allow(non_snake_case, reason = "the shell's own names")]
    impl IFileOperationProgressSink_Impl for Sink_Impl {
        fn StartOperations(&self) -> Result<()> {
            Ok(())
        }

        fn FinishOperations(&self, _result: HRESULT) -> Result<()> {
            Ok(())
        }

        fn PreRenameItem(&self, _flags: u32, _item: Ref<IShellItem>, _name: &PCWSTR) -> Result<()> {
            Ok(())
        }

        fn PostRenameItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            _name: &PCWSTR,
            _result: HRESULT,
            _made: Ref<IShellItem>,
        ) -> Result<()> {
            Ok(())
        }

        fn PreMoveItem(&self, _flags: u32, _item: Ref<IShellItem>, _to: Ref<IShellItem>, _name: &PCWSTR) -> Result<()> {
            Ok(())
        }

        fn PostMoveItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            _to: Ref<IShellItem>,
            _name: &PCWSTR,
            _result: HRESULT,
            _made: Ref<IShellItem>,
        ) -> Result<()> {
            Ok(())
        }

        fn PreCopyItem(&self, _flags: u32, _item: Ref<IShellItem>, _to: Ref<IShellItem>, _name: &PCWSTR) -> Result<()> {
            Ok(())
        }

        fn PostCopyItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            _to: Ref<IShellItem>,
            _name: &PCWSTR,
            _result: HRESULT,
            _made: Ref<IShellItem>,
        ) -> Result<()> {
            Ok(())
        }

        fn PreDeleteItem(&self, flags: u32, _item: Ref<IShellItem>) -> Result<()> {
            // The shell says here whether the item is going to the bin.
            // One that is not would be gone for good: it is refused, and
            // stays where it is.
            if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
                return Err(E_ABORT.into());
            }
            Ok(())
        }

        fn PostDeleteItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            result: HRESULT,
            in_the_bin: Ref<IShellItem>,
        ) -> Result<()> {
            if result.is_ok() && !in_the_bin.is_null() {
                self.taken.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }

        fn PreNewItem(&self, _flags: u32, _to: Ref<IShellItem>, _name: &PCWSTR) -> Result<()> {
            Ok(())
        }

        fn PostNewItem(
            &self,
            _flags: u32,
            _to: Ref<IShellItem>,
            _name: &PCWSTR,
            _template: &PCWSTR,
            _attributes: u32,
            _result: HRESULT,
            _made: Ref<IShellItem>,
        ) -> Result<()> {
            Ok(())
        }

        fn UpdateProgress(&self, _total: u32, _so_far: u32) -> Result<()> {
            Ok(())
        }

        fn ResetTimer(&self) -> Result<()> {
            Ok(())
        }

        fn PauseTimer(&self) -> Result<()> {
            Ok(())
        }

        fn ResumeTimer(&self) -> Result<()> {
            Ok(())
        }
    }

    /// Lets go of the apartment when the work is over, however it ends.
    struct Apartment;

    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: made only after `CoInitializeEx` succeeded on this thread.
            unsafe { CoUninitialize() };
        }
    }

    pub(super) fn recycle(paths: &[PathBuf], taken: Arc<AtomicUsize>) -> Result<()> {
        // SAFETY: the shell's file operation is used as its documentation
        // says, on the one thread that made it, in an apartment that is
        // left when the thread ends; the strings handed over outlive the
        // calls they are handed to.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()?;
            let _apartment = Apartment;
            let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            // Into the bin, with undo; without a question, a progress
            // window or a word on screen: the front end asked already.
            operation.SetOperationFlags(
                FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOFX_RECYCLEONDELETE,
            )?;
            let sink: IFileOperationProgressSink = Sink { taken }.into();
            let advised = operation.Advise(&sink)?;
            let mut asked = 0;
            for path in paths {
                let name = HSTRING::from(path.as_os_str());
                let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&name, None) else { continue };
                if operation.DeleteItem(&item, None).is_ok() {
                    asked += 1;
                }
            }
            // (A deletion the sink refused makes the whole a failure by
            // the shell's account; what matters is what is still there.)
            let done = if asked > 0 { operation.PerformOperations() } else { Ok(()) };
            let _ = operation.Unadvise(advised);
            done
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording(folder: &Path, name: &str, bytes: usize) -> (PathBuf, u64) {
        let path = folder.join(name);
        std::fs::write(&path, vec![7u8; bytes]).unwrap();
        (path, bytes as u64)
    }

    #[test]
    fn what_is_removed_is_set_aside_beside_the_takes_and_never_unlinked() {
        let folder = tempfile::tempdir().unwrap();
        let (voice, other) = (recording(folder.path(), "Voice.wav", 100), recording(folder.path(), "Other.wav", 40));
        let removed = Bin::Folder.remove(&[voice.clone(), other.clone()]).unwrap();
        let aside = folder.path().join(REMOVED_FOLDER);
        assert_eq!((removed.bytes, removed.recycled), (140, 0));
        assert_eq!(removed.set_aside, [aside.join("Voice.wav"), aside.join("Other.wav")]);
        assert!(!voice.0.exists() && !other.0.exists());
        assert_eq!(std::fs::read(aside.join("Voice.wav")).unwrap(), vec![7u8; 100], "what it held is all there");
        assert_eq!(removed.went(), format!("to {}", aside.display()));
        assert_eq!(Bin::Folder.goes(folder.path()), format!("to {}", aside.display()));

        // Another of the same name does not take the place of the first.
        let again = recording(folder.path(), "Voice.wav", 60);
        let removed = Bin::Folder.remove(&[again]).unwrap();
        assert_eq!(removed.set_aside, [aside.join("Voice (2).wav")]);
        assert_eq!(std::fs::read(aside.join("Voice.wav")).unwrap().len(), 100);
        assert_eq!(std::fs::read(aside.join("Voice (2).wav")).unwrap().len(), 60);
    }

    /// Puts one small file of this test's own into the Recycle Bin of
    /// whoever runs it, and leaves it there.
    #[test]
    #[cfg(windows)]
    #[ignore = "puts a file in this machine's Recycle Bin; run with `cargo test -p trill-engine --lib the_systems_bin -- --ignored --nocapture`"]
    fn the_systems_bin_takes_a_recording_and_says_so() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("trill test recording (safe to delete).wav");
        std::fs::write(&path, vec![0u8; 2048]).unwrap();
        let taken = recycle(std::slice::from_ref(&path));
        println!("the Recycle Bin took {taken} of 1: '{}'", path.display());
        assert_eq!(taken, 1, "the shell says it is in the bin");
        assert!(!path.exists(), "and it is gone from where it was");
        // Through the whole of it: recycled, and nothing set aside.
        let (again, bytes) = recording(folder.path(), "trill test recording (safe to delete).wav", 1024);
        let removed = Bin::System.remove(&[(again.clone(), bytes)]).unwrap();
        assert_eq!((removed.recycled, removed.set_aside.len(), removed.bytes), (1, 0, 1024));
        assert_eq!(removed.went(), "to the Recycle Bin");
        assert!(!again.exists() && !folder.path().join(REMOVED_FOLDER).exists());
    }
}
