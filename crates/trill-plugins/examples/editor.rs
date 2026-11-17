//! Opens the editor of one installed plugin, the way the DAW will: a small
//! host around `trill-plugins` for looking at a plugin's own interface and
//! for checking the hosting of it by eye.
//!
//! ```text
//! cargo run -p trill-plugins --example editor -- <device> [options]
//!
//! <device>                 a device URI (`clap:<plugin id>`, `vst3:<class id>`), or a
//!                          scheme and a plugin's name (`vst3:Surge XT`), or `list`
//! --screenshot <png>       save a picture of the editor's contents; the window is kept
//!                          in front of others (without the focus) until it is taken,
//!                          since a covered window does not paint
//! --settle <seconds>       wait this long before taking the picture (default 2), and up
//!                          to eight seconds longer for an editor that has drawn nothing
//! --exit-after <seconds>   close the editor and exit after this long; without it the
//!                          program runs until the editor's window is closed
//! --position <x,y>         where to put the window, in screen pixels
//! --title <text>           the window's title
//! --activate               let the window take the keyboard focus (it does not, by default)
//! --owned                  make the window belong to a hidden window of this program
//! --note                   hold middle C, so that an instrument has something to show
//! --drag <x1,y1,x2,y2>     drag the mouse across the editor, by messages to its window
//!                          only: the real pointer and the focus are left alone
//! --reopen                 close the editor and open it a second time before exiting
//! ```
//!
//! The program never touches a dialog a plugin puts up by itself (a licence
//! prompt, say). It reports the window, saves a picture of it next to the
//! screenshot and exits, which closes it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use trill_core::device::{
    CompositeFactory, DeviceController, DeviceDescriptor, DeviceFactory, DeviceProcessor, EditorImage, EditorOptions,
    Event, EventKind, ParamEditKind, ParamInfo, ProcessInfo,
};
use trill_core::{ChainRef, Project};
use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};

const SAMPLE_RATE: f64 = 48_000.0;
const BLOCK: usize = 512;
/// How often the program looks at what the editor and the plugin did.
const POLL: Duration = Duration::from_millis(50);
/// How long the whole run may take beyond what was asked for before the
/// program gives up on a plugin that hangs, and exits.
const GRACE: Duration = Duration::from_secs(45);
/// How often an editor that has not drawn yet is looked at again.
const LOOK: Duration = Duration::from_millis(250);
/// How much longer than asked the picture waits for an editor that is
/// still blank. Editors that draw with the graphics card can take seconds
/// to show their first frame.
const PATIENCE: Duration = Duration::from_secs(8);

struct Options {
    device: String,
    screenshot: Option<PathBuf>,
    settle: Duration,
    exit_after: Option<Duration>,
    position: Option<(i32, i32)>,
    title: Option<String>,
    activate: bool,
    owned: bool,
    note: bool,
    drag: Option<[i32; 4]>,
    reopen: bool,
}

fn usage() -> String {
    "usage: editor <device uri | scheme:plugin name | list> [--screenshot <png>] [--settle <seconds>] \
     [--exit-after <seconds>] [--position <x,y>] [--title <text>] [--activate] [--owned] [--note] \
     [--drag <x1,y1,x2,y2>] [--reopen]"
        .to_string()
}

fn numbers<const N: usize>(text: &str) -> Result<[i32; N], String> {
    let parsed: Vec<i32> = text.split(',').filter_map(|part| part.trim().parse().ok()).collect();
    parsed.try_into().map_err(|_| format!("'{text}' is not {N} whole numbers separated by commas"))
}

fn seconds(text: &str) -> Result<Duration, String> {
    let value: f64 = text.parse().map_err(|_| format!("'{text}' is not a number of seconds"))?;
    if value.is_finite() && value >= 0.0 {
        Ok(Duration::from_secs_f64(value))
    } else {
        Err(format!("'{text}' seconds?"))
    }
}

