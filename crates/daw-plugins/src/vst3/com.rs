//! Small helpers for the COM boundary between this host and a plugin.

use super::class_id::ClassId;
use std::ffi::c_void;
use std::ptr;
use vst3::Steinberg::{
    IPluginFactory, IPluginFactoryTrait, kInternalError, kInvalidArgument, kNoInterface, kNotImplemented,
    kNotInitialized, kOutOfMemory, kResultFalse, kResultOk, tresult,
};
use vst3::{Class, ComPtr, ComWrapper, Interface};

/// Stores `value` where an out-parameter points, unless it is null. Callers
/// on both sides of the VST3 API pass null for results they do not want.
///
/// # Safety
///
/// `target` must be null or valid for writing a `T`.
pub(crate) unsafe fn write_out<T>(target: *mut T, value: T) {
    if !target.is_null() {
        // SAFETY: not null, so valid for a write per this function's
        // contract. Nothing is known about how the caller aligned it.
        unsafe { target.write_unaligned(value) };
    }
}

/// The interface `I` of one of this host's own objects, as the pointer a
/// plugin is handed. The pointer is borrowed: it does not count as a
/// reference, and is valid for as long as `object` (or a reference the
/// plugin took) keeps the object alive.
///
/// Null if the object's class does not implement `I`.
pub(crate) fn interface<C: Class, I: Interface>(object: &ComWrapper<C>) -> *mut I {
    object.as_com_ref::<I>().map_or(ptr::null_mut(), |interface| interface.as_ptr())
}

/// Asks a module's factory for an object of class `class`, as its
/// interface `I`. `None` if the module has no such class or the class no
/// such interface.
pub(crate) fn create_instance<I: Interface>(factory: &ComPtr<IPluginFactory>, class: ClassId) -> Option<ComPtr<I>> {
    let class = class.to_tuid();
    let mut object: *mut c_void = ptr::null_mut();
    // SAFETY: `factory` is a live factory, both ids are 16 bytes that
    // outlive the call, and `object` is a local the factory writes to.
    let result = unsafe { factory.createInstance(class.as_ptr(), I::IID.as_ptr().cast(), &mut object) };
    if result != kResultOk {
        // Whatever is in `object` now, nothing says it is a reference
        // this host owns.
        return None;
    }
    // SAFETY: the factory reported success, so `object` is null or a
    // reference to an `I` that the caller owns.
    unsafe { ComPtr::from_raw(object.cast::<I>()) }
}

/// A result code, for messages.
pub(crate) fn result_name(result: tresult) -> String {
    let known = [
        (kResultOk, "ok"),
        (kResultFalse, "false"),
        (kNoInterface, "no interface"),
        (kInvalidArgument, "invalid argument"),
        (kNotImplemented, "not implemented"),
        (kInternalError, "internal error"),
        (kNotInitialized, "not initialized"),
        (kOutOfMemory, "out of memory"),
    ];
    match known.iter().find(|(code, _)| *code == result) {
        Some((_, name)) => (*name).to_string(),
        None => format!("result {result:#X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_parameters_are_optional() {
        let mut target = 1;
        // SAFETY: the pointer is null or to a local.
        unsafe {
            write_out(&mut target, 2);
            write_out(ptr::null_mut::<i32>(), 3);
        }
        assert_eq!(target, 2);
    }

    #[test]
    fn result_codes_have_names() {
        assert_eq!(result_name(kResultFalse), "false");
        assert_eq!(result_name(kNotImplemented), "not implemented");
        assert_eq!(result_name(0x1234), "result 0x1234");
    }
}
