//! A loaded VST3 module: the library, its entry and exit functions, and
//! the class factory it exports.
//!
//! Loading follows the SDK's "module" contract. The host loads the
//! library, calls its entry function, and gets the factory from
//! `GetPluginFactory`. When it is done it releases the factory, calls the
//! exit function and unloads the library, in that order. The functions'
//! names differ by platform; on Windows they are optional.

use libloading::Library;
use std::path::Path;
use vst3::ComPtr;
use vst3::Steinberg::IPluginFactory;

/// `GetPluginFactory`, exported by every module. The caller owns the
/// reference it returns.
type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;
/// A module's exit function. C++ `bool` is returned as a byte, and read as
/// one: a plugin returning something other than 0 or 1 must not produce an
/// invalid Rust `bool`.
type Exit = unsafe extern "C" fn() -> u8;

const GET_FACTORY: &[u8] = b"GetPluginFactory\0";

/// A VST3 module that is ready to use. Dropping it shuts the module down
/// and unloads its library; every object obtained from it must be gone by
/// then.
pub(crate) struct Module {
    /// `None` only while the module is being loaded or dropped.
    factory: Option<ComPtr<IPluginFactory>>,
    /// Called once the factory is released, if the module has one.
    exit: Option<Exit>,
    /// Whatever the entry function needs kept for as long as the module
    /// is in use.
    _context: platform::Context,
    /// `None` for a factory that did not come from a library. Declared
    /// last, so that the library is unloaded after everything else.
    _library: Option<Library>,
}

impl Module {
    /// Loads the library at `path` and brings the module up.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        // SAFETY: loading a plugin runs code this crate has no control
        // over; no check made here could make that sound. The file is
        // named as a VST3 module, and the damage one that is not can do is
        // contained where it matters: scanning runs in its own process.
        let library = unsafe { Library::new(path) }.map_err(|error| error.to_string())?;
        let (context, exit) = platform::enter(&library, path)?;
        // From here on, dropping `module` calls the exit function.
        let mut module = Self { factory: None, exit, _context: context, _library: None };
        // SAFETY: a VST3 module exports `GetPluginFactory` with this
        // signature, and may be asked for its factory once entered.
        let factory = unsafe {
            let get_factory = library.get::<GetFactory>(GET_FACTORY);
            let get_factory = get_factory.map_err(|_| "it is not a VST3 module (no GetPluginFactory)")?;
            ComPtr::from_raw(get_factory())
        };
        module.factory = Some(factory.ok_or("the module provides no plugin factory")?);
        module._library = Some(library);
        Ok(module)
    }

    /// Wraps a factory that is compiled into this program, as if it had
    /// been loaded from a library.
    #[cfg(test)]
    pub(crate) fn from_factory(factory: ComPtr<IPluginFactory>) -> Self {
        Self { factory: Some(factory), exit: None, _context: Default::default(), _library: None }
    }

    /// The module's class factory.
    pub(crate) fn factory(&self) -> &ComPtr<IPluginFactory> {
        // Set for the whole time between `load` and `drop`.
        self.factory.as_ref().expect("a loaded module has a factory")
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // The factory goes first: after the exit function nothing of the
        // module may be touched.
        self.factory = None;
        if let Some(exit) = self.exit {
            // SAFETY: the entry function succeeded, and every object of
            // the module is released, which is when the exit function is
            // to be called.
            unsafe { exit() };
        }
        // The fields are dropped next, in declaration order: the entry
        // context, then the library.
    }
}

#[cfg(windows)]
mod platform {
    use super::{Exit, Library, Path};

    /// `InitDll`. Like its counterpart `ExitDll`, a module need not have
    /// one.
    type Init = unsafe extern "C" fn() -> u8;

    #[derive(Default)]
    pub(super) struct Context;

