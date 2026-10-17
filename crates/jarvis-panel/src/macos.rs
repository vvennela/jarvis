//! AppKit-only unsafe boundary. All objects stay on the process main thread;
//! the worker receives paths and a Rust control handle, never Objective-C values.
use super::Options;
use jarvis_panel::worker::{Control, State, Worker};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::AnyObject,
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSColor,
    NSEventMask, NSFloatingWindowLevel, NSFont, NSPanel, NSScreen, NSTextField,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct ActionIvars {
    control: Arc<Control>,
}
define_class!(
    // SAFETY: NSObject has no additional subclass invariants; this class has no
    // custom Drop and is MainThreadOnly. All selectors below match ObjC actions.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ActionIvars]
    struct PanelActions;
    // SAFETY: NSObjectProtocol adds no requirements beyond the NSObject subclass.
    unsafe impl NSObjectProtocol for PanelActions {}
    impl PanelActions {
        #[unsafe(method(togglePause:))]
        fn toggle_pause(&self, _sender: Option<&AnyObject>) {
            if self.ivars().control.pause_requested() { self.ivars().control.resume(); }
            else { self.ivars().control.pause(); }
        }
        #[unsafe(method(quitPanel:))]
        fn quit_panel(&self, _sender: Option<&AnyObject>) { self.ivars().control.stop(); }
    }
);
impl PanelActions {
    fn new(mtm: MainThreadMarker, control: Arc<Control>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ActionIvars { control });
        // SAFETY: Correct NSObject init signature, initialized ivars remain owned.
        unsafe { msg_send![super(this), init] }
    }
}

fn label(mtm: MainThreadMarker, text: &str, frame: NSRect, size: f64) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setFrame(frame);
    field.setFont(Some(&NSFont::systemFontOfSize(size)));
    field
}
fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

