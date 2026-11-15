//! The host side of the VST3 API: what a plugin may ask of the DAW.
//!
//! Like its CLAP sibling, the host is deliberately static. A device's
//! parameter list, buses and latency are read once when it is created and
//! reported to the engine then; the device API has no way to change them
//! afterwards. So requests that amount to "my shape changed, look again"
//! are noted, and take effect the next time the device is created.

use super::com::{interface, write_out};
use super::links::ParamLinks;
use super::text;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use vst3::Steinberg::Vst::IAttributeList_::AttrID;
use vst3::Steinberg::Vst::{
    IAttributeList, IAttributeListTrait, IComponentHandler, IComponentHandlerTrait, IHostApplication,
    IHostApplicationTrait, IMessage, IMessageTrait, ParamID, ParamValue, String128, TChar,
};
use vst3::Steinberg::{FIDString, TUID, int32, int64, kInvalidArgument, kResultFalse, kResultOk, tresult, uint32};
use vst3::{Class, ComWrapper, Interface};

/// The name plugins know this host by.
const HOST_NAME: &str = "DAW";
/// Longest string attribute read from a plugin, in UTF-16 units. The end
/// of a string is wherever the plugin put a NUL; this keeps a missing one
/// from being searched for indefinitely.
const MAX_STRING_UNITS: usize = 1 << 16;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The context every plugin object is initialized with: who the host is,
/// and where a plugin gets the messages its two halves send each other.
pub(crate) struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication,);
}

impl HostApplication {
    pub(crate) fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self)
    }
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        let mut buffer: String128 = [0; 128];
        text::to_utf16(HOST_NAME, &mut buffer);
        // SAFETY: `name` is not null, so it is the caller's `String128`.
        unsafe { write_out(name, buffer) };
        kResultOk
    }

    /// Makes the objects only a host can make: messages and their
    /// attribute lists.
    unsafe fn createInstance(&self, cid: *mut TUID, iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
        if obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `obj` is not null, so it is the caller's pointer
        // variable. It is cleared first so that every failure leaves null.
        unsafe { write_out(obj, null_mut()) };
        if cid.is_null() || iid.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: neither is null, so each points to a 16-byte id.
        let (class, wanted) =
            unsafe { (cid.cast::<[u8; 16]>().read_unaligned(), iid.cast::<[u8; 16]>().read_unaligned()) };
        let object: *mut c_void = if class == IMessage::IID && wanted == IMessage::IID {
            Message::new().to_com_ptr::<IMessage>().map_or(null_mut(), |message| message.into_raw().cast())
        } else if class == IAttributeList::IID && wanted == IAttributeList::IID {
            let list = AttributeList::new().to_com_ptr::<IAttributeList>();
            list.map_or(null_mut(), |list| list.into_raw().cast())
        } else {
            null_mut()
        };
        if object.is_null() {
            return kResultFalse;
        }
        // SAFETY: as above. The caller receives the reference `into_raw`
        // gave up.
        unsafe { write_out(obj, object) };
        kResultOk
    }
}

/// What a plugin's edit controller tells the host: edits made in the
/// plugin's own editor, and requests to look at the plugin again.
///
/// A plugin may call this from any thread, so it only writes atomics.
pub(crate) struct ComponentHandler {
    /// Set once the plugin's parameters are known. Edits before that have
    /// nowhere to go: the processor half does not exist yet.
    links: OnceLock<Arc<ParamLinks>>,
    /// The VST3 restart flags requested and not yet looked at.
    restart: AtomicI32,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl ComponentHandler {
    pub(crate) fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self { links: OnceLock::new(), restart: AtomicI32::new(0) })
    }

    /// Connects the handler to the processor half. Only the first call has
    /// an effect.
    pub(crate) fn connect(&self, links: Arc<ParamLinks>) {
        let _ = self.links.set(links);
    }

    /// The restart flags the plugin requested since the last call.
    pub(crate) fn take_restart_requests(&self) -> int32 {
        self.restart.swap(0, Ordering::AcqRel)
    }
}

