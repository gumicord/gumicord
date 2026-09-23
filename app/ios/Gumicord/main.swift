// The entry point. Rust (winit) owns the lifecycle and calls
// UIApplicationMain itself; a Swift @main entry would call it first and
// make winit refuse to run. LiveContainer likewise jumps to the guest's
// main, which is expected to call UIApplicationMain as usual.

import Foundation

let docs = NSSearchPathForDirectoriesInDomains(
    .documentDirectory,
    .userDomainMask,
    true
).first ?? ""
let caches = NSSearchPathForDirectoriesInDomains(
    .cachesDirectory,
    .userDomainMask,
    true
).first ?? ""
docs.withCString { docsPtr in
    caches.withCString { cachesPtr in
        gumicord_ios_main(docsPtr, cachesPtr)
    }
}