    pub(super) fn enter(library: &Library, _path: &Path) -> Result<(Context, Option<Exit>), String> {
        // SAFETY: if the library exports these names, it is a VST3 module
        // and they have these signatures. `InitDll` is called first, once.
        unsafe {
            if let Ok(init) = library.get::<Init>(b"InitDll\0")
                && init() == 0
            {
                return Err("the module failed to initialize (InitDll)".to_string());
            }
            Ok((Context, library.get::<Exit>(b"ExitDll\0").ok().map(|exit| *exit)))
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::{Exit, Library, Path};
    use std::ffi::c_void;

    /// `ModuleEntry`, which takes the library's handle. A module must have
    /// it and `ModuleExit`.
    type Entry = unsafe extern "C" fn(*mut c_void) -> u8;

    #[derive(Default)]
    pub(super) struct Context;

    pub(super) fn enter(library: &Library, path: &Path) -> Result<(Context, Option<Exit>), String> {
        let missing = |name: &str| format!("it is not a VST3 module (no {name})");
        // SAFETY: a VST3 module exports these names with these signatures.
        // The handle passed to `ModuleEntry` is the one `dlopen` returned;
        // opening the library again yields the same handle, and it stays
        // valid because `library` holds the library open throughout.
        unsafe {
            let entry = library.get::<Entry>(b"ModuleEntry\0").map_err(|_| missing("ModuleEntry"))?;
            let exit = *library.get::<Exit>(b"ModuleExit\0").map_err(|_| missing("ModuleExit"))?;
            let handle = libloading::os::unix::Library::new(path).map_err(|error| error.to_string())?.into_raw();
            let entered = entry(handle) != 0;
            // Balances the second open; the first keeps the library loaded.
            drop(libloading::os::unix::Library::from_raw(handle));
            if !entered {
                return Err("the module failed to initialize (ModuleEntry)".to_string());
            }
            Ok((Context, Some(exit)))
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Exit, Library, Path};
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;

    /// `bundleEntry`, which takes the plugin's `CFBundleRef`. A module
    /// must have it and `bundleExit`.
    type Entry = unsafe extern "C" fn(*mut c_void) -> u8;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            length: isize,
            is_directory: u8,
        ) -> *const c_void;
        fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *mut c_void;
        fn CFRelease(object: *const c_void);
    }

    /// The `CFBundleRef` the module was entered with, which the module may
    /// use for as long as it is loaded.
    pub(super) struct Context(*mut c_void);

    impl Default for Context {
        fn default() -> Self {
            Self(std::ptr::null_mut())
        }
    }

    impl Drop for Context {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the pointer is the bundle `CFBundleCreate`
                // returned, whose one reference this struct owns.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    pub(super) fn enter(library: &Library, path: &Path) -> Result<(Context, Option<Exit>), String> {
        let missing = |name: &str| format!("it is not a VST3 module (no {name})");
        // The library is `Name.vst3/Contents/MacOS/Name`.
        let bundle = path.ancestors().nth(3).ok_or("the library is not inside a bundle")?;
        let bundle = bundle.as_os_str().as_bytes();
        // SAFETY: a VST3 module exports these names with these signatures.
        // The Core Foundation calls are given a valid byte buffer with its
        // length and the default allocator (null); what they return is
        // checked for null and released exactly once.
        unsafe {
            let entry = library.get::<Entry>(b"bundleEntry\0").map_err(|_| missing("bundleEntry"))?;
            let exit = *library.get::<Exit>(b"bundleExit\0").map_err(|_| missing("bundleExit"))?;
            let url =
                CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bundle.as_ptr(), bundle.len() as isize, 1);
            if url.is_null() {
                return Err("cannot make a URL of the bundle's path".to_string());
            }
            let context = Context(CFBundleCreate(std::ptr::null(), url));
            CFRelease(url);
            if context.0.is_null() {
                return Err("the bundle cannot be opened".to_string());
            }
            if entry(context.0) == 0 {
                return Err("the module failed to initialize (bundleEntry)".to_string());
            }
            Ok((context, Some(exit)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    #[test]
    fn a_file_that_is_not_a_library_is_an_error() {
        let dir = TestDir::new();
        let path = dir.file("fake.vst3", b"this is not a shared library");
        let error = Module::load(&path).err().expect("not a library");
        assert!(!error.is_empty());
        assert!(Module::load(&dir.path().join("missing.vst3")).is_err());
    }

    /// A library that loads but is no VST3 module: this test's own
    /// executable would do, but a system library is certain to export
    /// nothing by that name.
    #[cfg(windows)]
    #[test]
    fn a_library_without_a_factory_is_not_a_module() {
        let system = std::env::var_os("SystemRoot").expect("Windows has a system root");
        let library = Path::new(&system).join("System32").join("version.dll");
        let error = Module::load(&library).err().expect("not a VST3 module");
        assert!(error.contains("not a VST3 module"), "{error}");
    }
}