fn parse(mut arguments: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        device: String::new(),
        screenshot: None,
        settle: Duration::from_secs(2),
        exit_after: None,
        position: None,
        title: None,
        activate: false,
        owned: false,
        note: false,
        drag: None,
        reopen: false,
    };
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or_else(|| format!("{argument} needs a value\n{}", usage()));
        match argument.as_str() {
            "--screenshot" => options.screenshot = Some(PathBuf::from(value()?)),
            "--settle" => options.settle = seconds(&value()?)?,
            "--exit-after" => options.exit_after = Some(seconds(&value()?)?),
            "--position" => options.position = Some(numbers::<2>(&value()?)?.into()),
            "--title" => options.title = Some(value()?),
            "--activate" => options.activate = true,
            "--owned" => options.owned = true,
            "--note" => options.note = true,
            "--drag" => options.drag = Some(numbers::<4>(&value()?)?),
            "--reopen" => options.reopen = true,
            _ if argument.starts_with("--") => return Err(format!("unknown option {argument}\n{}", usage())),
            _ if options.device.is_empty() => options.device = argument,
            _ => return Err(format!("unexpected argument '{argument}'\n{}", usage())),
        }
    }
    if options.device.is_empty() { Err(usage()) } else { Ok(options) }
}

/// Prints what the host logs, which includes what plugins ask of it that
/// it does not provide.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level().as_str().to_lowercase(), record.args());
        }
    }

    fn flush(&self) {}
}

/// The descriptor `wanted` names: by its URI, or by scheme and name.
fn find(descriptors: &[DeviceDescriptor], wanted: &str) -> Result<DeviceDescriptor, String> {
    if let Some(found) = descriptors.iter().find(|descriptor| descriptor.uri == wanted) {
        return Ok(found.clone());
    }
    let (scheme, name) = wanted.split_once(':').ok_or_else(|| format!("'{wanted}' is not a device URI"))?;
    let named: Vec<&DeviceDescriptor> = descriptors
        .iter()
        .filter(|descriptor| {
            descriptor.uri.starts_with(&format!("{scheme}:")) && descriptor.name.eq_ignore_ascii_case(name)
        })
        .collect();
    match named.as_slice() {
        [found] => Ok((*found).clone()),
        [] => Err(format!("no installed plugin is '{wanted}'; run with `list` to see what there is")),
        several => Err(format!("{} plugins are named '{name}'; use one of their URIs", several.len())),
    }
}

/// Processes blocks at the pace a sound card would ask for them, until
/// told to stop: what the engine's audio thread does to a device, without
/// the engine and without making a sound.
fn drive(mut processor: Box<dyn DeviceProcessor>, note: bool, stop: Arc<AtomicBool>) {
    let (mut left, mut right) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let block_time = Duration::from_secs_f64(BLOCK as f64 / SAMPLE_RATE);
    let started = Instant::now();
    let mut blocks = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let info = ProcessInfo {
            sample_rate: SAMPLE_RATE,
            tempo: 120.0,
            time_signature: Default::default(),
            playing: true,
            position_beats: f64::from(blocks) * BLOCK as f64 / SAMPLE_RATE * 2.0,
            song: Default::default(),
            loop_range: None,
        }
        .steady();
        let events: &[Event] = if note && blocks == 20 {
            &[Event { offset: 0, kind: EventKind::NoteOn { pitch: 60, velocity: 0.8 } }]
        } else {
            &[]
        };
        left.fill(0.0);
        right.fill(0.0);
        processor.process(&info, events, &mut left, &mut right);
        blocks += 1;
        if let Some(ahead) = (block_time * blocks).checked_sub(started.elapsed()) {
            thread::sleep(ahead);
        }
    }
    // `processor` drops here, which tears the plugin down.
}

fn save(image: &EditorImage, path: &Path) -> Result<(), String> {
    let describe = |error: &dyn std::fmt::Display| format!("cannot write '{}': {error}", path.display());
    let file = std::fs::File::create(path).map_err(|error| describe(&error))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| describe(&error))?;
    writer.write_image_data(&image.rgba).map_err(|error| describe(&error))
}

