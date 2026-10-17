//! Objective-C/CoreFoundation ownership is contained in this adapter.
use super::*;
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWorkspace;
use objc2_application_services::{AXError, AXIsProcessTrusted, AXUIElement};
use objc2_core_foundation::{CFRetained, CFString, CFType};
use std::ptr::NonNull;

fn attribute(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let name = CFString::from_str(name);
    let mut raw = std::ptr::null();
    // SAFETY: name/element live through the call; out points to initialized local
    // pointer storage. Copy API transfers one owned retain only on success.
    let status = unsafe { element.copy_attribute_value(&name, NonNull::from(&mut raw)) };
    if status != AXError::Success {
        return None;
    }
    NonNull::new(raw.cast_mut()).map(|ptr| unsafe { CFRetained::from_raw(ptr) })
}
fn text_attribute(element: &AXUIElement, name: &str, limit: usize) -> Option<String> {
    let value = attribute(element, name)?;
    let value = value.downcast_ref::<CFString>()?;
    // Bound before allocating a UTF-8 conversion (UTF-16 length is conservative).
    if value.length() < 0 || value.length() as usize > limit {
        return None;
    }
    let text = value.to_string();
    (text.len() <= limit && !text.contains('\0')).then_some(text)
}
fn bounded_string(
    value: Option<objc2::rc::Retained<objc2_foundation::NSString>>,
) -> Option<String> {
    let value = value?;
    if value.length() > MAX_APP_BYTES {
        return None;
    }
    let value = value.to_string();
    (value.len() <= MAX_APP_BYTES && !value.contains('\0')).then_some(value)
}
/// No AX permission prompt is issued. Missing permission yields app-only data.
/// Calls are sequential, with 100ms AX messaging timeouts; no UI tree traversal.
pub fn capture() -> Result<Observation, AwarenessError> {
    autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        let Some(app) = workspace.frontmostApplication() else {
            return Ok(Observation {
                app: None,
                window: None,
                status: Status::Unavailable,
            });
        };
        let app = AppInfo {
            pid: app.processIdentifier(),
            bundle_id: bounded_string(app.bundleIdentifier()),
            name: bounded_string(app.localizedName()),
        };
        let mut observation = Observation {
            app: Some(app.clone()),
            window: None,
            status: Status::Unavailable,
        };
        if !app.bundle_id.as_deref().is_some_and(window_content_allowed) {
            observation.status = Status::Excluded;
            return Ok(observation);
        }
        // SAFETY: AX trust query is read-only and does not request permission.
        if !unsafe { AXIsProcessTrusted() } {
            observation.status = Status::PermissionMissing;
            return Ok(observation);
        }
        if app.pid <= 0 {
            return Ok(observation);
        }
        // SAFETY: positive live process ID comes from NSWorkspace. Retained AX
        // elements own native references, and all copied attributes are type checked.
        let element = unsafe { AXUIElement::new_application(app.pid) };
        unsafe {
            element.set_messaging_timeout(0.1);
        }
        let Some(window) = attribute(&element, "AXFocusedWindow")
            .and_then(|value| value.downcast::<AXUIElement>().ok())
        else {
            observation.status = Status::NoFocusedWindow;
            return Ok(observation);
        };
        unsafe {
            window.set_messaging_timeout(0.1);
        }
        observation.window = Some(WindowInfo {
            id: text_attribute(&window, "AXIdentifier", MAX_WINDOW_ID_BYTES),
            title: text_attribute(&window, "AXTitle", MAX_TITLE_BYTES),
        });
        // A focus switch during AX reads invalidates the capture instead of
        // attaching an old window to the new frontmost application.
        if workspace
            .frontmostApplication()
            .is_none_or(|current| current.processIdentifier() != app.pid)
        {
            observation.window = None;
            observation.status = Status::Unavailable;
        } else {
            observation.status = Status::Available;
        }
        Ok(observation)
    })
}
