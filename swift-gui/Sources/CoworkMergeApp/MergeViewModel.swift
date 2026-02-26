import CoworkMergeCore
import Foundation

enum MergeSourceMode: String, CaseIterable, Identifiable {
    case localProfileB = "Local Profile B"
    case remoteHost = "Remote Host"

    var id: String { rawValue }
}

enum MergeRunStatus {
    case idle
    case running
    case success
    case failed
    case cancelled
}

@MainActor
final class MergeViewModel: ObservableObject {
    @Published var form: MergeFormData
    @Published var workspacePath: String
    @Published var outputLog: String = ""
    @Published var sourceMode: MergeSourceMode
    @Published var statusText: String = "Ready"
    @Published var runStatus: MergeRunStatus = .idle
    @Published var isRunning: Bool = false
    @Published var showAdvancedOptions: Bool = false
    @Published var showManualBrowserState: Bool = false

    private let runner = ShellCommandRunner()
    private var runningTask: Task<Void, Never>?
    private let maxLogCharacters = 250_000

    init() {
        form = MergeFormData(
            profileA: "\(FileManager.default.homeDirectoryForCurrentUser.path)/Library/Application Support/Claude",
            remoteProfilePath: "Library/Application Support/Claude"
        )
        workspacePath = FileManager.default.currentDirectoryPath
        sourceMode = .remoteHost
    }

    var validationErrors: [String] {
        form.validationErrors
    }

    var canRun: Bool {
        !isRunning && validationErrors.isEmpty
    }

    var commandPreview: String {
        shellLine(from: MergeCommandBuilder.buildCommand(form: form))
    }

    func setSourceMode(_ mode: MergeSourceMode) {
        sourceMode = mode
        switch mode {
        case .localProfileB:
            form.mergeFrom = ""
        case .remoteHost:
            form.profileB = ""
        }
    }

    func runMerge() {
        guard !isRunning else {
            return
        }
        let errors = form.validationErrors
        guard errors.isEmpty else {
            appendOutput("Validation failed:\n")
            for error in errors {
                appendOutput("- \(error)\n")
            }
            runStatus = .failed
            statusText = "Fix validation errors before running."
            return
        }

        let command = MergeCommandBuilder.buildCommand(form: form)
        appendOutput("$ \(shellLine(from: command))\n")
        isRunning = true
        runStatus = .running
        statusText = "Merge in progress..."

        runningTask = Task {
            do {
                let status = try await runner.run(command: command, workingDirectory: workspacePath) { [weak self] chunk in
                    Task { @MainActor in
                        self?.appendOutput(chunk)
                    }
                }
                await MainActor.run {
                    self.isRunning = false
                    self.runStatus = status == 0 ? .success : .failed
                    self.statusText = status == 0
                        ? "Merge completed successfully."
                        : "Merge failed with exit code \(status)."
                }
            } catch {
                await MainActor.run {
                    self.isRunning = false
                    self.runStatus = .failed
                    self.statusText = "Merge failed: \(error.localizedDescription)"
                    self.appendOutput("Error: \(error.localizedDescription)\n")
                }
            }
        }
    }

    func cancel() {
        guard isRunning else {
            return
        }
        Task {
            await runner.cancel()
        }
        runningTask?.cancel()
        runningTask = nil
        isRunning = false
        runStatus = .cancelled
        statusText = "Cancelled."
        appendOutput("Command cancelled by user.\n")
    }

    func clearOutput() {
        outputLog = ""
    }

    private func appendOutput(_ text: String) {
        outputLog.append(text)
        if outputLog.count > maxLogCharacters {
            outputLog.removeFirst(outputLog.count - maxLogCharacters)
        }
    }

    private func shellLine(from command: [String]) -> String {
        command
            .map { part in
                if part.contains(" ") {
                    return "\"\(part)\""
                }
                return part
            }
            .joined(separator: " ")
    }
}
