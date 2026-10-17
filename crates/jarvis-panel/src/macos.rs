//! AppKit-only unsafe boundary. All objects stay on the process main thread;
//! the worker receives paths and a Rust control handle, never Objective-C values.
use super::Options;
use jarvis_panel::{
    chat::{self, ConversationWorker},
    speech::{self, SpeechWorker},
    wakeworker::{self, WakeWorker},
    worker::{Control, State, Worker},
};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::AnyObject,
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSColor,
    NSEventMask, NSFloatingWindowLevel, NSFont, NSPanel, NSScreen, NSScrollView, NSTextField,
    NSTextView, NSWindowCollectionBehavior, NSWindowStyleMask,
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
    chat: Arc<chat::Control>,
    speech: Arc<speech::Control>,
    voice: Arc<wakeworker::Control>,
    input: Retained<NSTextField>,
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
        fn quit_panel(&self, _sender: Option<&AnyObject>) {
            self.ivars().voice.shutdown();
            self.ivars().speech.shutdown();
            self.ivars().chat.stop();
            self.ivars().control.stop();
        }
        #[unsafe(method(sendQuestion:))]
        fn send_question(&self, _sender: Option<&AnyObject>) {
            let question = self.ivars().input.stringValue().to_string();
            if self.ivars().chat.submit(question).is_ok() {
                self.ivars().input.setStringValue(&NSString::from_str(""));
            }
        }
        #[unsafe(method(cancelQuestion:))]
        fn cancel_question(&self, _sender: Option<&AnyObject>) {
            self.ivars().voice.suppress(true);
            self.ivars().speech.stop_speaking();
            self.ivars().chat.cancel();
        }
        #[unsafe(method(readAloud:))]
        fn read_aloud(&self, _sender: Option<&AnyObject>) {
            let reply = self.ivars().chat.snapshot();
            if reply.state == chat::State::Reply {
                let mut end = reply.text.len().min(speech::MAX_TEXT_BYTES);
                while !reply.text.is_char_boundary(end) { end -= 1; }
                let _ = self.ivars().speech.speak(reply.text[..end].to_owned());
            }
        }
        #[unsafe(method(stopSpeaking:))]
        fn stop_speaking(&self, _sender: Option<&AnyObject>) { self.ivars().speech.stop_speaking(); }
        #[unsafe(method(toggleRecording:))]
        fn toggle_recording(&self, _sender: Option<&AnyObject>) {
            self.ivars().voice.set_enabled(!self.ivars().voice.enabled());
        }
    }
);
impl PanelActions {
    fn new(
        mtm: MainThreadMarker,
        control: Arc<Control>,
        chat: Arc<chat::Control>,
        speech: Arc<speech::Control>,
        voice: Arc<wakeworker::Control>,
        input: Retained<NSTextField>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ActionIvars {
            control,
            chat,
            speech,
            voice,
            input,
        });
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
    let mut worker = Worker::start(options.root, options.database.clone())?;
    let mut conversation = ConversationWorker::start(options.database)?;
    let mut speech = SpeechWorker::start()?;
    let mut voice = WakeWorker::from_env()?;
    // Smoke mode never enables microphone hardware.
    voice.control.set_enabled(!options.smoke_test);
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    application.finishLaunching();
    let screen = NSScreen::mainScreen(mtm).ok_or("No display is available for the panel")?;
    let frame = screen.visibleFrame();
    let width = 480.0;
    let height = 480.0;
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
    let title = label(mtm, "JARVIS", rect(18.0, 445.0, 444.0, 22.0), 13.0);
    title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
    let status = label(mtm, "Starting…", rect(18.0, 414.0, 444.0, 26.0), 19.0);
    let detail = label(
        mtm,
        "Microphone off · models idle",
        rect(18.0, 390.0, 444.0, 20.0),
        12.0,
    );
    detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let chat_status = label(
        mtm,
        "Preparing conversation…",
        rect(18.0, 362.0, 444.0, 20.0),
        12.0,
    );
    let scroll =
        NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(18.0, 124.0, 444.0, 230.0));
    scroll.setHasVerticalScroller(true);
    let response = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 424.0, 230.0));
    response.setEditable(false);
    response.setSelectable(true);
    response.setRichText(false);
    response.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    response.setVerticallyResizable(true);
    response.setHorizontallyResizable(false);
    response.setMinSize(NSSize::new(424.0, 230.0));
    response.setMaxSize(NSSize::new(424.0, 1_000_000.0));
    scroll.setDocumentView(Some(&response));
    let input = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(18.0, 82.0, 350.0, 30.0));
    input.setPlaceholderString(Some(&NSString::from_str("Ask Jarvis about your work…")));
    input.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    let actions = PanelActions::new(
        mtm,
        Arc::clone(&worker.control),
        Arc::clone(&conversation.control),
        Arc::clone(&speech.control),
        Arc::clone(&voice.control),
        input.clone(),
    );
    // SAFETY: This retained target implements sendQuestion: with the standard
    // single-object action signature and outlives the entry field.
    unsafe {
        input.setTarget(Some(&actions));
        input.setAction(Some(sel!(sendQuestion:)));
    }
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
    quit_button.setFrame(rect(380.0, 16.0, 82.0, 30.0));
    // SAFETY: The retained target declares these exact single-object selectors.
    let (send_button, cancel_button) = unsafe {
        (
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Ask"),
                Some(&actions),
                Some(sel!(sendQuestion:)),
                mtm,
            ),
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Cancel"),
                Some(&actions),
                Some(sel!(cancelQuestion:)),
                mtm,
            ),
        )
    };
    send_button.setFrame(rect(378.0, 82.0, 84.0, 30.0));
    cancel_button.setFrame(rect(118.0, 16.0, 130.0, 30.0));
    // SAFETY: Both selectors are declared on the retained main-thread target.
    let (read_button, stop_speech_button) = unsafe {
        (
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Read aloud"),
                Some(&actions),
                Some(sel!(readAloud:)),
                mtm,
            ),
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Stop speaking"),
                Some(&actions),
                Some(sel!(stopSpeaking:)),
                mtm,
            ),
        )
    };
    read_button.setFrame(rect(16.0, 48.0, 110.0, 28.0));
    stop_speech_button.setFrame(rect(132.0, 48.0, 140.0, 28.0));
    // SAFETY: The retained target implements this single-object selector.
    let record_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Mute microphone"),
            Some(&actions),
            Some(sel!(toggleRecording:)),
            mtm,
        )
    };
    record_button.setFrame(rect(282.0, 48.0, 180.0, 28.0));
    // Subviews remain owned by both retained handles and AppKit's hierarchy.
    view.addSubview(&title);
    view.addSubview(&status);
    view.addSubview(&detail);
    view.addSubview(&pause_button);
    view.addSubview(&quit_button);
    view.addSubview(&chat_status);
    view.addSubview(&scroll);
    view.addSubview(&input);
    view.addSubview(&send_button);
    view.addSubview(&cancel_button);
    view.addSubview(&read_button);
    view.addSubview(&stop_speech_button);
    view.addSubview(&record_button);
    // Deliberately neither activate the application nor make this panel key.
    panel.orderFrontRegardless();
    let started = Instant::now();
    let mut active_observed = false;
    let mut paused_observed = false;
    let mut resumed_observed = false;
    let mut smoke_phase = 0;
    let mut last_state = None;
    let mut had_error = false;
    let mut chat_revision = None;
    let mut voice_reply_revision = None;
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
        let chat_snapshot = conversation.control.snapshot();
        if chat_revision != Some(chat_snapshot.revision) {
            response.setString(&NSString::from_str(&chat_snapshot.text));
            let message = match chat_snapshot.state {
                chat::State::Starting => "Preparing conversation…",
                chat::State::Ready => "Ready for your question",
                chat::State::Thinking => "Thinking…",
                chat::State::Cancelling => "Cancelling…",
                chat::State::Reply if chat_snapshot.routing_degraded => {
                    "Reply ready · direct model route"
                }
                chat::State::Reply => "Reply ready",
                chat::State::Cancelled => "Cancelled · ready for another question",
                chat::State::Unavailable => "Add your model key to enable conversation",
                chat::State::Failed => "Ready to retry",
                chat::State::Stopped => "Conversation stopped",
            };
            chat_status.setStringValue(&NSString::from_str(message));
            chat_revision = Some(chat_snapshot.revision);
        }
        if voice_reply_revision.is_some_and(|revision| revision != chat_snapshot.revision)
            && chat_snapshot.state == chat::State::Reply
            && !conversation.control.busy()
        {
            let mut end = chat_snapshot.text.len().min(speech::MAX_TEXT_BYTES);
            while !chat_snapshot.text.is_char_boundary(end) {
                end -= 1;
            }
            let _ = speech.control.speak(chat_snapshot.text[..end].to_owned());
            voice_reply_revision = None;
        }
        if voice_reply_revision.is_some_and(|revision| revision != chat_snapshot.revision)
            && matches!(
                chat_snapshot.state,
                chat::State::Failed | chat::State::Cancelled | chat::State::Unavailable
            )
        {
            voice_reply_revision = None;
        }
        // A voice command owns the next response. Ambient decode text never reaches chat.
        if let Some(command) = voice.control.take_command() {
            input.setStringValue(&NSString::from_str(&command));
            if conversation.control.submit(command).is_ok() {
                voice_reply_revision = Some(chat_snapshot.revision);
            }
        }
        voice
            .control
            .suppress(speech.control.busy() || conversation.control.busy());
        let voice_snapshot = voice.control.snapshot();
        record_button.setTitle(&NSString::from_str(if voice.control.enabled() {
            "Mute microphone"
        } else {
            "Enable wake listening"
        }));
        record_button.setToolTip(Some(&NSString::from_str(voice_snapshot.message)));
        record_button.setEnabled(voice_snapshot.state != wakeworker::State::Stopped);
        input.setEnabled(!conversation.control.busy());
        send_button.setEnabled(conversation.control.available() && !conversation.control.busy());
        cancel_button.setEnabled(
            conversation.control.busy()
                || matches!(
                    voice_snapshot.state,
                    wakeworker::State::CommandListening | wakeworker::State::Decoding
                ),
        );
        read_button.setEnabled(
            chat_snapshot.state == chat::State::Reply
                && speech.control.available()
                && !speech.control.busy(),
        );
        stop_speech_button.setEnabled(speech.control.busy());
        if snapshot.state != State::Blocked {
            let message = if speech.control.busy() {
                "Speaking…"
            } else if conversation.control.busy() {
                "Thinking…"
            } else {
                voice_snapshot.message
            };
            detail.setStringValue(&NSString::from_str(message));
            let color = if matches!(
                voice_snapshot.state,
                wakeworker::State::WakeListening | wakeworker::State::CommandListening
            ) {
                NSColor::systemGreenColor()
            } else {
                NSColor::secondaryLabelColor()
            };
            detail.setTextColor(Some(&color));
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
                voice.control.shutdown();
                speech.control.shutdown();
                conversation.control.stop();
                worker.control.stop();
            }
        }
        if worker.control.stop_requested()
            && worker.is_finished()
            && conversation.is_finished()
            && speech.is_finished()
            && voice.is_finished()
        {
            break;
        }
        if worker.is_finished() && !worker.control.stop_requested() && !had_error {
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
    conversation.join()?;
    speech.join()?;
    voice.join()?;
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
