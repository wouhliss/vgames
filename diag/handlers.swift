import CoreServices
import Foundation

// usage: handlers <scheme> <bundle id>; exit 0 when the id handles the scheme.
let args = CommandLine.arguments
let scheme = args[1] as CFString
let id = args[2]
let all = (LSCopyAllHandlersForURLScheme(scheme)?.takeRetainedValue() as? [String]) ?? []
let def = LSCopyDefaultHandlerForURLScheme(scheme)?.takeRetainedValue() as String?
print("handlers=\(all) default=\(def ?? "nil")")
exit(all.contains(id) ? 0 : 1)
