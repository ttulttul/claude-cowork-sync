import CoworkMergeCore
import Foundation

@MainActor
final class MergeViewModel: ObservableObject {
    @Published var form: MergeFormData = MergeFormData(
        profileA: "\(FileManager.default.homeDirectoryForCurrentUser.path)/Library/Application Support/Claude",
        remoteProfilePath: "Library/Application Support/Claude"
    )
    @Published var workspacePath: String = FileManager.default.currentDirectoryPath
    @Published var outputLog: String = ""
    @Published var isRunning: Bool = false
    @Published var statusText: String = "Idle"

    private let runner = ShellCommandRunner()
    private var runningTask: Task<Void, Never>?

    var validationErrors: [String] {
        form.validationErrors
    }

    func runMerge() {
        guard !isRunning else {
            return
        }
        let errors = form.validationErrors
        guard errors.isEmpty else {
            outputLog.append("Validation failed:\n")
            for error in errors {
                outputLog.append("- \(error)\n")
            }
            return
        }

        let command = MergeCommandBuilder.buildCommand(form: form)
        outputLog.append("$ \(shellLine(from: command))\n")
        isRunning = true
        statusText = "Running merge..."
        runningTask = Task {
            do {
                let status = try await runner.run(command: command, workingDirectory: self.workspacePath) { [weak self] chunk in
                    Task { @MainActor in
                        self?.outputLog.append(chunk)
                    }
                }
                await MainActor.run {
                    self.isRunning = false
                    self.statusText = status == 0 ? "Merge completed successfully." : "Merge failed with exit code \(status)."
                }
            } catch {
                await MainActor.run {
                    self.isRunning = false
                    self.statusText = "Merge failed: \(error.localizedDescription)"
                    self.outputLog.append("Error: \(error.localizedDescription)\n")
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
        statusText = "Cancelled."
        outputLog.append("Command cancelled by user.\n")
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
