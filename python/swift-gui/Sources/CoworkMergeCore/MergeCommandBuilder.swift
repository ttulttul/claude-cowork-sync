import Foundation

public enum MergeCommandBuilder {
    public static func buildCommand(form: MergeFormData) -> [String] {
        var args: [String] = ["uv", "run", "cowork-merge", "--log-level", form.logLevel, "merge"]

        appendOptionalFlag("--profile-a", value: form.profileA, to: &args)
        appendOptionalFlag("--profile-b", value: form.profileB, to: &args)
        appendOptionalFlag("--merge-from", value: form.mergeFrom, to: &args)
        appendOptionalFlag("--remote-profile-path", value: form.remoteProfilePath, to: &args)
        appendOptionalFlag("--output-profile", value: form.outputProfile, to: &args)
        appendOptionalFlag("--browser-state-a", value: form.browserStateA, to: &args)
        appendOptionalFlag("--browser-state-b", value: form.browserStateB, to: &args)
        appendOptionalFlag("--browser-state-output", value: form.browserStateOutput, to: &args)
        appendOptionalFlag("--parallel-remote", value: form.parallelRemote, to: &args)
        appendOptionalFlag("--parallel-local", value: form.parallelLocal, to: &args)

        args.append("--base-source")
        args.append(form.baseSource)

        appendBooleanFlag("--auto-export-browser-state", enabled: form.autoExportBrowserState, to: &args)
        appendBooleanFlag("--skip-browser-state", enabled: form.skipBrowserState, to: &args)
        appendBooleanFlag("--skip-indexeddb", enabled: form.skipIndexedDB, to: &args)
        appendBooleanFlag("--include-vm-bundles", enabled: form.includeVmBundles, to: &args)
        appendBooleanFlag("--include-cache-dirs", enabled: form.includeCacheDirs, to: &args)
        appendBooleanFlag("--apply", enabled: form.apply, to: &args)
        appendBooleanFlag("--force", enabled: form.force, to: &args)
        appendBooleanFlag(
            "--include-sensitive-claude-credentials",
            enabled: form.includeSensitiveClaudeCredentials,
            to: &args
        )

        args.append(form.headlessBrowserState ? "--headless-browser-state" : "--no-headless-browser-state")
        return args
    }

    private static func appendOptionalFlag(_ flag: String, value: String, to args: inout [String]) {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return
        }
        args.append(flag)
        args.append(trimmed)
    }

    private static func appendBooleanFlag(_ flag: String, enabled: Bool, to args: inout [String]) {
        guard enabled else {
            return
        }
        args.append(flag)
    }
}