impl IComponentHandlerTrait for ComponentHandler {
    /// The start and end of a gesture are not passed on: the session
    /// records plugin state when it saves, not edit by edit.
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    /// The user moved a control in the plugin's editor. The controller
    /// already has the value; the processor gets it with its next block.
    unsafe fn performEdit(&self, id: ParamID, value_normalized: ParamValue) -> tresult {
        if !value_normalized.is_finite() {
            return kInvalidArgument;
        }
        let Some(links) = self.links.get() else { return kResultFalse };
        let Some(slot) = links.slot(id) else { return kInvalidArgument };
        links.to_processor.post(slot, value_normalized.clamp(0.0, 1.0));
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        self.restart.fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

/// A message one half of a plugin sends the other. The plugin fills it in
/// and passes it on itself; the host only provides the object.
struct Message {
    /// Every id the message was given, the current one last. Earlier ones
    /// are kept so that a pointer the plugin got from `getMessageID` stays
    /// valid for as long as the message does.
    ids: Mutex<Vec<CString>>,
    attributes: ComWrapper<AttributeList>,
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl Message {
    fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self { ids: Mutex::new(Vec::new()), attributes: AttributeList::new() })
    }
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        lock(&self.ids).last().map_or(std::ptr::null(), |id| id.as_ptr())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        if id.is_null() {
            return;
        }
        // SAFETY: `id` is not null, and a message id is a NUL-terminated
        // string by the API's contract.
        let id = unsafe { CStr::from_ptr(id) }.to_owned();
        lock(&self.ids).push(id);
    }

    /// The pointer is borrowed, as the VST3 API has it: valid for as long
    /// as the message.
    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        interface(&self.attributes)
    }
}

enum Attribute {
    Int(int64),
    Float(f64),
    /// UTF-16, with its terminating NUL.
    String(Vec<TChar>),
    Binary(Vec<u8>),
}

