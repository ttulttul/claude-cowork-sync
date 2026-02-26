import AppKit
import Foundation

enum DirectoryPicker {
    static func pickDirectory() -> String? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Select"
        return panel.runModal() == .OK ? panel.url?.path : nil
    }

    static func pickFile() -> String? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Select"
        return panel.runModal() == .OK ? panel.url?.path : nil
    }
}
