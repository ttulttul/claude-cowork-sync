import CoworkMergeCore
import SwiftUI

struct ContentView: View {
    @ObservedObject var viewModel: MergeViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Claude Cowork Sync")
                .font(.title2.weight(.semibold))
            Text("Run cowork merges with a native macOS UI while keeping the existing Python merge engine.")
                .foregroundStyle(.secondary)

            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    GroupBox("Workspace") {
                        pathField(
                            title: "Repository Root",
                            text: workspaceBinding(for: \.workspacePath),
                            chooseDirectory: true
                        )
                    }

                    GroupBox("Sources") {
                        pathField(title: "Profile A", text: formStringBinding(for: \.profileA), chooseDirectory: true)
                        pathField(title: "Profile B", text: formStringBinding(for: \.profileB), chooseDirectory: true)
                        textField(title: "Merge From (user@host)", text: formStringBinding(for: \.mergeFrom))
                        textField(title: "Remote Profile Path", text: formStringBinding(for: \.remoteProfilePath))
                        pathField(title: "Output Profile", text: formStringBinding(for: \.outputProfile), chooseDirectory: true)
                    }

                    GroupBox("Browser State") {
                        toggleField(title: "Auto Export Browser State", value: formBoolBinding(for: \.autoExportBrowserState))
                        toggleField(title: "Headless Browser State", value: formBoolBinding(for: \.headlessBrowserState))
                        toggleField(title: "Skip Browser State", value: formBoolBinding(for: \.skipBrowserState))
                        toggleField(title: "Skip IndexedDB", value: formBoolBinding(for: \.skipIndexedDB))
                        pathField(title: "Browser State A JSON", text: formStringBinding(for: \.browserStateA), chooseDirectory: false)
                        pathField(title: "Browser State B JSON", text: formStringBinding(for: \.browserStateB), chooseDirectory: false)
                        pathField(
                            title: "Browser State Output JSON",
                            text: formStringBinding(for: \.browserStateOutput),
                            chooseDirectory: false
                        )
                    }

                    GroupBox("Options") {
                        Picker("Base Source", selection: formStringBinding(for: \.baseSource)) {
                            Text("a").tag("a")
                            Text("b").tag("b")
                        }
                        Picker("Log Level", selection: formStringBinding(for: \.logLevel)) {
                            Text("DEBUG").tag("DEBUG")
                            Text("INFO").tag("INFO")
                            Text("WARNING").tag("WARNING")
                            Text("ERROR").tag("ERROR")
                        }
                        textField(title: "Parallel Remote", text: formStringBinding(for: \.parallelRemote))
                        textField(title: "Parallel Local", text: formStringBinding(for: \.parallelLocal))
                        toggleField(title: "Include vm_bundles", value: formBoolBinding(for: \.includeVmBundles))
                        toggleField(title: "Include cache dirs", value: formBoolBinding(for: \.includeCacheDirs))
                        toggleField(title: "Apply after merge", value: formBoolBinding(for: \.apply))
                        toggleField(title: "Force output overwrite", value: formBoolBinding(for: \.force))
                        toggleField(
                            title: "Include sensitive .claude/.credentials.json",
                            value: formBoolBinding(for: \.includeSensitiveClaudeCredentials)
                        )
                    }

                    if !viewModel.validationErrors.isEmpty {
                        GroupBox("Validation") {
                            ForEach(viewModel.validationErrors, id: \.self) { error in
                                Text("• \(error)")
                                    .foregroundStyle(.red)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }

                    GroupBox("Output") {
                        ScrollView {
                            Text(viewModel.outputLog.isEmpty ? "No output yet." : viewModel.outputLog)
                                .font(.system(.body, design: .monospaced))
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .textSelection(.enabled)
                        }
                        .frame(minHeight: 200)
                    }
                }
            }

            HStack {
                Text(viewModel.statusText)
                    .foregroundStyle(viewModel.isRunning ? .blue : .secondary)
                Spacer()
                Button("Cancel") {
                    viewModel.cancel()
                }
                .disabled(!viewModel.isRunning)
                Button("Run Merge") {
                    viewModel.runMerge()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(viewModel.isRunning || !viewModel.validationErrors.isEmpty)
            }
        }
        .padding()
    }

    private func workspaceBinding(for keyPath: ReferenceWritableKeyPath<MergeViewModel, String>) -> Binding<String> {
        Binding(
            get: { viewModel[keyPath: keyPath] },
            set: { viewModel[keyPath: keyPath] = $0 }
        )
    }

    private func formStringBinding(for keyPath: WritableKeyPath<MergeFormData, String>) -> Binding<String> {
        Binding(
            get: { viewModel.form[keyPath: keyPath] },
            set: { viewModel.form[keyPath: keyPath] = $0 }
        )
    }

    private func formBoolBinding(for keyPath: WritableKeyPath<MergeFormData, Bool>) -> Binding<Bool> {
        Binding(
            get: { viewModel.form[keyPath: keyPath] },
            set: { viewModel.form[keyPath: keyPath] = $0 }
        )
    }

    @ViewBuilder
    private func textField(title: String, text: Binding<String>) -> some View {
        HStack {
            Text(title)
                .frame(width: 220, alignment: .leading)
            TextField(title, text: text)
                .textFieldStyle(.roundedBorder)
        }
    }

    @ViewBuilder
    private func pathField(title: String, text: Binding<String>, chooseDirectory: Bool) -> some View {
        HStack {
            Text(title)
                .frame(width: 220, alignment: .leading)
            TextField(title, text: text)
                .textFieldStyle(.roundedBorder)
            Button("Browse") {
                if chooseDirectory {
                    if let selectedPath = DirectoryPicker.pickDirectory() {
                        text.wrappedValue = selectedPath
                    }
                } else if let selectedPath = DirectoryPicker.pickFile() {
                    text.wrappedValue = selectedPath
                }
            }
        }
    }

    @ViewBuilder
    private func toggleField(title: String, value: Binding<Bool>) -> some View {
        Toggle(title, isOn: value)
    }
}
