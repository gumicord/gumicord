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
docs.withCString { gumicord_ios_main($0) }
