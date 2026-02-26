import Foundation

public struct MergeFormData: Equatable {
    public var profileA: String
    public var profileB: String
    public var mergeFrom: String
    public var remoteProfilePath: String
    public var outputProfile: String
    public var browserStateA: String
    public var browserStateB: String
    public var browserStateOutput: String
    public var autoExportBrowserState: Bool
    public var headlessBrowserState: Bool
    public var baseSource: String
    public var skipBrowserState: Bool
    public var skipIndexedDB: Bool
    public var includeVmBundles: Bool
    public var includeCacheDirs: Bool
    public var parallelRemote: String
    public var parallelLocal: String
    public var apply: Bool
    public var force: Bool
    public var includeSensitiveClaudeCredentials: Bool
    public var logLevel: String

    public init(
        profileA: String = "",
        profileB: String = "",
        mergeFrom: String = "",
        remoteProfilePath: String = "",
        outputProfile: String = "",
        browserStateA: String = "",
        browserStateB: String = "",
        browserStateOutput: String = "",
        autoExportBrowserState: Bool = true,
        headlessBrowserState: Bool = true,
        baseSource: String = "a",
        skipBrowserState: Bool = false,
        skipIndexedDB: Bool = false,
        includeVmBundles: Bool = false,
        includeCacheDirs: Bool = false,
        parallelRemote: String = "",
        parallelLocal: String = "",
        apply: Bool = false,
        force: Bool = false,
        includeSensitiveClaudeCredentials: Bool = false,
        logLevel: String = "WARNING"
    ) {
        self.profileA = profileA
        self.profileB = profileB
        self.mergeFrom = mergeFrom
        self.remoteProfilePath = remoteProfilePath
        self.outputProfile = outputProfile
        self.browserStateA = browserStateA
        self.browserStateB = browserStateB
        self.browserStateOutput = browserStateOutput
        self.autoExportBrowserState = autoExportBrowserState
        self.headlessBrowserState = headlessBrowserState
        self.baseSource = baseSource
        self.skipBrowserState = skipBrowserState
        self.skipIndexedDB = skipIndexedDB
        self.includeVmBundles = includeVmBundles
        self.includeCacheDirs = includeCacheDirs
        self.parallelRemote = parallelRemote
        self.parallelLocal = parallelLocal
        self.apply = apply
        self.force = force
        self.includeSensitiveClaudeCredentials = includeSensitiveClaudeCredentials
        self.logLevel = logLevel
    }

    public var validationErrors: [String] {
        var errors: [String] = []
        let hasProfileB = !profileB.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let hasMergeFrom = !mergeFrom.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty

        if hasProfileB == hasMergeFrom {
            errors.append("Set exactly one source: Profile B path or Merge From host.")
        }

        if let remoteParallelism = parsePositiveInt(parallelRemote), remoteParallelism < 1 {
            errors.append("Parallel Remote must be greater than or equal to 1.")
        } else if !parallelRemote.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  parsePositiveInt(parallelRemote) == nil {
            errors.append("Parallel Remote must be a whole number greater than or equal to 1.")
        }

        if let localParallelism = parsePositiveInt(parallelLocal), localParallelism < 1 {
            errors.append("Parallel Local must be greater than or equal to 1.")
        } else if !parallelLocal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  parsePositiveInt(parallelLocal) == nil {
            errors.append("Parallel Local must be a whole number greater than or equal to 1.")
        }

        if !skipBrowserState {
            let providedBrowserStates = [
                browserStateA.trimmingCharacters(in: .whitespacesAndNewlines),
                browserStateB.trimmingCharacters(in: .whitespacesAndNewlines),
                browserStateOutput.trimmingCharacters(in: .whitespacesAndNewlines),
            ].filter { !$0.isEmpty }
            if !providedBrowserStates.isEmpty && providedBrowserStates.count != 3 {
                errors.append("Provide all browser-state paths or leave all empty.")
            }
        }

        return errors
    }

    public var isValid: Bool {
        validationErrors.isEmpty
    }

    private func parsePositiveInt(_ value: String) -> Int? {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return nil
        }
        return Int(trimmed)
    }
}
