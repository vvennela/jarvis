//! AppKit-only unsafe boundary. All objects stay on the process main thread;
//! the worker receives paths and a Rust control handle, never Objective-C values.
use super::Options;
use jarvis_panel::{
    chat::{self, ConversationWorker},
    codingworker::{self, CodingWorker},
    osworker::{self, OsWorker},
    speech::{self, SpeechWorker},
    tasks::{self, TaskWorker},
    voicehandoff::{VoiceHandoff, dispatch_question_with_coding},
    wakeworker::{self, WakeWorker},
    worker::{Control, State, Worker},
};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{AnyClass, AnyObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSColor,
    NSEventMask, NSFloatingWindowLevel, NSFont, NSGlassEffectView, NSGlassEffectViewStyle, NSImage,
    NSPanel, NSProgressIndicator, NSProgressIndicatorStyle, NSScreen, NSScrollView, NSSound,
    NSStatusBar, NSTextField, NSTextView, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString,
};
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

struct ActionIvars {
    control: Arc<Control>,
    os: Arc<osworker::Control>,
    chat: Arc<chat::Control>,
    tasks: Arc<tasks::Control>,
    coding: Option<Arc<codingworker::Control>>,
    action_error: Cell<Option<&'static str>>,
    speech: Arc<speech::Control>,
    voice: Arc<wakeworker::Control>,
    input: Retained<NSTextField>,
    expanded: Cell<bool>,
    visible: Cell<bool>,
    wake_opened: Cell<bool>,
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
        #[unsafe(method(togglePanel:))]
        fn toggle_panel(&self, _sender: Option<&AnyObject>) {
            let visible = !self.ivars().visible.get();
            self.ivars().visible.set(visible);
            self.ivars().wake_opened.set(false);
            if visible { self.ivars().expanded.set(false); }
        }
        #[unsafe(method(dismissPanel:))]
        fn dismiss_panel(&self, _sender: Option<&AnyObject>) {
            self.ivars().visible.set(false);
            self.ivars().wake_opened.set(false);
        }
        #[unsafe(method(toggleDetails:))]
        fn toggle_details(&self, _sender: Option<&AnyObject>) {
            self.ivars().expanded.set(!self.ivars().expanded.get());
            self.ivars().wake_opened.set(false);
        }
        #[unsafe(method(togglePause:))]
        fn toggle_pause(&self, _sender: Option<&AnyObject>) {
            if self.ivars().control.pause_requested() { self.ivars().control.resume(); self.ivars().os.resume(); }
            else { self.ivars().control.pause(); self.ivars().os.pause(); }
        }
        #[unsafe(method(quitPanel:))]
        fn quit_panel(&self, _sender: Option<&AnyObject>) {
            eprintln!("Jarvis: quit control invoked");
            self.ivars().os.stop();
            self.ivars().voice.shutdown();
            self.ivars().speech.shutdown();
            self.ivars().chat.stop();
            self.ivars().tasks.shutdown();
            if let Some(coding)=&self.ivars().coding {coding.shutdown();}
            self.ivars().control.stop();
        }
        #[unsafe(method(sendQuestion:))]
        fn send_question(&self, _sender: Option<&AnyObject>) {
            let question = self.ivars().input.stringValue().to_string();
            match dispatch_question_with_coding(question, &self.ivars().chat, &self.ivars().tasks, self.ivars().coding.as_deref()) {
                Ok(_) => {
                    self.ivars().action_error.set(None);
                    self.ivars().input.setStringValue(&NSString::from_str(""));
                }
                Err(error) => self.ivars().action_error.set(Some(error)),
            }
        }
        #[unsafe(method(cancelQuestion:))]
        fn cancel_question(&self, _sender: Option<&AnyObject>) {
            self.ivars().voice.suppress(true);
            self.ivars().speech.stop_speaking();
            self.ivars().chat.cancel();
            self.ivars().tasks.cancel();
            if let Some(coding)=&self.ivars().coding {coding.cancel();}
        }
        #[unsafe(method(openDraft:))]
        fn open_draft(&self, _sender: Option<&AnyObject>) {
            self.ivars().action_error.set(
                self.ivars().tasks.request_review().err().map(|_| "The result is not ready to open yet.")
            );
        }
        #[unsafe(method(openCodeReview:))]
        fn open_code_review(&self, _sender: Option<&AnyObject>) {
            self.ivars().action_error.set(self.ivars().coding.as_ref().and_then(|coding|coding.request_review().err()).map(|_|"The coding result is not ready to open yet."));
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
    fn reveal_for_wake(&self) {
        self.ivars().expanded.set(false);
        self.ivars().visible.set(true);
        self.ivars().wake_opened.set(true);
    }
    fn new(
        mtm: MainThreadMarker,
        control: Arc<Control>,
        os: Arc<osworker::Control>,
        conversation: (
            Arc<chat::Control>,
            Arc<tasks::Control>,
            Option<Arc<codingworker::Control>>,
        ),
        speech: Arc<speech::Control>,
        voice: Arc<wakeworker::Control>,
        input: Retained<NSTextField>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ActionIvars {
            control,
            os,
            chat: conversation.0,
            tasks: conversation.1,
            coding: conversation.2,
            action_error: Cell::new(None),
            speech,
            voice,
            input,
            expanded: Cell::new(false),
            visible: Cell::new(false),
            wake_opened: Cell::new(false),
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
    let mut os = if options.smoke_test {
        OsWorker::disabled()
    } else {
        OsWorker::start(options.database.clone())?
    };
    let database = if options.database.is_absolute() {
        options.database.clone()
    } else {
        std::env::current_dir()
            .map_err(|_| "Current directory unavailable")?
            .join(&options.database)
    };
    let review_directory = database
        .parent()
        .ok_or("Context database has no parent directory")?
        .join("reviews");
    let mut tasks = TaskWorker::start(database, review_directory)?;
    let (mut coding, coding_error) = if options.smoke_test {
        (None, None)
    } else {
        match CodingWorker::from_env() {
            Ok(worker) => (worker, None),
            Err(error) => (None, Some(error)),
        }
    };
    let mut conversation = ConversationWorker::start_with_workers(
        options.database,
        Arc::clone(&tasks.control),
        coding.as_ref().map(|worker| Arc::clone(&worker.control)),
    )?;
    let mut speech = SpeechWorker::start()?;
    let mut voice = WakeWorker::from_env()?;
    // Smoke mode never enables microphone hardware.
    voice.control.set_enabled(!options.smoke_test);
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    application.finishLaunching();
    let screen = NSScreen::mainScreen(mtm).ok_or("No display is available for the panel")?;
    let frame = screen.visibleFrame();
    let width = 360.0;
    let height = 140.0;
    let expanded_height = 440.0;
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
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, width, height));
    // Runtime availability preserves the frosted-glass fallback on older macOS.
    let native_glass = AnyClass::get(c"NSGlassEffectView").is_some();
    let (glass, frosted) = if native_glass {
        let glass = NSGlassEffectView::initWithFrame(
            NSGlassEffectView::alloc(mtm),
            rect(0.0, 0.0, width, height),
        );
        glass.setStyle(NSGlassEffectViewStyle::Regular);
        glass.setCornerRadius(18.0);
        glass.setContentView(Some(&view));
        panel.setContentView(Some(&glass));
        (Some(glass), None)
    } else {
        let frosted = NSVisualEffectView::initWithFrame(
            NSVisualEffectView::alloc(mtm),
            rect(0.0, 0.0, width, height),
        );
        frosted.setMaterial(NSVisualEffectMaterial::Popover);
        frosted.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        frosted.setState(NSVisualEffectState::Active);
        frosted.setWantsLayer(true);
        if let Some(layer) = frosted.layer() {
            layer.setCornerRadius(18.0);
            layer.setMasksToBounds(true);
        }
        frosted.addSubview(&view);
        panel.setContentView(Some(&frosted));
        (None, Some(frosted))
    };
    let header = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, width, height));
    let details = NSView::initWithFrame(
        NSView::alloc(mtm),
        rect(0.0, 0.0, width, expanded_height - height),
    );
    details.setHidden(true);
    view.addSubview(&header);
    view.addSubview(&details);
    let title = label(mtm, "Jarvis", rect(18.0, 110.0, 200.0, 18.0), 11.0);
    title.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let status = label(mtm, "Starting…", rect(18.0, 274.0, 324.0, 18.0), 11.0);
    let detail = label(
        mtm,
        "Microphone off · models idle",
        rect(34.0, 82.0, 308.0, 24.0),
        15.0,
    );
    detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let os_status = label(
        mtm,
        "Starting app awareness…",
        rect(18.0, 254.0, 324.0, 18.0),
        12.0,
    );
    let chat_status = label(
        mtm,
        "Preparing conversation…",
        rect(18.0, 234.0, 324.0, 18.0),
        12.0,
    );
    let task_status = label(mtm, "Drafts · ready", rect(18.0, 214.0, 324.0, 18.0), 11.0);
    task_status.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let scroll =
        NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(18.0, 108.0, 324.0, 100.0));
    scroll.setHasVerticalScroller(true);
    scroll.setDrawsBackground(false);
    let response = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 304.0, 100.0));
    response.setEditable(false);
    response.setDrawsBackground(false);
    response.setSelectable(true);
    response.setRichText(false);
    response.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    response.setVerticallyResizable(true);
    response.setHorizontallyResizable(false);
    response.setMinSize(NSSize::new(304.0, 100.0));
    response.setMaxSize(NSSize::new(304.0, 1_000_000.0));
    scroll.setDocumentView(Some(&response));
    let input = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(18.0, 70.0, 250.0, 30.0));
    input.setPlaceholderString(Some(&NSString::from_str("Ask Jarvis about your work…")));
    input.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    let actions = PanelActions::new(
        mtm,
        Arc::clone(&worker.control),
        Arc::clone(&os.control),
        (
            Arc::clone(&conversation.control),
            Arc::clone(&tasks.control),
            coding.as_ref().map(|worker| Arc::clone(&worker.control)),
        ),
        Arc::clone(&speech.control),
        Arc::clone(&voice.control),
        input.clone(),
    );
    actions.ivars().action_error.set(coding_error);
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
    pause_button.setFrame(rect(16.0, 8.0, 74.0, 28.0));
    quit_button.setFrame(rect(274.0, 8.0, 68.0, 28.0));
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
    send_button.setFrame(rect(274.0, 70.0, 68.0, 30.0));
    cancel_button.setFrame(rect(94.0, 8.0, 78.0, 28.0));
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
    read_button.setFrame(rect(16.0, 38.0, 110.0, 28.0));
    stop_speech_button.setFrame(rect(132.0, 38.0, 140.0, 28.0));
    // SAFETY: The retained target implements this single-object selector.
    let record_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Mute microphone"),
            Some(&actions),
            Some(sel!(toggleRecording:)),
            mtm,
        )
    };
    record_button.setFrame(rect(14.0, 9.0, 60.0, 23.0));
    record_button.setBordered(false);
    record_button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    // SAFETY: The retained target declares this standard single-object action.
    let expand_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Expand"),
            Some(&actions),
            Some(sel!(toggleDetails:)),
            mtm,
        )
    };
    expand_button.setFrame(rect(278.0, 9.0, 68.0, 23.0));
    expand_button.setBordered(false);
    expand_button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    // SAFETY: This retained main-thread target implements openDraft:.
    let review_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Open result"),
            Some(&actions),
            Some(sel!(openDraft:)),
            mtm,
        )
    };
    review_button.setFrame(rect(96.0, 9.0, 100.0, 23.0));
    review_button.setBordered(false);
    review_button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    review_button.setEnabled(false);
    review_button.setHidden(true);
    let review_initially_disabled = !review_button.isEnabled();
    // SAFETY: Retained main-thread target implements this single-object selector.
    let code_review_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Open code"),
            Some(&actions),
            Some(sel!(openCodeReview:)),
            mtm,
        )
    };
    code_review_button.setFrame(rect(200.0, 9.0, 76.0, 23.0));
    code_review_button.setBordered(false);
    code_review_button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    code_review_button.setEnabled(false);
    code_review_button.setHidden(true);
    let preview =
        NSTextField::wrappingLabelWithString(&NSString::from_str("Here when you need me."), mtm);
    preview.setFrame(rect(18.0, 44.0, 324.0, 36.0));
    preview.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    preview.setMaximumNumberOfLines(2);
    preview.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let activity = label(mtm, "●", rect(18.0, 84.0, 12.0, 18.0), 9.0);
    activity.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let spinner = NSProgressIndicator::initWithFrame(
        NSProgressIndicator::alloc(mtm),
        rect(16.0, 86.0, 16.0, 16.0),
    );
    spinner.setStyle(NSProgressIndicatorStyle::Spinning);
    spinner.setIndeterminate(true);
    spinner.setDisplayedWhenStopped(false);
    spinner.setHidden(true);
    let spinner_indeterminate =
        spinner.isIndeterminate() && spinner.style() == NSProgressIndicatorStyle::Spinning;
    let mut spinner_active = false;
    let mut spinner_busy_visible = false;
    let mut spinner_idle_hidden = false;
    // SAFETY: Target owns this exact single-object selector for the entire UI loop.
    let dismiss_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("×"),
            Some(&actions),
            Some(sel!(dismissPanel:)),
            mtm,
        )
    };
    dismiss_button.setFrame(rect(320.0, 108.0, 24.0, 22.0));
    dismiss_button.setBordered(false);
    dismiss_button.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    dismiss_button.setToolTip(Some(&NSString::from_str(
        "Dismiss · Jarvis keeps listening",
    )));
    header.addSubview(&dismiss_button);
    header.addSubview(&activity);
    header.addSubview(&spinner);
    header.addSubview(&title);
    header.addSubview(&detail);
    header.addSubview(&preview);
    header.addSubview(&record_button);
    header.addSubview(&expand_button);
    header.addSubview(&review_button);
    header.addSubview(&code_review_button);
    details.addSubview(&status);
    details.addSubview(&pause_button);
    details.addSubview(&quit_button);
    details.addSubview(&os_status);
    details.addSubview(&chat_status);
    details.addSubview(&task_status);
    details.addSubview(&scroll);
    details.addSubview(&input);
    details.addSubview(&send_button);
    details.addSubview(&cancel_button);
    details.addSubview(&read_button);
    details.addSubview(&stop_speech_button);
    let compact_width = panel.frame().size.width;
    let compact_height = panel.frame().size.height;
    let translucent = !panel.isOpaque();
    let nonactivating = panel
        .styleMask()
        .contains(NSWindowStyleMask::NonactivatingPanel);
    let mut expanded = false;
    let mut expanded_observed = false;
    let mut collapsed_observed = false;
    // The status item owns the idle presence; the panel starts completely hidden.
    let status_bar = NSStatusBar::systemStatusBar();
    let status_item = status_bar.statusItemWithLength(28.0);
    let menu_button = status_item
        .button(mtm)
        .ok_or("Menu bar control unavailable")?;
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("waveform"),
        Some(&NSString::from_str("Jarvis")),
    ) {
        image.setTemplate(true);
        menu_button.setImage(Some(&image));
    } else {
        menu_button.setTitle(&NSString::from_str("J"));
    }
    // SAFETY: The retained target implements the action and outlives the item.
    unsafe {
        menu_button.setTarget(Some(&actions));
        menu_button.setAction(Some(sel!(togglePanel:)));
    }
    menu_button.setToolTip(Some(&NSString::from_str("Jarvis · click to open")));
    let initial_hidden = !panel.isVisible();
    let menu_bar_present = status_item.isVisible();
    let mut visible = false;
    let mut wake_revealed = false;
    let mut dismissed_hidden = false;
    let mut workers_alive_after_dismiss = false;
    let initial_voice_enabled = voice.control.enabled();
    let mut voice_unchanged_after_dismiss = false;
    let mut menu_toggle_revealed = false;
    let mut interaction_revealed = false;
    let mut audible_wake_count = 0;
    let mut wake_idle_since: Option<Instant> = None;
    let started = Instant::now();
    let mut next_voice_diagnostic = Duration::ZERO;
    let mut active_observed = false;
    let mut paused_observed = false;
    let mut resumed_observed = false;
    let mut smoke_phase = 0;
    let mut last_state = None;
    let mut had_error = false;
    let mut chat_revision = None;
    let mut task_revision = None;
    let mut task_notice = false;
    let mut coding_revision = None;
    let mut coding_notice = false;
    let mut voice_handoff = VoiceHandoff::new();
    loop {
        if expanded != actions.ivars().expanded.get() {
            expanded = actions.ivars().expanded.get();
            let target_height = if expanded { expanded_height } else { height };
            let current = panel.frame();
            panel.setFrame_display(
                rect(
                    current.origin.x,
                    current.origin.y + current.size.height - target_height,
                    width,
                    target_height,
                ),
                true,
            );
            if let Some(glass) = &glass {
                glass.setFrame(rect(0.0, 0.0, width, target_height));
            }
            if let Some(frosted) = &frosted {
                frosted.setFrame(rect(0.0, 0.0, width, target_height));
            }
            view.setFrame(rect(0.0, 0.0, width, target_height));
            header.setFrame(rect(0.0, target_height - height, width, height));
            details.setHidden(!expanded);
            expand_button.setTitle(&NSString::from_str(if expanded {
                "Less"
            } else {
                "Details"
            }));
            if expanded {
                expanded_observed =
                    panel.frame().size.height == expanded_height && !details.isHidden();
            } else {
                collapsed_observed = panel.frame().size.height == height && details.isHidden();
            }
        }
        if visible != actions.ivars().visible.get() {
            visible = actions.ivars().visible.get();
            // Neither path activates the application or makes the panel key.
            if visible {
                panel.orderFrontRegardless();
            } else {
                panel.orderOut(None);
            }
        }
        let snapshot = worker.control.snapshot();
        let os_snapshot = os.control.snapshot();
        os_status.setStringValue(&NSString::from_str(os_snapshot.message));
        if snapshot.state == State::Active {
            active_observed = true;
            if paused_observed {
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
        pause_button.setEnabled(!matches!(snapshot.state, State::Stopping | State::Stopped));
        if snapshot.state == State::Blocked {
            detail.setStringValue(&NSString::from_str(snapshot.message));
        }
        let task_snapshot = tasks.control.snapshot();
        if task_revision != Some(task_snapshot.revision) {
            task_status.setStringValue(&NSString::from_str(&task_snapshot.text));
            task_status.setToolTip(Some(&NSString::from_str(&task_snapshot.text)));
            task_notice = task_snapshot.state != tasks::State::Idle;
            if task_snapshot.state == tasks::State::ReadyForReview && !actions.ivars().visible.get()
            {
                actions.reveal_for_wake();
            }
            task_revision = Some(task_snapshot.revision);
        }
        let draft_ready =
            task_snapshot.state == tasks::State::ReadyForReview && !tasks.control.busy();
        review_button.setEnabled(draft_ready);
        review_button.setHidden(task_snapshot.state != tasks::State::ReadyForReview);
        let coding_snapshot = coding.as_ref().map(|worker| worker.control.snapshot());
        if let Some(snapshot) = &coding_snapshot {
            if coding_revision != Some(snapshot.revision) {
                coding_notice = snapshot.state != tasks::State::Idle;
                if snapshot.state == tasks::State::ReadyForReview && !actions.ivars().visible.get()
                {
                    actions.reveal_for_wake();
                }
                coding_revision = Some(snapshot.revision);
            }
            if coding_notice {
                task_status.setStringValue(&NSString::from_str(&snapshot.text));
                task_status.setToolTip(Some(&NSString::from_str(&snapshot.text)));
            }
        }
        let code_ready = coding_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.state == tasks::State::ReadyForReview)
            && coding.as_ref().is_some_and(|worker| !worker.control.busy());
        code_review_button.setEnabled(code_ready);
        code_review_button.setHidden(!code_ready);
        let chat_snapshot = conversation.control.snapshot();
        if chat_revision != Some(chat_snapshot.revision) {
            if matches!(
                chat_snapshot.state,
                chat::State::Reply | chat::State::Failed
            ) {
                task_notice = false;
                coding_notice = false;
            }
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
                chat::State::Unavailable => "Conversation needs attention",
                chat::State::Failed => "Ready to retry",
                chat::State::Stopped => "Conversation stopped",
            };
            chat_status.setStringValue(&NSString::from_str(message));
            chat_revision = Some(chat_snapshot.revision);
        }
        if let Some(command) = voice_handoff.tick_with_workers(
            &voice.control,
            &conversation.control,
            &speech.control,
            &tasks.control,
            coding.as_ref().map(|worker| worker.control.as_ref()),
        ) {
            input.setStringValue(&NSString::from_str(&command));
            if !interaction_revealed {
                actions.reveal_for_wake();
            }
            interaction_revealed = true;
        }
        if voice_handoff.take_microphone_reveal() {
            actions.reveal_for_wake();
            wake_idle_since = None;
        }
        let voice_snapshot = voice.control.snapshot();
        if voice_snapshot.diagnostics.wake_matches > audible_wake_count {
            audible_wake_count = voice_snapshot.diagnostics.wake_matches;
            if !options.smoke_test
                && let Some(chime) = NSSound::soundNamed(&NSString::from_str("Tink"))
            {
                let _ = chime.play();
            }
        }
        let working = conversation.control.busy()
            || tasks.control.busy()
            || coding.as_ref().is_some_and(|worker| worker.control.busy())
            || speech.control.busy()
            || speech.control.snapshot().state == speech::State::Starting
            || matches!(
                voice_snapshot.state,
                wakeworker::State::Loading
                    | wakeworker::State::Starting
                    | wakeworker::State::Decoding
                    | wakeworker::State::CommandReady
                    | wakeworker::State::CommandListening
                    | wakeworker::State::Suppressed
            )
            || (options.smoke_test && smoke_phase == 1);
        if working != spinner_active {
            // SAFETY: Native animation receives nil sender and stays owned by
            // this retained main-thread AppKit view throughout the event loop.
            unsafe {
                if working {
                    spinner.startAnimation(None);
                } else {
                    spinner.stopAnimation(None);
                }
            }
            spinner.setHidden(!working);
            activity.setHidden(working);
            spinner_active = working;
        }
        if options.smoke_test {
            if spinner_active && panel.isVisible() && !spinner.isHidden() && activity.isHidden() {
                spinner_busy_visible = true;
            }
            if spinner_busy_visible && !spinner_active && spinner.isHidden() && !activity.isHidden()
            {
                spinner_idle_hidden = true;
            }
        }
        // An explicit, bounded ten-minute diagnostic session, never ambient text.
        if options.voice_diagnostics
            && started.elapsed() < Duration::from_secs(600)
            && started.elapsed() >= next_voice_diagnostic
        {
            eprintln!("{}", voice_snapshot.diagnostic_line());
            next_voice_diagnostic = started.elapsed() + Duration::from_secs(2);
        }
        if voice_snapshot.state == wakeworker::State::WakeListening {
            interaction_revealed = false;
        }
        if voice_snapshot.state == wakeworker::State::CommandListening && !interaction_revealed {
            actions.reveal_for_wake();
            interaction_revealed = true;
        }
        let voice_diagnostics = &voice_snapshot.diagnostics;
        let microphone_name = voice_diagnostics
            .input
            .as_ref()
            .map(|input| input.name.as_str())
            .unwrap_or("Starting microphone");
        let microphone_details = format!(
            "{}\nInput: {}\nLevel: {:.4} · Peak: {:.4}\nDecodes: {} · Wake matches: {} · Last decode: {} ms",
            voice_snapshot.message,
            microphone_name,
            voice_diagnostics.last_rms,
            voice_diagnostics.last_peak,
            voice_diagnostics.decode_count,
            voice_diagnostics.wake_matches,
            voice_diagnostics.last_decode_ms,
        );
        menu_button.setToolTip(Some(&NSString::from_str(&microphone_details)));
        record_button.setTitle(&NSString::from_str(if voice.control.enabled() {
            "Mute"
        } else {
            "Listen"
        }));
        record_button.setToolTip(Some(&NSString::from_str(&microphone_details)));
        record_button.setEnabled(voice_snapshot.state != wakeworker::State::Stopped);
        input.setEnabled(!conversation.control.busy());
        send_button.setEnabled(
            !conversation.control.busy()
                && (conversation.control.available() || !tasks.control.busy()),
        );
        cancel_button.setEnabled(
            conversation.control.busy()
                || tasks.control.busy()
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
        let blocked =
            snapshot.state == State::Blocked || os_snapshot.state == osworker::State::Blocked;
        let message = if blocked {
            "Awareness needs attention"
        } else if speech.control.busy() {
            "Speaking…"
        } else if speech.control.snapshot().state == speech::State::Failed {
            "Couldn't play audio"
        } else if conversation.control.busy() {
            "Thinking…"
        } else if chat_snapshot.state == chat::State::Failed
            && !task_notice
            && !matches!(
                voice_snapshot.state,
                wakeworker::State::CommandListening | wakeworker::State::Decoding
            )
        {
            "Request failed · ready to retry"
        } else if coding_notice
            && coding_snapshot.is_some()
            && !matches!(
                voice_snapshot.state,
                wakeworker::State::CommandListening | wakeworker::State::Decoding
            )
        {
            match coding_snapshot.as_ref().map(|snapshot| snapshot.state) {
                Some(tasks::State::Running) => "Codex is working in the background…",
                Some(tasks::State::ReadyForReview) => "Coding changes ready for review",
                Some(tasks::State::Blocked | tasks::State::Failed) => "Coding needs attention",
                Some(tasks::State::Cancelled) => "Coding cancelled",
                _ => "Ready when you are",
            }
        } else if task_notice
            && !matches!(
                voice_snapshot.state,
                wakeworker::State::CommandListening | wakeworker::State::Decoding
            )
        {
            match task_snapshot.state {
                tasks::State::Running => "Drafting in the background…",
                tasks::State::ReadyForReview => "Draft ready for review",
                tasks::State::Blocked => "Draft needs attention",
                tasks::State::Failed => "Draft could not finish",
                tasks::State::Cancelled => "Draft cancelled",
                tasks::State::Idle | tasks::State::Stopped => "Ready when you are",
            }
        } else {
            match voice_snapshot.state {
                wakeworker::State::WakeListening => "Listening for Hey Jarvis",
                wakeworker::State::CommandListening => "I'm listening…",
                wakeworker::State::Decoding => "One moment…",
                wakeworker::State::Loading | wakeworker::State::Starting => "Starting…",
                wakeworker::State::Muted => "Microphone muted",
                wakeworker::State::Failed => "Microphone needs attention",
                wakeworker::State::Stopped => "Microphone off",
                wakeworker::State::CommandReady => "Request heard",
                wakeworker::State::Suppressed => "One moment…",
            }
        };
        detail.setStringValue(&NSString::from_str(message));
        detail.setTextColor(Some(&NSColor::labelColor()));
        let activity_color = if blocked
            || voice_snapshot.state == wakeworker::State::Failed
            || chat_snapshot.state == chat::State::Failed
            || (task_notice
                && matches!(
                    task_snapshot.state,
                    tasks::State::Blocked | tasks::State::Failed
                )) {
            NSColor::systemOrangeColor()
        } else if voice.control.enabled() {
            NSColor::systemGreenColor()
        } else {
            NSColor::tertiaryLabelColor()
        };
        activity.setTextColor(Some(&activity_color));
        let preview_text = if let Some(notice) = voice_handoff.microphone_notice()
            && !conversation.control.busy()
            && !matches!(
                voice_snapshot.state,
                wakeworker::State::CommandReady
                    | wakeworker::State::CommandListening
                    | wakeworker::State::Decoding
            ) {
            notice.to_owned()
        } else if let Some(error) = actions.ivars().action_error.get() {
            error.to_owned()
        } else if coding_notice && !conversation.control.busy() {
            coding_snapshot
                .as_ref()
                .map(|snapshot| snapshot.text.chars().take(180).collect::<String>())
                .unwrap_or_default()
        } else if task_notice && !conversation.control.busy() {
            task_snapshot.text.chars().take(180).collect::<String>()
        } else if blocked {
            "Open Details to check access.".to_owned()
        } else if matches!(
            chat_snapshot.state,
            chat::State::Starting
                | chat::State::Thinking
                | chat::State::Unavailable
                | chat::State::Failed
        ) {
            chat_snapshot.text.chars().take(180).collect::<String>()
        } else if chat_snapshot.state == chat::State::Reply && !chat_snapshot.text.is_empty() {
            chat_snapshot
                .text
                .chars()
                .take(180)
                .collect::<String>()
                .replace('\n', " ")
        } else if voice_snapshot.state == wakeworker::State::Failed {
            voice_snapshot.message.to_owned()
        } else if voice.control.enabled() && voice_diagnostics.input.is_some() {
            format!("Listening with {microphone_name}.")
        } else {
            "Here when you need me.".to_owned()
        };
        preview.setStringValue(&NSString::from_str(&preview_text));
        // Only cards opened by wake recognition auto-dismiss. Explicit menu or
        // Details interaction keeps the card open until the user dismisses it.
        if visible
            && actions.ivars().wake_opened.get()
            && !expanded
            && !interaction_revealed
            && !conversation.control.busy()
            && !speech.control.busy()
            && matches!(
                voice_snapshot.state,
                wakeworker::State::WakeListening | wakeworker::State::Decoding
            )
        {
            let idle_since = wake_idle_since.get_or_insert_with(Instant::now);
            if idle_since.elapsed() >= Duration::from_secs(8) {
                actions.dismiss_panel(sel!(dismissPanel:), None);
            }
        } else {
            wake_idle_since = None;
        }
        if options.smoke_test {
            if smoke_phase == 0 && active_observed {
                actions.reveal_for_wake();
                smoke_phase = 1;
            } else if smoke_phase == 1 && panel.isVisible() {
                wake_revealed = true;
                actions.toggle_details(sel!(toggleDetails:), None);
                smoke_phase = 2;
            } else if smoke_phase == 2 && expanded_observed {
                actions.toggle_details(sel!(toggleDetails:), None);
                smoke_phase = 3;
            } else if smoke_phase == 3 && collapsed_observed {
                actions.dismiss_panel(sel!(dismissPanel:), None);
                smoke_phase = 4;
            } else if smoke_phase == 4 && !panel.isVisible() {
                dismissed_hidden = true;
                workers_alive_after_dismiss =
                    !worker.control.stop_requested() && !worker.is_finished();
                voice_unchanged_after_dismiss = voice.control.enabled() == initial_voice_enabled;
                actions.toggle_panel(sel!(togglePanel:), None);
                smoke_phase = 5;
            } else if smoke_phase == 5 && panel.isVisible() {
                menu_toggle_revealed = true;
                actions.toggle_pause(sel!(togglePause:), None);
                smoke_phase = 6;
            } else if smoke_phase == 6 && paused_observed {
                actions.toggle_pause(sel!(togglePause:), None);
                smoke_phase = 7;
            } else if smoke_phase == 7 && resumed_observed {
                actions.quit_panel(sel!(quitPanel:), None);
                smoke_phase = 8;
            }
            if started.elapsed() > Duration::from_secs(8) || had_error {
                os.control.stop();
                voice.control.shutdown();
                speech.control.shutdown();
                conversation.control.stop();
                tasks.control.shutdown();
                if let Some(coding) = &coding {
                    coding.control.shutdown();
                }
                worker.control.stop();
            }
        }
        if worker.control.stop_requested()
            && worker.is_finished()
            && os.is_finished()
            && conversation.is_finished()
            && speech.is_finished()
            && voice.is_finished()
            && tasks.is_finished()
            && coding.as_ref().is_none_or(|worker| worker.is_finished())
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
    // SAFETY: The retained native indicator is still alive on the main thread.
    unsafe {
        spinner.stopAnimation(None);
    }
    panel.orderOut(None);
    status_bar.removeStatusItem(&status_item);
    worker.join()?;
    os.join()?;
    conversation.join()?;
    speech.join()?;
    voice.join()?;
    tasks.join()?;
    if let Some(coding) = &mut coding {
        coding.join()?;
    }
    let task_worker_stopped = tasks.is_finished();
    if options.smoke_test {
        println!(
            "{{\"panel_created\":true,\"active_observed\":{active_observed},\"paused_observed\":{paused_observed},\"resumed_observed\":{resumed_observed},\"microphone_active\":false,\"compact_width\":{compact_width},\"compact_height\":{compact_height},\"expanded_observed\":{expanded_observed},\"collapsed_observed\":{collapsed_observed},\"translucent\":{translucent},\"native_glass\":{native_glass},\"nonactivating\":{nonactivating},\"initial_hidden\":{initial_hidden},\"menu_bar_present\":{menu_bar_present},\"wake_revealed\":{wake_revealed},\"dismissed_hidden\":{dismissed_hidden},\"workers_alive_after_dismiss\":{workers_alive_after_dismiss},\"voice_unchanged_after_dismiss\":{voice_unchanged_after_dismiss},\"menu_toggle_revealed\":{menu_toggle_revealed},\"review_initially_disabled\":{review_initially_disabled},\"task_worker_stopped\":{task_worker_stopped},\"spinner_indeterminate\":{spinner_indeterminate},\"spinner_busy_visible\":{spinner_busy_visible},\"spinner_idle_hidden\":{spinner_idle_hidden}}}"
        );
        if had_error
            || !active_observed
            || !paused_observed
            || !resumed_observed
            || !expanded_observed
            || !collapsed_observed
            || !initial_hidden
            || !menu_bar_present
            || !wake_revealed
            || !dismissed_hidden
            || !workers_alive_after_dismiss
            || !voice_unchanged_after_dismiss
            || !menu_toggle_revealed
            || !review_initially_disabled
            || !task_worker_stopped
            || !spinner_indeterminate
            || !spinner_busy_visible
            || !spinner_idle_hidden
        {
            return Err("Panel smoke test did not complete its observer lifecycle");
        }
    }
    Ok(())
}
