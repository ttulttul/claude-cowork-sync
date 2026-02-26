import CoworkMergeCore
import XCTest

final class MergeCommandBuilderTests: XCTestCase {
    func testBuildCommandWithRemoteSourceAndApply() {
        let form = MergeFormData(
            profileA: "/Users/test/Library/Application Support/Claude",
            mergeFrom: "user@example-host",
            remoteProfilePath: "Library/Application Support/Claude",
            outputProfile: "/tmp/merged-output",
            autoExportBrowserState: true,
            headlessBrowserState: true,
            baseSource: "a",
            skipBrowserState: false,
            skipIndexedDB: false,
            includeVmBundles: true,
            includeCacheDirs: true,
            parallelRemote: "4",
            parallelLocal: "8",
            apply: true,
            force: true,
            includeSensitiveClaudeCredentials: true,
            logLevel: "INFO"
        )

        let command = MergeCommandBuilder.buildCommand(form: form)

        XCTAssertEqual(
            command,
            [
                "uv", "run", "cowork-merge", "--log-level", "INFO", "merge",
                "--profile-a", "/Users/test/Library/Application Support/Claude",
                "--merge-from", "user@example-host",
                "--remote-profile-path", "Library/Application Support/Claude",
                "--output-profile", "/tmp/merged-output",
                "--parallel-remote", "4",
                "--parallel-local", "8",
                "--base-source", "a",
                "--auto-export-browser-state",
                "--include-vm-bundles",
                "--include-cache-dirs",
                "--apply",
                "--force",
                "--include-sensitive-claude-credentials",
                "--headless-browser-state",
            ]
        )
    }

    func testValidationRequiresExactlyOneSecondarySource() {
        var form = MergeFormData(profileA: "/tmp/a")
        XCTAssertFalse(form.isValid)
        XCTAssertTrue(form.validationErrors.contains("Set exactly one source: Profile B path or Merge From host."))

        form.profileB = "/tmp/b"
        XCTAssertTrue(form.isValid)

        form.mergeFrom = "user@host"
        XCTAssertFalse(form.isValid)
        XCTAssertTrue(form.validationErrors.contains("Set exactly one source: Profile B path or Merge From host."))
    }

    func testValidationRequiresAllBrowserStatePathsWhenAnyProvided() {
        let form = MergeFormData(
            profileA: "/tmp/a",
            profileB: "/tmp/b",
            browserStateA: "/tmp/a.json"
        )

        XCTAssertFalse(form.isValid)
        XCTAssertTrue(form.validationErrors.contains("Provide all browser-state paths or leave all empty."))
    }

    func testBuildCommandWithNonHeadlessFlag() {
        let form = MergeFormData(
            profileA: "/tmp/a",
            profileB: "/tmp/b",
            headlessBrowserState: false
        )

        let command = MergeCommandBuilder.buildCommand(form: form)
        XCTAssertTrue(command.contains("--no-headless-browser-state"))
        XCTAssertFalse(command.contains("--headless-browser-state"))
    }

    func testValidationRejectsNonPositiveParallelism() {
        let form = MergeFormData(
            profileA: "/tmp/a",
            profileB: "/tmp/b",
            parallelRemote: "0",
            parallelLocal: "-2"
        )

        XCTAssertFalse(form.isValid)
        XCTAssertTrue(form.validationErrors.contains("Parallel Remote must be greater than or equal to 1."))
        XCTAssertTrue(form.validationErrors.contains("Parallel Local must be greater than or equal to 1."))
    }
}
