import SwiftUI

@main
struct CoworkMergeApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView(viewModel: MergeViewModel())
                .frame(minWidth: 900, minHeight: 700)
        }
    }
}