/// How much of a picture differs from its first pixel: an editor that has
/// not drawn, or could not be captured, is one flat color.
fn varied_share(image: &EditorImage) -> f64 {
    let Some(first) = image.rgba.get(..3) else { return 0.0 };
    let differs = |pixel: &&[u8]| pixel[..3].iter().zip(first).any(|(channel, first)| channel.abs_diff(*first) > 16);
    let varied = image.rgba.chunks_exact(4).filter(differs).count();
    varied as f64 / (image.width as f64 * image.height as f64).max(1.0)
}

/// What the plugin's parameters are called, to report edits by.
fn name_of(params: &[ParamInfo], index: usize) -> &str {
    params.get(index).map_or("?", |param| param.name.as_str())
}

/// Reports the edits the plugin made to its own parameters since the last
/// call. Returns how many there were.
fn report_edits(controller: &mut dyn DeviceController, params: &[ParamInfo]) -> usize {
    let edits = controller.take_param_edits();
    for edit in &edits {
        let name = name_of(params, edit.index);
        match edit.kind {
            ParamEditKind::Begin => println!("edit: the user took hold of '{name}'"),
            ParamEditKind::Value(value) => println!("edit: '{name}' = {value}"),
            ParamEditKind::End => println!("edit: the user let go of '{name}'"),
        }
    }
    edits.len()
}