pub fn run(options: Options) -> Result<(), &'static str> {
    let mtm = MainThreadMarker::new().ok_or("Panel must run on the main thread")?;
    // Validate explicit scope before showing the panel. No user folders inferred.
    let mut worker = Worker::start(options.root, options.database)?;
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    application.finishLaunching();
    let screen = NSScreen::mainScreen(mtm).ok_or("No display is available for the panel")?;
    let frame = screen.visibleFrame();
    let width = 360.0;
    let height = 154.0;
    // Retain the panel throughout the event loop; disable release-on-close below.
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect(
            frame.origin.x + frame.size.width - width - 18.0,
            frame.origin.y + frame.size.height - height - 18.0,
            width,
            height,
        ),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: Retained<NSPanel> owns lifetime, so close must not release ownership.
    unsafe {
        panel.setReleasedWhenClosed(false);
    }
    panel.setTitle(&NSString::from_str("Jarvis awareness"));
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setHidesOnDeactivate(false);
    panel.setLevel(NSFloatingWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel.setHasShadow(true);
    panel.setOpaque(true);
    panel.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
    let view = panel
        .contentView()
        .ok_or("Panel content view unavailable")?;
    let title = label(mtm, "JARVIS", rect(18.0, 119.0, 324.0, 22.0), 13.0);
    title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
    let status = label(mtm, "Starting…", rect(18.0, 87.0, 324.0, 26.0), 19.0);
    let detail = label(
        mtm,
        "Microphone off · models idle",
        rect(18.0, 63.0, 324.0, 20.0),
        12.0,
    );
    detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let actions = PanelActions::new(mtm, Arc::clone(&worker.control));
    // SAFETY: Targets are this retained MainThreadOnly class, both selectors have
    // the expected single-object argument, and actions outlive the buttons.
    let (pause_button, quit_button) = unsafe {
        (
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Pause"),
                Some(&actions),
                Some(sel!(togglePause:)),
                mtm,
            ),
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Quit"),
                Some(&actions),
                Some(sel!(quitPanel:)),
                mtm,
            ),
        )
    };
    pause_button.setFrame(rect(16.0, 16.0, 94.0, 30.0));
    quit_button.setFrame(rect(260.0, 16.0, 82.0, 30.0));
    // Subviews remain owned by both retained handles and AppKit's hierarchy.
    view.addSubview(&title);
    view.addSubview(&status);
    view.addSubview(&detail);
    view.addSubview(&pause_button);
    view.addSubview(&quit_button);
    // Deliberately neither activate the application nor make this panel key.
    panel.orderFrontRegardless();
    let started = Instant::now();
    let mut active_observed = false;
    let mut paused_observed = false;
    let mut resumed_observed = false;
    let mut smoke_phase = 0;
    let mut last_state = None;
    let mut had_error = false;
    loop {
        let snapshot = worker.control.snapshot();
        if snapshot.state == State::Active {
            active_observed = true;
            if smoke_phase >= 2 {
                resumed_observed = true;
            }
        }
        if snapshot.state == State::Paused {
            paused_observed = true;
        }
        if snapshot.state == State::Blocked {
            had_error = true;
        }
        if last_state != Some(snapshot.state) {
            let (text, color) = match snapshot.state {
                State::Starting => ("Starting…", NSColor::secondaryLabelColor()),
                State::Active => ("● Awareness on — files", NSColor::systemGreenColor()),
                State::Pausing => ("Pausing…", NSColor::secondaryLabelColor()),
                State::Paused => ("Awareness paused", NSColor::secondaryLabelColor()),
                State::Stopping => ("Stopping…", NSColor::secondaryLabelColor()),
                State::Stopped => ("Awareness off", NSColor::secondaryLabelColor()),
                State::Blocked => ("Observer blocked", NSColor::systemRedColor()),
            };
            status.setStringValue(&NSString::from_str(text));
            status.setTextColor(Some(&color));
            last_state = Some(snapshot.state);
        }
        pause_button.setTitle(&NSString::from_str(if worker.control.pause_requested() {
            "Resume"
        } else {
            "Pause"
        }));
        pause_button.setEnabled(!matches!(
            snapshot.state,
            State::Blocked | State::Stopping | State::Stopped
        ));
        if snapshot.state == State::Blocked {
            detail.setStringValue(&NSString::from_str(snapshot.message));
        }
        if options.smoke_test {
            if smoke_phase == 0 && active_observed {
                actions.toggle_pause(sel!(togglePause:), None);
                smoke_phase = 1;
            } else if smoke_phase == 1 && paused_observed {
                actions.toggle_pause(sel!(togglePause:), None);
                smoke_phase = 2;
            } else if smoke_phase == 2 && resumed_observed {
                actions.quit_panel(sel!(quitPanel:), None);
                smoke_phase = 3;
            }
            if started.elapsed() > Duration::from_secs(8) || had_error {
                worker.control.stop();
            }
        }
        if worker.control.stop_requested() && worker.is_finished() {
            break;
        }
        if worker.is_finished() && !had_error {
            had_error = true;
            status.setStringValue(&NSString::from_str("Observer stopped unexpectedly"));
        }
        autoreleasepool(|_| {
            let expiration = NSDate::dateWithTimeIntervalSinceNow(0.05);
            // SAFETY: NSDefaultRunLoopMode is Apple's immutable global NSString.
            let mode = unsafe { NSDefaultRunLoopMode };
            if let Some(event) = application.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&expiration),
                mode,
                true,
            ) {
                application.sendEvent(&event);
            }
            application.updateWindows();
        });
    }
    panel.orderOut(None);
    worker.join()?;
    if options.smoke_test {
        println!(
            "{{\"panel_created\":true,\"active_observed\":{active_observed},\"paused_observed\":{paused_observed},\"resumed_observed\":{resumed_observed},\"microphone_active\":false}}"
        );
        if had_error || !active_observed || !paused_observed || !resumed_observed {
            return Err("Panel smoke test did not complete its observer lifecycle");
        }
    }
    Ok(())
}
