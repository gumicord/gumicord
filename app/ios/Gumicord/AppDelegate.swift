// Entry point. Xcode owns the app bundle; Rust owns everything past
// this call. The call blocks on the main thread running winit's loop,
// which is where iOS requires the event loop to live.

import UIKit

@main
class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        // Files-visible Documents directory; empty string falls back to
        // the platform default on the Rust side.
        let docs = NSSearchPathForDirectoriesInDomains(
            .documentDirectory,
            .userDomainMask,
            true
        ).first ?? ""
        docs.withCString { gumicord_ios_main($0) }
        return true
    }
}
