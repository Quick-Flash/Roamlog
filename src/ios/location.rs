//! CoreLocation watch: one `CLLocationManager` per watch, delivering on the main thread.
use crate::mobile::Reading;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_core_location::{
    CLAuthorizationStatus, CLLocation, CLLocationManager, CLLocationManagerDelegate,
    kCLLocationAccuracyBest, kCLLocationAccuracyNearestTenMeters,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use std::time::Duration;

pub(super) enum Update {
    Fix(Reading),
    Failed(String),
}

pub(super) struct Ivars {
    callback: Box<dyn Fn(Update)>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RoamlogLocationDelegate"]
    #[ivars = Ivars]
    pub(super) struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl CLLocationManagerDelegate for Delegate {
        #[unsafe(method(locationManager:didUpdateLocations:))]
        fn did_update_locations(
            &self,
            _manager: &CLLocationManager,
            locations: &NSArray<CLLocation>,
        ) {
            for location in locations.iter() {
                (self.ivars().callback)(Update::Fix(reading(&location)));
            }
        }

        #[unsafe(method(locationManager:didFailWithError:))]
        fn did_fail(&self, _manager: &CLLocationManager, error: &NSError) {
            // kCLErrorLocationUnknown: no fix yet; CoreLocation keeps trying.
            if error.code() == 0 {
                return;
            }
            (self.ivars().callback)(Update::Failed(error.localizedDescription().to_string()));
        }
    }
);

/// Running location updates; dropping it stops them.
pub(super) struct Watch {
    manager: Retained<CLLocationManager>,
    _delegate: Retained<Delegate>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            self.manager.stopUpdatingLocation();
            self.manager.setDelegate(None);
        }
    }
}

/// Starts updates, asking for When In Use permission first if never asked. `background` keeps
/// updates (and the app) running while backgrounded, with the system location indicator.
/// `precise` is GPS at every fix for recording; otherwise ~10 m fixes every 20 m moved.
pub(super) fn watch(
    mtm: MainThreadMarker,
    background: bool,
    precise: bool,
    callback: impl Fn(Update) + 'static,
) -> Watch {
    let delegate = Delegate::alloc(mtm).set_ivars(Ivars {
        callback: Box::new(callback),
    });
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
    let manager = unsafe { CLLocationManager::new() };
    unsafe {
        manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        if precise {
            manager.setDesiredAccuracy(kCLLocationAccuracyBest);
        } else {
            manager.setDesiredAccuracy(kCLLocationAccuracyNearestTenMeters);
            manager.setDistanceFilter(20.0);
        }
        if background {
            manager.setAllowsBackgroundLocationUpdates(true);
            manager.setPausesLocationUpdatesAutomatically(false);
            manager.setShowsBackgroundLocationIndicator(true);
        }
        manager.requestWhenInUseAuthorization();
        manager.startUpdatingLocation();
    }
    Watch {
        manager,
        _delegate: delegate,
    }
}

fn status() -> CLAuthorizationStatus {
    unsafe { CLLocationManager::new().authorizationStatus() }
}

/// User refused location, or a policy forbids it; only Settings can change that.
pub(super) fn denied() -> bool {
    let status = status();
    status == CLAuthorizationStatus::Denied || status == CLAuthorizationStatus::Restricted
}

pub(super) fn authorized() -> bool {
    let status = status();
    status == CLAuthorizationStatus::AuthorizedWhenInUse
        || status == CLAuthorizationStatus::AuthorizedAlways
}

fn reading(location: &CLLocation) -> Reading {
    unsafe {
        let coordinate = location.coordinate();
        let timestamp = location.timestamp();
        let accuracy = location.horizontalAccuracy();
        let speed = location.speed();
        let course = location.course();
        // Negative values mean "invalid" in CoreLocation.
        Reading {
            lat: coordinate.latitude,
            lon: coordinate.longitude,
            accuracy_m: (accuracy >= 0.0).then_some(accuracy as f32),
            speed: (speed >= 0.0).then_some(speed as f32),
            bearing: (course >= 0.0).then_some(course as f32),
            time_ms: (timestamp.timeIntervalSince1970() * 1000.0) as u64,
            age: Duration::from_secs_f64((-timestamp.timeIntervalSinceNow()).max(0.0)),
        }
    }
}