fn run(options: &Options) -> Result<ExitCode, String> {
    // The caches are this program's own, so that it does not disturb the
    // application's.
    let cache = std::env::temp_dir().join("trill-editor-example");
    let host = PluginHost::with_configs(
        ClapConfig { scanner: Scanner::Auto, cache_path: Some(cache.join("clap.json")), ..ClapConfig::default() },
        Vst3Config { scanner: Scanner::Auto, cache_path: Some(cache.join("vst3.json")), ..Vst3Config::default() },
    )
    .map_err(|error| format!("cannot start the plugin host: {error}"))?;
    let factory = CompositeFactory::new(host.factories());
    let descriptors = factory.descriptors();
    if options.device == "list" {
        for descriptor in &descriptors {
            println!("{}\t{}\t{}", descriptor.uri, descriptor.name, descriptor.vendor);
        }
        return Ok(ExitCode::SUCCESS);
    }
    let descriptor = find(&descriptors, &options.device)?;
    println!("plugin: {} by {} ({})", descriptor.name, descriptor.vendor, descriptor.uri);

    let mut project = Project::new("editor example");
    let id = project
        .add_device(ChainRef::Master, descriptor.uri.clone(), descriptor.name.clone(), None)
        .map_err(|error| error.to_string())?;
    let instance = factory
        .create(project.device(id).map_err(|error| error.to_string())?, SAMPLE_RATE, BLOCK)
        .map_err(|error| error.to_string())?;
    let params = instance.params;
    let mut controller = instance.controller.ok_or("the device has no controller")?;
    println!("parameters: {}; has an editor: {}", params.len(), controller.has_editor());
    let before = controller.param_values();

    let stop = Arc::new(AtomicBool::new(false));
    let audio = {
        let (processor, note, stop) = (instance.processor, options.note, stop.clone());
        thread::Builder::new().name("audio".into()).spawn(move || drive(processor, note, stop))
    }
    .map_err(|error| format!("cannot start the audio thread: {error}"))?;

    let owner = options.owned.then(platform::Owner::create);
    let title = options.title.clone().unwrap_or_else(|| format!("{} - Trill editor example", descriptor.name));
    let editor = EditorOptions {
        title: title.clone(),
        owner: owner.as_ref().map(platform::Owner::handle),
        position: options.position,
        activate: options.activate,
    };
    let opening = Instant::now();
    let opened = controller.open_editor(&editor);
    println!("open: {opened:?} after {:.0?}; the editor is open: {}", opening.elapsed(), controller.is_editor_open());

    let mut outcome = ExitCode::SUCCESS;
    let mut edits = 0;
    if opened.is_ok() {
        if let Some(geometry) = platform::describe(&title) {
            println!("window: {geometry}");
        }
        if options.screenshot.is_some() {
            platform::keep_in_front(&title);
        }
        let started = Instant::now();
        let wants_picture = options.screenshot.is_some();
        let (mut captured, mut dragged, mut reopened) = (!wants_picture, options.drag.is_none(), !options.reopen);
        // When the editor first showed anything, and when it was last
        // looked at for that.
        let (mut drawn, mut looked) = (None, Duration::ZERO);
        // The longest the plugin's main thread kept a request waiting
        // while the editor had yet to draw.
        let mut longest_wait = Duration::ZERO;
        loop {
            thread::sleep(POLL);
            if let Some(owner) = &owner {
                owner.pump();
            }
            edits += report_edits(controller.as_mut(), &params);
            // A window of the plugin's own making: not this program's to
            // deal with. Record it and leave.
            let strays = platform::stray_windows(&title);
            if !strays.is_empty() {
                for (index, stray) in strays.iter().enumerate() {
                    println!("the plugin opened a window of its own: {}", stray.description);
                    if let (Some(path), Some(image)) = (&options.screenshot, platform::capture(stray)) {
                        let path = path.with_extension(format!("dialog{index}.png"));
                        println!("  its picture: {} ({:?})", path.display(), save(&image, &path));
                    }
                }
                outcome = ExitCode::from(3);
                break;
            }
            if !controller.is_editor_open() {
                println!("the editor's window was closed");
                break;
            }
            let elapsed = started.elapsed();
            let watching = wants_picture || options.drag.is_some();
            if watching && drawn.is_none() && elapsed >= options.settle.max(looked + LOOK) {
                looked = elapsed;
                let asked = Instant::now();
                let image = controller.capture_editor();
                longest_wait = longest_wait.max(asked.elapsed());
                if image.is_some_and(|image| varied_share(&image) >= 0.01) {
                    let after = opening.elapsed();
                    println!(
                        "drawn: the editor shows something after {after:.1?} (its thread answered within {longest_wait:.0?})"
                    );
                    drawn = Some(elapsed);
                }
            }
            // The picture is taken once the editor has had the time that
            // was asked for and has drawn, or patience runs out.
            let ready = elapsed >= options.settle
                && (drawn.is_some_and(|drawn| elapsed >= drawn + LOOK) || elapsed >= options.settle + PATIENCE);
            if !dragged && ready {
                dragged = true;
                let [x1, y1, x2, y2] = options.drag.unwrap_or_default();
                println!("drag: from {x1},{y1} to {x2},{y2}: {}", platform::drag(&title, (x1, y1), (x2, y2)));
                // What the drag did shows in the picture.
                drawn = Some(elapsed + Duration::from_millis(600));
            } else if !captured && ready {
                captured = true;
                let path = options.screenshot.as_deref().unwrap_or(Path::new(""));
                match controller.capture_editor() {
                    Some(image) => {
                        let saved = save(&image, path);
                        let varied = varied_share(&image) * 100.0;
                        let blank =
                            if varied < 1.0 { " (blank: nothing was drawn, or it could not be captured)" } else { "" };
                        println!("screenshot: {}x{}, {varied:.0}% varied{blank}, {saved:?}", image.width, image.height);
                    }
                    None => {
                        println!("screenshot: the editor could not be captured");
                        outcome = ExitCode::from(4);
                    }
                }
            } else if captured && dragged && !reopened {
                reopened = true;
                controller.close_editor();
                println!("reopen: closed; the editor is open: {}", controller.is_editor_open());
                println!(
                    "reopen: {:?}; the editor is open: {}",
                    controller.open_editor(&editor),
                    controller.is_editor_open()
                );
                if let Some(geometry) = platform::describe(&title) {
                    println!("window: {geometry}");
                }
            }
            // The time asked for is over, and nothing that was asked for
            // is still to do.
            if options.exit_after.is_some_and(|limit| elapsed >= limit) && captured && dragged && reopened {
                break;
            }
        }
    } else {
        outcome = ExitCode::from(1);
    }

    // What the plugin's parameters are now, against what they were.
    if let (Some(before), Some(after)) = (before, controller.param_values()) {
        let changed: Vec<String> = before
            .iter()
            .zip(&after)
            .enumerate()
            .filter(|(_, (before, after))| before != after)
            .map(|(index, (before, after))| format!("'{}' {before} -> {after}", name_of(&params, index)))
            .collect();
        println!(
            "edits reported: {edits}; parameters changed: {}",
            if changed.is_empty() { "none".into() } else { changed.join(", ") }
        );
    }

    let closing = Instant::now();
    controller.close_editor();
    println!("close: the editor is open: {} after {:.0?}", controller.is_editor_open(), closing.elapsed());
    drop(controller);
    stop.store(true, Ordering::Relaxed);
    audio.join().map_err(|_| "the audio thread panicked")?;
    drop(owner);
    drop(factory);
    drop(host);
    println!("done: everything is torn down after {:.0?}", closing.elapsed());
    Ok(outcome)
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    static LOG: StderrLog = StderrLog;
    if log::set_logger(&LOG).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
    // As the application does when it starts its own window.
    platform::be_dpi_aware();

    // A plugin that hangs, or blocks its thread with a dialog, must not
    // leave this program and its windows behind: exiting closes them.
    let limit = options.exit_after.map(|asked| asked + options.settle + PATIENCE + GRACE);
    if let Some(limit) = limit {
        thread::spawn(move || {
            thread::sleep(limit);
            eprintln!("still running after {limit:.0?}; giving up on the plugin and exiting");
            for stray in platform::stray_windows("") {
                eprintln!("a window that was open: {}", stray.description);
            }
            std::process::exit(5);
        });
    }
    match run(&options) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}

