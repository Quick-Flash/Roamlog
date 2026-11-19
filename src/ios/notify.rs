//! Local notifications (friend alert areas, group membership) and incoming invitation links.
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSDictionary, NSError, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_ui_kit::{UIApplication, UIApplicationDelegate};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RoamlogNotificationDelegate"]
    struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        /// iOS hides notifications of the app in front unless asked; alerts matter then too.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }
    }
);

/// Asks once; iOS remembers the answer and later calls return at once.
pub(super) fn request_permission(mtm: MainThreadMarker) {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    if center.delegate().is_none() {
        let delegate: Retained<NotificationDelegate> =
            unsafe { msg_send![NotificationDelegate::alloc(mtm), init] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center does not retain its delegate; this one lives as long as the app.
        std::mem::forget(delegate);
    }
    let done = RcBlock::new(|_granted: Bool, _error: *mut NSError| {});
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &done,
    );
}

/// Posts now; a later post with the same `id` replaces it. Callable from any thread.
pub(super) fn post(id: i32, title: &str, text: &str) {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(text));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&id.to_string()),
        &content,
        None,
    );
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, None);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RoamlogAppDelegate"]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl UIApplicationDelegate for AppDelegate {
        #[unsafe(method(application:openURL:options:))]
        fn open_url(
            &self,
            _application: &UIApplication,
            url: &NSURL,
            _options: &NSDictionary<NSString, objc2::runtime::AnyObject>,
        ) -> Bool {
            let Some(link) = url
                .absoluteString()
                .map(|link| link.to_string())
                .filter(|link| link.starts_with("roamlog://pair/"))
            else {
                return Bool::NO;
            };
            *crate::sharing::app::PENDING_INVITE
                .lock()
                .expect("invite lock") = Some(link);
            Bool::YES
        }
    }
);

/// winit installs no application delegate, so Roamlog's only job for one is opened links.
pub(super) fn listen_for_links(mtm: MainThreadMarker) {
    let delegate: Retained<AppDelegate> = unsafe { msg_send![AppDelegate::alloc(mtm), init] };
    unsafe {
        UIApplication::sharedApplication(mtm)
            .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }
    // UIApplication does not retain its delegate; this one lives as long as the app.
    std::mem::forget(delegate);
}
