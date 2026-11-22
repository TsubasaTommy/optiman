//! Settings window (AppKit). It runs only while open: closing it ends the process, so nothing
//! stays resident.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSApplication,
    NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSBitmapImageFileType, NSBorderType,
    NSButton, NSCell, NSControlStateValueOff, NSControlStateValueOn, NSLineBreakMode, NSModalResponseOK, NSOpenPanel,
    NSScrollView, NSSlider, NSTableColumn, NSTableView, NSTableViewDataSource, NSTextField, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSDictionary, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use crate::app::{self, Settings, ui_text as text};
use crate::config::{Config, EFFORT_RANGE, Quality};

const WIDTH: f64 = 560.0;
const HEIGHT: f64 = 624.0;

/// Frame from top-left based coordinates (AppKit's origin is bottom-left).
fn frame(x: f64, top: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, HEIGHT - top - h), NSSize::new(w, h))
}

pub struct Ivars {
    config_path: PathBuf,
    include: RefCell<Vec<PathBuf>>,
    exclude: RefCell<Vec<PathBuf>>,
    effort: Cell<u8>,
    quality: Cell<Quality>,
    window: OnceCell<Retained<NSWindow>>,
    include_table: OnceCell<Retained<NSTableView>>,
    exclude_table: OnceCell<Retained<NSTableView>>,
    effort_label: OnceCell<Retained<NSTextField>>,
    quality_note: OnceCell<Retained<NSTextField>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Controller does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSApplicationDelegate for Controller {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.build_window();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            true
        }
    }

    unsafe impl NSTableViewDataSource for Controller {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, table: &NSTableView) -> NSInteger {
            self.list_for(table).map_or(0, |l| l.borrow().len() as NSInteger)
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn object_value(&self, table: &NSTableView, _column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<AnyObject>> {
            self.row_value(table, row)
        }
    }

    impl Controller {
        #[unsafe(method(effortChanged:))]
        fn effort_changed(&self, sender: &NSSlider) {
            let value = sender.integerValue().clamp(i64::from(*EFFORT_RANGE.start()) as NSInteger, i64::from(*EFFORT_RANGE.end()) as NSInteger) as u8;
            self.ivars().effort.set(value);
            if let Some(label) = self.ivars().effort_label.get() {
                label.setStringValue(&NSString::from_str(&app::effort_label(value)));
            }
        }

        #[unsafe(method(qualityChanged:))]
        fn quality_changed(&self, sender: &NSButton) {
            let Some(&q) = usize::try_from(sender.tag()).ok().and_then(|i| Quality::ALL.get(i)) else { return };
            self.ivars().quality.set(q);
            if let Some(note) = self.ivars().quality_note.get() {
                note.setStringValue(&NSString::from_str(q.describe()));
            }
        }

        #[unsafe(method(addInclude:))]
        fn add_include(&self, _sender: Option<&AnyObject>) {
            self.add_folders(true);
        }

        #[unsafe(method(removeInclude:))]
        fn remove_include(&self, _sender: Option<&AnyObject>) {
            self.remove_selected(true);
        }

        #[unsafe(method(addExclude:))]
        fn add_exclude(&self, _sender: Option<&AnyObject>) {
            self.add_folders(false);
        }

        #[unsafe(method(removeExclude:))]
        fn remove_exclude(&self, _sender: Option<&AnyObject>) {
            self.remove_selected(false);
        }

        #[unsafe(method(save:))]
        fn save(&self, _sender: Option<&AnyObject>) {
            let settings = Settings {
                effort: self.ivars().effort.get(),
                quality: self.ivars().quality.get(),
                include: self.ivars().include.borrow().clone(),
                exclude: self.ivars().exclude.borrow().clone(),
            };
            match app::save_settings(&self.ivars().config_path, &settings) {
                Ok(_) => {
                    self.alert(text::SAVED, text::SAVED_DETAIL);
                    NSApplication::sharedApplication(self.mtm()).terminate(None);
                }
                Err(e) => self.alert(text::SAVE_FAILED, &format!("{e:#}")),
            }
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker, config_path: &Path, cfg: &Config) -> Retained<Self> {
        let s = Settings::from_config(cfg);
        let this = Self::alloc(mtm).set_ivars(Ivars {
            config_path: config_path.to_path_buf(),
            include: RefCell::new(s.include),
            exclude: RefCell::new(s.exclude),
            effort: Cell::new(s.effort),
            quality: Cell::new(s.quality),
            window: OnceCell::new(),
            include_table: OnceCell::new(),
            exclude_table: OnceCell::new(),
            effort_label: OnceCell::new(),
            quality_note: OnceCell::new(),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn list_for(&self, table: &NSTableView) -> Option<&RefCell<Vec<PathBuf>>> {
        let iv = self.ivars();
        if iv.include_table.get().is_some_and(|t| std::ptr::eq(&**t, table)) {
            Some(&iv.include)
        } else if iv.exclude_table.get().is_some_and(|t| std::ptr::eq(&**t, table)) {
            Some(&iv.exclude)
        } else {
            None
        }
    }

    fn row_value(&self, table: &NSTableView, row: NSInteger) -> Option<Retained<AnyObject>> {
        let list = self.list_for(table)?.borrow();
        let path = list.get(usize::try_from(row).ok()?)?;
        Some(Retained::into_super(Retained::into_super(NSString::from_str(&crate::config::contract_tilde(path)))))
    }

    fn table(&self, include: bool) -> Option<&Retained<NSTableView>> {
        if include { self.ivars().include_table.get() } else { self.ivars().exclude_table.get() }
    }

    fn add_folders(&self, include: bool) {
        let panel = NSOpenPanel::openPanel(self.mtm());
        panel.setCanChooseDirectories(true);
        panel.setCanChooseFiles(false);
        panel.setAllowsMultipleSelection(true);
        panel.setPrompt(Some(&NSString::from_str("追加")));
        if panel.runModal() != NSModalResponseOK {
            return;
        }
        let urls = panel.URLs();
        let list = if include { &self.ivars().include } else { &self.ivars().exclude };
        for i in 0..urls.count() {
            if let Some(path) = urls.objectAtIndex(i).path() {
                let p = PathBuf::from(path.to_string());
                if !list.borrow().contains(&p) {
                    list.borrow_mut().push(p);
                }
            }
        }
        if let Some(t) = self.table(include) {
            t.reloadData();
        }
    }

    fn remove_selected(&self, include: bool) {
        let Some(table) = self.table(include) else { return };
        let row = table.selectedRow();
        let list = if include { &self.ivars().include } else { &self.ivars().exclude };
        if let Ok(i) = usize::try_from(row)
            && i < list.borrow().len()
        {
            list.borrow_mut().remove(i);
            table.reloadData();
        }
    }

    fn alert(&self, title: &str, body: &str) {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(body));
        alert.runModal();
    }

    fn label(&self, text: &str, f: NSRect, content: &NSView) -> Retained<NSTextField> {
        let l = NSTextField::wrappingLabelWithString(&NSString::from_str(text), self.mtm());
        l.setFrame(f);
        content.addSubview(&l);
        l
    }

    fn button(&self, title: &str, action: objc2::runtime::Sel, f: NSRect, content: &NSView) -> Retained<NSButton> {
        let target: &AnyObject = self.as_ref();
        let b = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(action), self.mtm())
        };
        b.setFrame(f);
        content.addSubview(&b);
        b
    }

    fn folder_list(&self, top: f64, content: &NSView) -> Retained<NSTableView> {
        let mtm = self.mtm();
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame(20.0, top, 410.0, 110.0));
        scroll.setHasVerticalScroller(true);
        scroll.setBorderType(NSBorderType::BezelBorder);
        let table = NSTableView::initWithFrame(
            NSTableView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(410.0, 110.0)),
        );
        let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str("path"));
        column.setWidth(400.0);
        // Long paths keep both ends visible: "/Volumes/…/2026/旅行".
        if let Ok(cell) = column.dataCell().downcast::<NSCell>() {
            cell.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        }
        table.addTableColumn(&column);
        table.setHeaderView(None);
        table.setUsesAlternatingRowBackgroundColors(true);
        unsafe { table.setDataSource(Some(ProtocolObject::from_ref(self))) };
        scroll.setDocumentView(Some(&table));
        content.addSubview(&scroll);
        table
    }

    fn build_window(&self) {
        let mtm = self.mtm();
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT)),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(text::TITLE));
        let content = window.contentView().expect("content view");

        self.label(text::INTRO, frame(20.0, 16.0, 520.0, 52.0), &content);

        self.label(text::EFFORT, frame(20.0, 84.0, 110.0, 20.0), &content);
        let target: &AnyObject = self.as_ref();
        let slider = unsafe {
            NSSlider::sliderWithValue_minValue_maxValue_target_action(
                f64::from(self.ivars().effort.get()),
                f64::from(*EFFORT_RANGE.start()),
                f64::from(*EFFORT_RANGE.end()),
                Some(target),
                Some(sel!(effortChanged:)),
                mtm,
            )
        };
        slider.setNumberOfTickMarks(i64::from(*EFFORT_RANGE.end() - *EFFORT_RANGE.start() + 1) as NSInteger);
        slider.setAllowsTickMarkValuesOnly(true);
        slider.setFrame(frame(130.0, 80.0, 300.0, 28.0));
        content.addSubview(&slider);
        let effort_label =
            self.label(&app::effort_label(self.ivars().effort.get()), frame(440.0, 84.0, 110.0, 20.0), &content);
        let _ = self.ivars().effort_label.set(effort_label);
        self.label(text::EFFORT_HINT, frame(130.0, 110.0, 400.0, 18.0), &content);

        self.label(text::QUALITY, frame(20.0, 146.0, 110.0, 20.0), &content);
        // Radio buttons that share a superview and an action form one group.
        let widths = [150.0, 70.0, 70.0, 70.0];
        let mut x = 130.0;
        for (i, (q, w)) in Quality::ALL.iter().zip(widths).enumerate() {
            let radio = unsafe {
                NSButton::radioButtonWithTitle_target_action(
                    &NSString::from_str(q.name()),
                    Some(target),
                    Some(sel!(qualityChanged:)),
                    mtm,
                )
            };
            radio.setTag(i as NSInteger);
            radio.setState(if *q == self.ivars().quality.get() {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            radio.setFrame(frame(x, 144.0, w, 22.0));
            content.addSubview(&radio);
            x += w + 10.0;
        }
        let note = self.label(self.ivars().quality.get().describe(), frame(130.0, 172.0, 410.0, 36.0), &content);
        let _ = self.ivars().quality_note.set(note);

        self.label(text::INCLUDE, frame(20.0, 224.0, 400.0, 20.0), &content);
        let include = self.folder_list(246.0, &content);
        let _ = self.ivars().include_table.set(include);
        self.button(text::ADD, sel!(addInclude:), frame(440.0, 246.0, 100.0, 30.0), &content);
        self.button(text::REMOVE, sel!(removeInclude:), frame(440.0, 280.0, 100.0, 30.0), &content);

        self.label(text::EXCLUDE, frame(20.0, 372.0, 400.0, 20.0), &content);
        let exclude = self.folder_list(394.0, &content);
        let _ = self.ivars().exclude_table.set(exclude);
        self.button(text::ADD, sel!(addExclude:), frame(440.0, 394.0, 100.0, 30.0), &content);
        self.button(text::REMOVE, sel!(removeExclude:), frame(440.0, 428.0, 100.0, 30.0), &content);

        if let Some(hint) = super::image::viewer_hint() {
            self.label(&hint, frame(20.0, 520.0, 520.0, 36.0), &content);
        }

        self.button(text::CANCEL, sel!(cancel:), frame(330.0, 574.0, 100.0, 32.0), &content);
        let save = self.button(text::SAVE, sel!(save:), frame(440.0, 574.0, 100.0, 32.0), &content);
        save.setKeyEquivalent(&NSString::from_str("\r"));

        window.center();
        window.makeKeyAndOrderFront(None);
        for include in [true, false] {
            if let Some(t) = self.table(include) {
                t.reloadData();
            }
        }
        if let Some(path) = std::env::var_os("OPTIMAN_UI_SNAPSHOT") {
            window.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }).as_deref());
            content.layoutSubtreeIfNeeded();
            window.displayIfNeeded();
            snapshot(&unsafe { content.superview() }.unwrap_or_else(|| content.clone()), &path.to_string_lossy());
            NSApplication::sharedApplication(mtm).terminate(None);
        }
        let _ = self.ivars().window.set(window);
    }
}

/// Renders the window content to a PNG (layout checks without screen-recording permission).
fn snapshot(view: &NSView, path: &str) {
    let bounds = view.bounds();
    if let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(bounds) {
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        if let Some(png) =
            unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
        {
            png.writeToFile_atomically(&NSString::from_str(path), true);
        }
    }
}

/// Opens the settings window and returns when it is closed.
pub fn run_settings(config_path: &Path) -> Result<()> {
    let mtm = MainThreadMarker::new().context("設定画面はメインスレッドで開く必要があります")?;
    let cfg = Config::load(config_path).unwrap_or_default();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let controller = Controller::new(mtm, config_path, &cfg);
    app.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
    Ok(())
}
