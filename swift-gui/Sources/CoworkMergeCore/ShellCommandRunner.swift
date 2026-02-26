import Foundation

public actor ShellCommandRunner {
    private var process: Process?

    public init() {}

    public func run(
        command: [String],
        workingDirectory: String?,
        onOutput: @escaping @Sendable (String) -> Void
    ) async throws -> Int32 {
        guard !command.isEmpty else {
            throw ShellCommandRunnerError.emptyCommand
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = command
        if let workingDirectory, !workingDirectory.isEmpty {
            process.currentDirectoryURL = URL(fileURLWithPath: workingDirectory)
        }

        let outputPipe = Pipe()
        process.standardOutput = outputPipe
        process.standardError = outputPipe

        let outputHandle = outputPipe.fileHandleForReading
        outputHandle.readabilityHandler = { handle in
            let data = handle.availableData
            guard !data.isEmpty else {
                return
            }
            if let text = String(data: data, encoding: .utf8) {
                onOutput(text)
            }
        }

        self.process = process
        try process.run()
        process.waitUntilExit()
        outputHandle.readabilityHandler = nil
        self.process = nil
        return process.terminationStatus
    }

    public func cancel() {
        process?.terminate()
    }
}

public enum ShellCommandRunnerError: Error {
    case emptyCommand
}
