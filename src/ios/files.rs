//! UIKit document picker (open and export), share sheet and the app's Settings page.
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSArray, NSDictionary, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_ui_kit::{
    UIActivityViewController, UIApplication, UIApplicationOpenSettingsURLString,
    UIDocumentPickerDelegate, UIDocumentPickerViewController, UIViewController,
};
use objc2_uniform_type_identifiers::UTType;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// Picked file path, `None` when cancelled.
type Picked = Box<dyn FnOnce(Option<PathBuf>)>;

pub(super) struct Ivars {
    done: RefCell<Option<Picked>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RoamlogPickerDelegate"]
    #[ivars = Ivars]
    pub(super) struct PickerDelegate;

    unsafe impl NSObjectProtocol for PickerDelegate {}

    unsafe impl UIDocumentPickerDelegate for PickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn did_pick(&self, _picker: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            let path = urls
                .firstObject()
                .and_then(|url| url.path())
                .map(|path| PathBuf::from(path.to_string()));
            self.finish(path);
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn cancelled(&self, _picker: &UIDocumentPickerViewController) {
            self.finish(None);
        }
    }
);

impl PickerDelegate {
    fn finish(&self, path: Option<PathBuf>) {
        if let Some(done) = self.ivars().done.borrow_mut().take() {
            done(path);
        }
        // Picker holds its delegate weakly; release the one kept alive for it.
        DELEGATE.with_borrow_mut(|d| d.take());
    }
}

thread_local! {
    /// Delegate of the picker on screen, kept alive until it answers.
    static DELEGATE: RefCell<Option<Retained<PickerDelegate>>> = const { RefCell::new(None) };
}

fn top_controller(mtm: MainThreadMarker) -> Result<Retained<UIViewController>, String> {
    #[allow(deprecated)]
    let window = UIApplication::sharedApplication(mtm)
        .keyWindow()
        .ok_or("No window to show picker over")?;
    let mut controller = window.rootViewController().ok_or("No view controller")?;
    while let Some(presented) = controller.presentedViewController() {
        controller = presented;
    }
    Ok(controller)
}

fn present_picker(
    mtm: MainThreadMarker,
    picker: Retained<UIDocumentPickerViewController>,
    done: impl FnOnce(Option<PathBuf>) + 'static,
) -> Result<(), String> {
    let delegate = PickerDelegate::alloc(mtm).set_ivars(Ivars {
        done: RefCell::new(Some(Box::new(done))),
    });
    let delegate: Retained<PickerDelegate> = unsafe { msg_send![super(delegate), init] };
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    DELEGATE.with_borrow_mut(|d| *d = Some(delegate));
    top_controller(mtm)?.presentViewController_animated_completion(&picker, true, None);
    Ok(())
}

/// Lets the user pick any file; `done` gets a private copy's path, or `None` when cancelled.
pub(super) fn open(
    mtm: MainThreadMarker,
    done: impl FnOnce(Option<PathBuf>) + 'static,
) -> Result<(), String> {
    let data = UTType::typeWithIdentifier(&NSString::from_str("public.data"))
        .ok_or("No public.data type")?;
    let types = NSArray::from_retained_slice(&[data]);
    let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(
        UIDocumentPickerViewController::alloc(mtm),
        &types,
        true,
    );
    present_picker(mtm, picker, done)
}

/// Lets the user save a copy of `file` where they choose; `done(true)` once saved.
pub(super) fn export(
    mtm: MainThreadMarker,
    file: &Path,
    done: impl FnOnce(bool) + 'static,
) -> Result<(), String> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
    let urls = NSArray::from_retained_slice(&[url]);
    let picker = UIDocumentPickerViewController::initForExportingURLs_asCopy(
        UIDocumentPickerViewController::alloc(mtm),
        &urls,
        true,
    );
    present_picker(mtm, picker, move |path| done(path.is_some()))
}

/// System share sheet with `text`.
pub(super) fn share(mtm: MainThreadMarker, text: &str) -> Result<(), String> {
    let item: Retained<objc2::runtime::AnyObject> = NSString::from_str(text).into();
    let items = NSArray::from_retained_slice(&[item]);
    let sheet = unsafe {
        UIActivityViewController::initWithActivityItems_applicationActivities(
            UIActivityViewController::alloc(mtm),
            &items,
            None,
        )
    };
    top_controller(mtm)?.presentViewController_animated_completion(&sheet, true, None);
    Ok(())
}

/// Opens Roamlog's page in the Settings app (location permission lives there).
pub(super) fn open_settings(mtm: MainThreadMarker) -> Result<(), String> {
    let url = NSURL::URLWithString(unsafe { UIApplicationOpenSettingsURLString })
        .ok_or("No Settings URL")?;
    unsafe {
        UIApplication::sharedApplication(mtm).openURL_options_completionHandler(
            &url,
            &NSDictionary::new(),
            None,
        );
    }
    Ok(())
}