/// The contents of a message: values by name.
struct AttributeList {
    values: Mutex<HashMap<CString, Attribute>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

impl AttributeList {
    fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self { values: Mutex::new(HashMap::new()) })
    }

    fn set(&self, id: AttrID, value: Attribute) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `id` is not null, and an attribute id is a
        // NUL-terminated string by the API's contract.
        let id = unsafe { CStr::from_ptr(id) }.to_owned();
        lock(&self.values).insert(id, value);
        kResultOk
    }

    /// Calls `read` with the attribute `id`, if there is one. `read`
    /// answers whether the attribute was of the kind asked for.
    fn get(&self, id: AttrID, read: impl FnOnce(&Attribute) -> bool) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: as in `set`.
        let id = unsafe { CStr::from_ptr(id) };
        match lock(&self.values).get(id) {
            Some(attribute) if read(attribute) => kResultOk,
            _ => kResultFalse,
        }
    }
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: AttrID, value: int64) -> tresult {
        self.set(id, Attribute::Int(value))
    }

    unsafe fn getInt(&self, id: AttrID, value: *mut int64) -> tresult {
        self.get(id, |attribute| match attribute {
            Attribute::Int(stored) => {
                // SAFETY: `value` is null or the caller's `int64`.
                unsafe { write_out(value, *stored) };
                true
            }
            _ => false,
        })
    }

    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        self.set(id, Attribute::Float(value))
    }

    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        self.get(id, |attribute| match attribute {
            Attribute::Float(stored) => {
                // SAFETY: `value` is null or the caller's `f64`.
                unsafe { write_out(value, *stored) };
                true
            }
            _ => false,
        })
    }

    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut units = Vec::new();
        // SAFETY: `string` is not null and NUL-terminated by the API's
        // contract, so every unit up to its terminator is readable.
        while let unit @ 1.. = unsafe { string.add(units.len()).read_unaligned() } {
            units.push(unit);
            if units.len() == MAX_STRING_UNITS {
                break;
            }
        }
        units.push(0);
        self.set(id, Attribute::String(units))
    }

    /// Copies as much of the string as fits, always terminated.
    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: uint32) -> tresult {
        let capacity = size_in_bytes as usize / size_of::<TChar>();
        if string.is_null() || capacity == 0 {
            return kInvalidArgument;
        }
        self.get(id, |attribute| match attribute {
            Attribute::String(stored) => {
                let count = (stored.len() - 1).min(capacity - 1);
                // SAFETY: the caller's buffer holds `capacity` units, and
                // `count + 1` is at most that. It is the caller's own
                // memory, so it does not overlap the stored string.
                unsafe {
                    std::ptr::copy_nonoverlapping(stored.as_ptr(), string, count);
                    string.add(count).write_unaligned(0);
                }
                true
            }
            _ => false,
        })
    }

    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size_in_bytes: uint32) -> tresult {
        if data.is_null() && size_in_bytes > 0 {
            return kInvalidArgument;
        }
        let bytes = if size_in_bytes == 0 {
            Vec::new()
        } else {
            // SAFETY: `data` is not null and, by the API's contract, holds
            // `size_in_bytes` readable bytes.
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size_in_bytes as usize) }.to_vec()
        };
        self.set(id, Attribute::Binary(bytes))
    }

    /// The data is not copied: the caller gets a pointer into the list,
    /// valid until the attribute is set again or the list is released.
    unsafe fn getBinary(&self, id: AttrID, data: *mut *const c_void, size_in_bytes: *mut uint32) -> tresult {
        self.get(id, |attribute| match attribute {
            Attribute::Binary(stored) => {
                // SAFETY: each pointer is null or the caller's variable.
                unsafe {
                    write_out(data, stored.as_ptr().cast());
                    write_out(size_in_bytes, stored.len() as uint32);
                }
                true
            }
            _ => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::{ComPtr, ComRef};

    #[test]
    fn the_host_introduces_itself() {
        let host = HostApplication::new();
        let host: ComPtr<IHostApplication> = host.to_com_ptr().unwrap();
        let mut name: String128 = [0xFFFF; 128];
        // SAFETY: `name` is a local `String128`; null is refused.
        unsafe {
            assert_eq!(host.getName(&mut name), kResultOk);
            assert_eq!(host.getName(null_mut()), kInvalidArgument);
        }
        assert_eq!(text::from_utf16(&name), HOST_NAME);
    }

    /// Asks the host for an object of class `class` as interface `wanted`.
    fn create(host: &ComPtr<IHostApplication>, class: [u8; 16], wanted: [u8; 16]) -> (tresult, *mut c_void) {
        let (mut class, mut wanted) = (class.map(|byte| byte as i8), wanted.map(|byte| byte as i8));
        let mut object: *mut c_void = std::ptr::dangling_mut();
        // SAFETY: the ids are 16 bytes each and `object` is a local.
        let result = unsafe { host.createInstance(class.as_mut_ptr().cast(), wanted.as_mut_ptr().cast(), &mut object) };
        (result, object)
    }

    #[test]
    fn the_host_makes_messages_and_attribute_lists_and_nothing_else() {
        let host: ComPtr<IHostApplication> = HostApplication::new().to_com_ptr().unwrap();

        let (result, message) = create(&host, IMessage::IID, IMessage::IID);
        assert_eq!(result, kResultOk);
        // SAFETY: on success the host stored a reference the caller owns.
        let message = unsafe { ComPtr::from_raw(message.cast::<IMessage>()) }.expect("a message");
        let (result, list) = create(&host, IAttributeList::IID, IAttributeList::IID);
        assert_eq!(result, kResultOk);
        // SAFETY: as above.
        let list = unsafe { ComPtr::from_raw(list.cast::<IAttributeList>()) }.expect("an attribute list");

        for (class, wanted) in [
            (IMessage::IID, IAttributeList::IID),
            (IHostApplication::IID, IHostApplication::IID),
            ([0; 16], IMessage::IID),
        ] {
            let (result, object) = create(&host, class, wanted);
            assert_eq!(result, kResultFalse);
            assert!(object.is_null(), "a failure leaves the caller's pointer null");
        }
        // SAFETY: null arguments are refused before they are used.
        unsafe {
            let mut id = IMessage::IID.map(|byte| byte as i8);
            let mut object: *mut c_void = std::ptr::dangling_mut();
            assert_eq!(host.createInstance(null_mut(), id.as_mut_ptr().cast(), &mut object), kInvalidArgument);
            assert!(object.is_null());
            assert_eq!(
                host.createInstance(id.as_mut_ptr().cast(), id.as_mut_ptr().cast(), null_mut()),
                kInvalidArgument
            );
        }

        // The objects are the caller's: they outlive the host.
        drop(host);
        // SAFETY: both objects are alive; the id is a C string literal.
        unsafe {
            message.setMessageID(c"hello".as_ptr());
            assert_eq!(list.setInt(c"n".as_ptr(), 1), kResultOk);
        }
    }

    #[test]
    fn a_message_carries_an_id_and_attributes() {
        let message = Message::new();
        let message: ComPtr<IMessage> = message.to_com_ptr().unwrap();
        // SAFETY: the message is alive throughout, ids are C string
        // literals, and the returned pointers are used while it lives.
        unsafe {
            assert!(message.getMessageID().is_null(), "no id yet");
            message.setMessageID(c"first".as_ptr());
            let first = message.getMessageID();
            message.setMessageID(c"second".as_ptr());
            message.setMessageID(std::ptr::null());
            assert_eq!(CStr::from_ptr(message.getMessageID()), c"second");
            assert_eq!(CStr::from_ptr(first), c"first", "an id handed out earlier is still readable");

            let attributes = ComRef::from_raw(message.getAttributes()).expect("a message has attributes");
            assert_eq!(attributes.setFloat(c"gain".as_ptr(), 0.5), kResultOk);
            let mut gain = 0.0;
            assert_eq!(attributes.getFloat(c"gain".as_ptr(), &mut gain), kResultOk);
            assert_eq!(gain, 0.5);
            assert_eq!(message.getAttributes(), attributes.as_ptr(), "always the same list");
        }
    }

    #[test]
    fn attributes_keep_their_kind_and_value() {
        let list: ComPtr<IAttributeList> = AttributeList::new().to_com_ptr().unwrap();
        let wide: Vec<u16> = "Größe\0".encode_utf16().collect();
        let blob = [1u8, 2, 3, 250];
        // SAFETY: ids are C string literals; every other pointer is null
        // or to a local of the right type and size.
        unsafe {
            assert_eq!(list.setInt(c"int".as_ptr(), -7), kResultOk);
            assert_eq!(list.setFloat(c"float".as_ptr(), 2.5), kResultOk);
            assert_eq!(list.setString(c"string".as_ptr(), wide.as_ptr()), kResultOk);
            assert_eq!(list.setBinary(c"binary".as_ptr(), blob.as_ptr().cast(), 4), kResultOk);
            assert_eq!(list.setBinary(c"empty".as_ptr(), std::ptr::null(), 0), kResultOk);

            let (mut int, mut float) = (0, 0.0);
            assert_eq!(list.getInt(c"int".as_ptr(), &mut int), kResultOk);
            assert_eq!(list.getFloat(c"float".as_ptr(), &mut float), kResultOk);
            assert_eq!((int, float), (-7, 2.5));

            let mut string = [0xFFFFu16; 16];
            assert_eq!(list.getString(c"string".as_ptr(), string.as_mut_ptr(), 32), kResultOk);
            assert_eq!(text::from_utf16(&string), "Größe");
            // A buffer that is too small gets what fits, terminated.
            let mut short = [0xFFFFu16; 3];
            assert_eq!(list.getString(c"string".as_ptr(), short.as_mut_ptr(), 6), kResultOk);
            assert_eq!(short, [u16::from(b'G'), u16::from(b'r'), 0]);

            let (mut data, mut size) = (std::ptr::null(), 0);
            assert_eq!(list.getBinary(c"binary".as_ptr(), &mut data, &mut size), kResultOk);
            assert_eq!(std::slice::from_raw_parts(data.cast::<u8>(), size as usize), blob);
            assert_eq!(list.getBinary(c"empty".as_ptr(), &mut data, &mut size), kResultOk);
            assert_eq!(size, 0);

            // Asking for the wrong kind, or for nothing that exists, fails.
            assert_eq!(list.getInt(c"float".as_ptr(), &mut int), kResultFalse);
            assert_eq!(list.getFloat(c"missing".as_ptr(), &mut float), kResultFalse);
            assert_eq!(list.getString(c"int".as_ptr(), string.as_mut_ptr(), 32), kResultFalse);
            assert_eq!(list.getBinary(c"string".as_ptr(), &mut data, &mut size), kResultFalse);
            // Setting again replaces, also with another kind.
            assert_eq!(list.setInt(c"float".as_ptr(), 3), kResultOk);
            assert_eq!(list.getInt(c"float".as_ptr(), &mut int), kResultOk);
            assert_eq!(int, 3);
        }
    }

    #[test]
    fn attribute_calls_with_nonsense_arguments_are_refused() {
        let list: ComPtr<IAttributeList> = AttributeList::new().to_com_ptr().unwrap();
        let mut unit = 0u16;
        // SAFETY: every pointer is null, a C string literal or `unit`.
        unsafe {
            assert_eq!(list.setInt(std::ptr::null(), 1), kInvalidArgument);
            assert_eq!(list.getInt(std::ptr::null(), null_mut()), kInvalidArgument);
            assert_eq!(list.setString(c"s".as_ptr(), std::ptr::null()), kInvalidArgument);
            assert_eq!(list.setBinary(c"b".as_ptr(), std::ptr::null(), 4), kInvalidArgument);
            assert_eq!(list.setString(c"s".as_ptr(), &raw const unit), kResultOk);
            assert_eq!(list.getString(c"s".as_ptr(), null_mut(), 32), kInvalidArgument);
            assert_eq!(list.getString(c"s".as_ptr(), &raw mut unit, 1), kInvalidArgument, "no room for a single unit");
            assert_eq!(list.getString(c"s".as_ptr(), &raw mut unit, 2), kResultOk);
            // Results the caller does not want are optional.
            assert_eq!(list.setInt(c"i".as_ptr(), 1), kResultOk);
            assert_eq!(list.getInt(c"i".as_ptr(), null_mut()), kResultOk);
        }
    }

    #[test]
    fn edits_reach_the_processor_once_it_exists() {
        let handler = ComponentHandler::new();
        let plugin: ComPtr<IComponentHandler> = handler.to_com_ptr().unwrap();
        let collected = |links: &ParamLinks| {
            let mut values = Vec::new();
            links.to_processor.collect(|slot, value| values.push((slot, value)));
            values
        };
        // SAFETY: the handler's methods take plain values.
        unsafe {
            assert_eq!(plugin.performEdit(7, 0.5), kResultFalse, "nowhere to send it yet");

            let links = Arc::new(ParamLinks::new(vec![3, 7]));
            handler.connect(links.clone());
            assert_eq!(plugin.beginEdit(7), kResultOk);
            assert_eq!(plugin.performEdit(7, 0.5), kResultOk);
            assert_eq!(plugin.performEdit(3, 7.0), kResultOk);
            assert_eq!(plugin.endEdit(7), kResultOk);
            assert_eq!(collected(&links), [(0, 1.0), (1, 0.5)], "by slot, and inside 0..=1");

            assert_eq!(plugin.performEdit(99, 0.5), kInvalidArgument, "not a parameter of this plugin");
            assert_eq!(plugin.performEdit(7, f64::NAN), kInvalidArgument);
            assert_eq!(collected(&links), []);
            assert_eq!(collected_by_controller(&links), [], "an edit is not echoed back to the controller");
        }
    }

    fn collected_by_controller(links: &ParamLinks) -> Vec<(usize, f64)> {
        let mut values = Vec::new();
        links.to_controller.collect(|slot, value| values.push((slot, value)));
        values
    }

    #[test]
    fn restart_requests_are_recorded_until_taken() {
        let handler = ComponentHandler::new();
        let plugin: ComPtr<IComponentHandler> = handler.to_com_ptr().unwrap();
        assert_eq!(handler.take_restart_requests(), 0);
        // SAFETY: the handler's methods take plain values.
        unsafe {
            assert_eq!(plugin.restartComponent(0b0100), kResultOk);
            assert_eq!(plugin.restartComponent(0b1000), kResultOk);
            assert_eq!(plugin.restartComponent(0b0100), kResultOk);
        }
        assert_eq!(handler.take_restart_requests(), 0b1100, "requests accumulate");
        assert_eq!(handler.take_restart_requests(), 0);
    }
}
