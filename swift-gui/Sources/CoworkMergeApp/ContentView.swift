import CoworkMergeCore
import SwiftUI

struct ContentView: View {
    @ObservedObject var viewModel: MergeViewModel

    var body: some View {
        GeometryReader { proxy in
            Group {
                if proxy.size.width < 1_180 {
                    VStack(spacing: 12) {
                        configurationPane
                            .frame(maxHeight: proxy.size.height * 0.56)
                        executionPane
                    }
                } else {
                    HSplitView {
                        configurationPane
                            .frame(minWidth: 460, maxWidth: .infinity)
                        executionPane
                            .frame(minWidth: 460, maxWidth: .infinity)
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(16)
        }
    }

    private var configurationPane: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                sectionCard("Workflow Setup") {
                    inputRow(
                        title: "Repository Root",
                        hint: "Set this to the repo containing pyproject.toml so uv can resolve cowork-merge."
                    ) {
                        pathInput(
                            text: workspaceBinding(for: \.workspacePath),
                            browseAction: {
                                if let selectedPath = DirectoryPicker.pickDirectory() {
                                    viewModel.workspacePath = selectedPath
                                }
                            }
                        )
                    }

                    inputRow(
                        title: "Secondary Source Type",
                        hint: "Choose local profile merge or remote host pull."
                    ) {
                        Picker(
                            "Source Type",
                            selection: Binding(
                                get: { viewModel.sourceMode },
                                set: { viewModel.setSourceMode($0) }
                            )
                        ) {
                            ForEach(MergeSourceMode.allCases) { mode in
                                Text(mode.rawValue).tag(mode)
                            }
                        }
                        .labelsHidden()
                        .pickerStyle(.segmented)
                    }
                }

                sectionCard("Profiles") {
                    inputRow(
                        title: "Profile A",
                        hint: "Local live Claude profile to merge into."
                    ) {
                        pathInput(
                            text: formStringBinding(for: \.profileA),
                            browseAction: {
                                if let selectedPath = DirectoryPicker.pickDirectory() {
                                    viewModel.form.profileA = selectedPath
                                }
                            }
                        )
                    }

                    if viewModel.sourceMode == .localProfileB {
                        inputRow(
                            title: "Profile B",
                            hint: "Secondary local profile directory."
                        ) {
                            pathInput(
                                text: formStringBinding(for: \.profileB),
                                browseAction: {
                                    if let selectedPath = DirectoryPicker.pickDirectory() {
                                        viewModel.form.profileB = selectedPath
                                    }
                                }
                            )
                        }
                    } else {
                        inputRow(
                            title: "Merge From",
                            hint: "Remote host in user@host format."
                        ) {
                            TextField("user@remote-mac", text: formStringBinding(for: \.mergeFrom))
                                .textFieldStyle(.roundedBorder)
                        }

                        inputRow(
                            title: "Remote Profile Path",
                            hint: "Remote Claude profile path (absolute or relative to remote home)."
                        ) {
                            TextField(
                                "Library/Application Support/Claude",
                                text: formStringBinding(for: \.remoteProfilePath)
                            )
                            .textFieldStyle(.roundedBorder)
                        }
                    }

                    inputRow(
                        title: "Output Profile",
                        hint: "Optional explicit output directory; leave empty for temp output path."
                    ) {
                        pathInput(
                            text: formStringBinding(for: \.outputProfile),
                            browseAction: {
                                if let selectedPath = DirectoryPicker.pickDirectory() {
                                    viewModel.form.outputProfile = selectedPath
                                }
                            }
                        )
                    }
                }

                sectionCard("Browser State") {
                    Toggle("Skip browser-state merge", isOn: formBoolBinding(for: \.skipBrowserState))

                    if !viewModel.form.skipBrowserState {
                        Toggle("Auto export browser state", isOn: formBoolBinding(for: \.autoExportBrowserState))
                        Toggle(
                            "Run browser export/import in headless mode",
                            isOn: formBoolBinding(for: \.headlessBrowserState)
                        )
                        Toggle("Skip IndexedDB merge", isOn: formBoolBinding(for: \.skipIndexedDB))

                        DisclosureGroup("Manual browser-state files", isExpanded: $viewModel.showManualBrowserState) {
                            VStack(spacing: 10) {
                                inputRow(
                                    title: "Browser State A JSON",
                                    hint: "Optional when auto export is enabled."
                                ) {
                                    pathInput(
                                        text: formStringBinding(for: \.browserStateA),
                                        browseAction: {
                                            if let selectedPath = DirectoryPicker.pickFile() {
                                                viewModel.form.browserStateA = selectedPath
                                            }
                                        }
                                    )
                                }

                                inputRow(
                                    title: "Browser State B JSON",
                                    hint: "Optional when auto export is enabled."
                                ) {
                                    pathInput(
                                        text: formStringBinding(for: \.browserStateB),
                                        browseAction: {
                                            if let selectedPath = DirectoryPicker.pickFile() {
                                                viewModel.form.browserStateB = selectedPath
                                            }
                                        }
                                    )
                                }

                                inputRow(
                                    title: "Browser State Output JSON",
                                    hint: "Destination path for merged browser-state output."
                                ) {
                                    pathInput(
                                        text: formStringBinding(for: \.browserStateOutput),
                                        browseAction: {
                                            if let selectedPath = DirectoryPicker.pickSaveFile(
                                                defaultName: "browser_state_merged.json"
                                            ) {
                                                viewModel.form.browserStateOutput = selectedPath
                                            }
                                        }
                                    )
                                }
                            }
                            .padding(.top, 8)
                        }
                        .padding(.top, 2)
                    }
                }

                sectionCard("Merge Behavior") {
                    inputRow(
                        title: "Unknown Key Base",
                        hint: "Keep unknown browser keys from source A or B."
                    ) {
                        Picker("Base Source", selection: formStringBinding(for: \.baseSource)) {
                            Text("A").tag("a")
                            Text("B").tag("b")
                        }
                        .labelsHidden()
                        .pickerStyle(.segmented)
                    }

                    Toggle("Apply merged profile after validation", isOn: formBoolBinding(for: \.apply))
                    Toggle("Force overwrite existing output profile", isOn: formBoolBinding(for: \.force))
                }

                sectionCard("Advanced") {
                    DisclosureGroup("Advanced Options", isExpanded: $viewModel.showAdvancedOptions) {
                        VStack(spacing: 10) {
                            Toggle("Include remote vm_bundles", isOn: formBoolBinding(for: \.includeVmBundles))
                            Toggle("Include cache directories", isOn: formBoolBinding(for: \.includeCacheDirs))
                            Toggle(
                                "Include secondary .claude/.credentials.json",
                                isOn: formBoolBinding(for: \.includeSensitiveClaudeCredentials)
                            )

                            inputRow(
                                title: "Parallel Remote",
                                hint: "Positive integer. Controls remote hashing parallelism."
                            ) {
                                TextField("example: 4", text: formStringBinding(for: \.parallelRemote))
                                    .textFieldStyle(.roundedBorder)
                            }

                            inputRow(
                                title: "Parallel Local",
                                hint: "Positive integer. Reserved for local parallel merge stages."
                            ) {
                                TextField("example: 8", text: formStringBinding(for: \.parallelLocal))
                                    .textFieldStyle(.roundedBorder)
                            }

                            inputRow(title: "Log Level") {
                                Picker("Log Level", selection: formStringBinding(for: \.logLevel)) {
                                    Text("DEBUG").tag("DEBUG")
                                    Text("INFO").tag("INFO")
                                    Text("WARNING").tag("WARNING")
                                    Text("ERROR").tag("ERROR")
                                }
                                .labelsHidden()
                                .pickerStyle(.segmented)
                            }
                        }
                        .padding(.top, 8)
                    }
                }

                if !viewModel.validationErrors.isEmpty {
                    sectionCard("Validation") {
                        VStack(alignment: .leading, spacing: 6) {
                            ForEach(viewModel.validationErrors, id: \.self) { error in
                                Label(error, systemImage: "exclamationmark.triangle.fill")
                                    .foregroundStyle(.red)
                            }
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var executionPane: some View {
        VStack(spacing: 12) {
            GroupBox {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .center, spacing: 10) {
                        Image(systemName: statusSymbolName)
                            .foregroundStyle(statusColor)
                        Text(viewModel.statusText)
                            .font(.headline)
                        Spacer()
                        if viewModel.isRunning {
                            ProgressView()
                                .controlSize(.small)
                        }
                    }

                    HStack(spacing: 10) {
                        Button("Run Merge") {
                            viewModel.runMerge()
                        }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(!viewModel.canRun)

                        Button("Cancel") {
                            viewModel.cancel()
                        }
                        .disabled(!viewModel.isRunning)

                        Spacer()

                        Button("Clear Log") {
                            viewModel.clearOutput()
                        }
                        .disabled(viewModel.outputLog.isEmpty)
                    }

                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Text("Command Preview")
                                .font(.subheadline.weight(.semibold))
                            Spacer()
                            Button("Copy") {
                                Clipboard.copyText(viewModel.commandPreview)
                            }
                        }
                        ScrollView {
                            Text(viewModel.commandPreview)
                                .font(.system(.caption, design: .monospaced))
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(8)
                        }
                        .frame(minHeight: 68, maxHeight: 68)
                        .background(Color(nsColor: .textBackgroundColor))
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                    }
                }
                .padding(8)
            }

            GroupBox {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("Execution Log")
                            .font(.headline)
                        Spacer()
                        Button("Copy Log") {
                            Clipboard.copyText(viewModel.outputLog)
                        }
                        .disabled(viewModel.outputLog.isEmpty)
                    }

                    ScrollView {
                        Text(viewModel.outputLog.isEmpty ? "Run a merge to see live output." : viewModel.outputLog)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(10)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Color(nsColor: .textBackgroundColor))
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                }
                .padding(8)
            }
            .frame(maxHeight: .infinity)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var statusSymbolName: String {
        switch viewModel.runStatus {
        case .idle:
            return "circle.dashed"
        case .running:
            return "arrow.triangle.2.circlepath.circle.fill"
        case .success:
            return "checkmark.circle.fill"
        case .failed:
            return "xmark.octagon.fill"
        case .cancelled:
            return "pause.circle.fill"
        }
    }

    private var statusColor: Color {
        switch viewModel.runStatus {
        case .idle:
            return .secondary
        case .running:
            return .blue
        case .success:
            return .green
        case .failed:
            return .red
        case .cancelled:
            return .orange
        }
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
    private func sectionCard<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.headline)
            content()
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor))
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    @ViewBuilder
    private func inputRow<Content: View>(
        title: String,
        hint: String? = nil,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.subheadline.weight(.semibold))
            content()
            if let hint {
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private func pathInput(text: Binding<String>, browseAction: @escaping () -> Void) -> some View {
        HStack(spacing: 8) {
            TextField("", text: text)
                .textFieldStyle(.roundedBorder)
                .frame(maxWidth: .infinity)
            Button("Browse", action: browseAction)
                .frame(width: 72)
        }
    }
}