/// What the program needs of Windows beyond the plugin host: the things
/// an application's own window brings (an owner for the editor, a message
/// loop), and ways of looking at windows without touching them.
#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use trill_core::device::EditorImage;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GdiFlush, GetDC, HDC, MapWindowPoints, ReleaseDC, SelectObject,
    };
    use windows_sys::Win32::Storage::Xps::PrintWindow;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CWP_SKIPINVISIBLE, ChildWindowFromPointEx, CreateWindowExW, DestroyWindow, DispatchMessageW, EnumWindows,
        FindWindowW, GWL_STYLE, GetClassNameW, GetClientRect, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
        GetWindowThreadProcessId, HWND_TOPMOST, IsWindowVisible, MSG, PM_REMOVE, PeekMessageW, PostMessageW,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, TranslateMessage, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MOUSEMOVE, WS_OVERLAPPEDWINDOW, WS_THICKFRAME,
    };

    /// `PrintWindow`'s flag for capturing what the compositor shows.
    const PW_RENDERFULLCONTENT: u32 = 2;
    /// The mouse message flag for "the left button is down".
    const MK_LBUTTON: usize = 1;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    pub fn be_dpi_aware() {
        // SAFETY: the context is one of the system's constants. The call
        // fails harmlessly if the process has made its choice already.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    }

    /// A hidden window of the program's main thread, for an editor to
    /// belong to as it would to the application's window.
    pub struct Owner(HWND);

    impl Owner {
        pub fn create() -> Self {
            // SAFETY: "STATIC" is a window class the system provides; the
            // strings are NUL-terminated and outlive the call.
            let window = unsafe {
                let (class, title) = (wide("STATIC"), wide("Trill editor example (owner)"));
                let module = GetModuleHandleW(null());
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_OVERLAPPEDWINDOW,
                    0,
                    0,
                    200,
                    100,
                    null_mut(),
                    null_mut(),
                    module,
                    null(),
                )
            };
            Self(window)
        }

        pub fn handle(&self) -> isize {
            self.0 as isize
        }

        /// Dispatches the messages waiting for this thread's windows, as
        /// an application's event loop does all the time.
        pub fn pump(&self) {
            // SAFETY: `MSG` is plain data that `PeekMessageW` fills in; a
            // null window selects the calling thread's messages.
            unsafe {
                let mut message: MSG = std::mem::zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            // SAFETY: the window is this thread's own.
            unsafe { DestroyWindow(self.0) };
        }
    }

    /// A top-level window of this process.
    pub struct Stray {
        window: HWND,
        pub description: String,
    }

    fn text_of(window: HWND, read: unsafe extern "system" fn(HWND, *mut u16, i32) -> i32) -> String {
        let mut buffer = [0u16; 256];
        // SAFETY: the buffer holds as many units as the call is told.
        let length = unsafe { read(window, buffer.as_mut_ptr(), buffer.len() as i32) };
        String::from_utf16_lossy(&buffer[..length.clamp(0, buffer.len() as i32) as usize])
    }

    fn rect_of(window: HWND) -> RECT {
        // SAFETY: the rectangle is a local the system fills in.
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            GetWindowRect(window, &mut rect);
            rect
        }
    }

    /// The visible top-level windows of this process other than the one
    /// titled `editor` and the program's own.
    pub fn stray_windows(editor: &str) -> Vec<Stray> {
        unsafe extern "system" fn collect(window: HWND, found: LPARAM) -> i32 {
            // SAFETY: `found` is the vector `stray_windows` passed, which
            // outlives the enumeration.
            unsafe {
                let mut process = 0;
                GetWindowThreadProcessId(window, &mut process);
                if process == GetCurrentProcessId() && IsWindowVisible(window) != 0 {
                    (*(found as *mut Vec<HWND>)).push(window);
                }
            }
            1
        }
        let mut windows: Vec<HWND> = Vec::new();
        // SAFETY: the callback only uses the vector for the duration of
        // the call.
        unsafe { EnumWindows(Some(collect), (&raw mut windows) as LPARAM) };
        windows
            .into_iter()
            .filter_map(|window| {
                let (title, class) = (text_of(window, GetWindowTextW), text_of(window, GetClassNameW));
                // The editor itself, the owner it may belong to, and the
                // console window of a program started from a terminal.
                let own = title == editor || title.ends_with("(owner)") || class == "ConsoleWindowClass";
                let rect = rect_of(window);
                let empty = rect.right <= rect.left || rect.bottom <= rect.top;
                (!own && !empty).then(|| Stray {
                    window,
                    description: format!(
                        "'{title}' (class {class}) at {},{} size {}x{}",
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top
                    ),
                })
            })
            .collect()
    }

    /// A picture of `width` by `height` pixels, painted by `paint`, which
    /// is given the bitmap to paint into and the screen to copy from, and
    /// says whether it managed.
    fn picture(width: i32, height: i32, paint: impl FnOnce(HDC, HDC) -> bool) -> Option<EditorImage> {
        if width <= 0 || height <= 0 {
            return None;
        }
        // SAFETY: every GDI object made here is released before returning.
        // The bitmap is 32 bits per pixel and `width` by `height`, so
        // `bits` points to `width * height * 4` bytes while it exists.
        unsafe {
            let screen = GetDC(null_mut());
            let memory = CreateCompatibleDC(screen);
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bits: *mut c_void = null_mut();
            let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            let mut image = None;
            if !bitmap.is_null() && !bits.is_null() {
                let previous = SelectObject(memory, bitmap);
                if paint(memory, screen) {
                    GdiFlush();
                    let bgra = std::slice::from_raw_parts(bits.cast::<u8>(), width as usize * height as usize * 4);
                    let rgba = bgra.chunks_exact(4).flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 255]).collect();
                    image = Some(EditorImage { width: width as u32, height: height as u32, rgba });
                }
                SelectObject(memory, previous);
            }
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
            image
        }
    }

    /// A picture of a whole window, frame included, as the window drew it.
    pub fn capture(stray: &Stray) -> Option<EditorImage> {
        let rect = rect_of(stray.window);
        // SAFETY: the window may be gone by now, which makes the call fail.
        picture(rect.right - rect.left, rect.bottom - rect.top, |memory, _| unsafe {
            PrintWindow(stray.window, memory, PW_RENDERFULLCONTENT) != 0
        })
    }

    fn editor_window(title: &str) -> Option<HWND> {
        let title = wide(title);
        // SAFETY: the title is NUL-terminated; a null class matches any.
        let window = unsafe { FindWindowW(null(), title.as_ptr()) };
        (!window.is_null()).then_some(window)
    }

    /// Keeps the editor's window in front of every other, without giving
    /// it the focus: a window that is covered does not paint, and one that
    /// has not painted makes an empty picture.
    pub fn keep_in_front(title: &str) {
        if let Some(window) = editor_window(title) {
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
            // SAFETY: only the window's place in the stack is changed.
            unsafe { SetWindowPos(window, HWND_TOPMOST, 0, 0, 0, 0, flags) };
        }
    }

    /// Where the editor's window is and how it is set up, in words.
    pub fn describe(title: &str) -> Option<String> {
        let window = editor_window(title)?;
        // SAFETY: the rectangle is a local, and the style an integer.
        unsafe {
            let outer = rect_of(window);
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(window, &mut client);
            let dpi = GetDpiForWindow(window);
            let resizable = GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_THICKFRAME != 0;
            Some(format!(
                "at {},{}, {}x{} outside, {}x{} inside; {dpi} dpi ({}%); resizable by the user: {resizable}",
                outer.left,
                outer.top,
                outer.right - outer.left,
                outer.bottom - outer.top,
                client.right,
                client.bottom,
                dpi * 100 / 96
            ))
        }
    }

    /// Drags the left mouse button from `from` to `to`, both inside the
    /// editor's window, by posting to the plugin's window the messages a
    /// real drag would bring it. The pointer on screen does not move.
    pub fn drag(title: &str, from: (i32, i32), to: (i32, i32)) -> String {
        let Some(editor) = editor_window(title) else { return "the editor has no window".to_string() };
        // SAFETY: plain calls with locals; a window handle that has gone
        // stale makes them fail, not misbehave.
        unsafe {
            // The plugin's own window under the starting point, however
            // deep it nests them.
            let (mut target, mut origin) = (editor, POINT { x: from.0, y: from.1 });
            loop {
                let child = ChildWindowFromPointEx(target, origin, CWP_SKIPINVISIBLE);
                if child.is_null() || child == target {
                    break;
                }
                MapWindowPoints(target, child, &mut origin, 1);
                target = child;
            }
            if target == editor {
                return "there is no window of the plugin's at that point".to_string();
            }
            let offset = (origin.x - from.0, origin.y - from.1);
            let at = |(x, y): (i32, i32)| (((y + offset.1) as u16 as isize) << 16) | ((x + offset.0) as u16 as isize);
            PostMessageW(target, WM_MOUSEMOVE, 0, at(from));
            PostMessageW(target, WM_LBUTTONDOWN, MK_LBUTTON, at(from));
            const STEPS: i32 = 12;
            for step in 1..=STEPS {
                std::thread::sleep(std::time::Duration::from_millis(15));
                let point = (from.0 + (to.0 - from.0) * step / STEPS, from.1 + (to.1 - from.1) * step / STEPS);
                PostMessageW(target, WM_MOUSEMOVE, MK_LBUTTON, at(point));
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
            PostMessageW(target, WM_LBUTTONUP, 0, at(to));
            format!("posted to the plugin's window of class '{}'", text_of(target, GetClassNameW))
        }
    }
}

/// Editors are hosted on Windows only so far; elsewhere the program gets
/// as far as the host telling it so.
#[cfg(not(windows))]
mod platform {
    use trill_core::device::EditorImage;

    pub fn be_dpi_aware() {}

    pub struct Owner;

    impl Owner {
        pub fn create() -> Self {
            Self
        }

        pub fn handle(&self) -> isize {
            0
        }

        pub fn pump(&self) {}
    }

    pub struct Stray {
        pub description: String,
    }

    pub fn stray_windows(_editor: &str) -> Vec<Stray> {
        Vec::new()
    }

    pub fn capture(_stray: &Stray) -> Option<EditorImage> {
        None
    }

    pub fn keep_in_front(_title: &str) {}

    pub fn describe(_title: &str) -> Option<String> {
        None
    }

    pub fn drag(_title: &str, _from: (i32, i32), _to: (i32, i32)) -> String {
        "not supported on this platform".to_string()
    }
}
