import XCTest

/// Rotate the simulated device through XCTest. An app's geometry request is
/// rejected in iPad multitasking mode, even though physical rotation works.
final class IPadRotationTests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    func test01ShelfLandscape() throws { try capture("shelf") }
    func test02DetailLandscape() throws { try capture("detail") }
    func test03ReaderLandscape() throws { try capture("reader") }

    private func capture(_ screen: String) throws {
        let app = XCUIApplication(bundleIdentifier: "app.yejian.reader.rust")
        app.launchArguments = ["--ui-snapshot", screen, "--ui-landscape"]
        XCUIDevice.shared.orientation = .portrait
        app.launch()
        defer {
            app.terminate()
            XCUIDevice.shared.orientation = .portrait
        }
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 30))
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 30))
        XCUIDevice.shared.orientation = .landscapeRight
        let window = app.windows.firstMatch
        let rotated = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in window.frame.width > window.frame.height },
            object: window
        )
        XCTAssertEqual(XCTWaiter.wait(for: [rotated], timeout: 30), .completed)

        // Allow the production UI to finish loading its page and to write the
        // readiness marker that the shell checks separately after these tests.
        let settled = expectation(description: "landscape compositor settled")
        DispatchQueue.main.asyncAfter(deadline: .now() + 5) { settled.fulfill() }
        wait(for: [settled], timeout: 10)
        XCTAssertEqual(app.state, .runningForeground)
        let screenshot = XCUIScreen.main.screenshot()
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = "\(screen)-landscape"
        attachment.lifetime = .keepAlways
        add(attachment)
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        try screenshot.pngRepresentation.write(
            to: documents.appendingPathComponent("\(screen)-landscape.png")
        )
    }
}
