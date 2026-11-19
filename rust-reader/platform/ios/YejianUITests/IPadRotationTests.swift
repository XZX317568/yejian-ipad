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
        let runID = UUID().uuidString
        // Reuse the library imported before XCTest installed this app. Seeding
        // here would conceal broken stored paths after the container moves.
        app.launchArguments = [
            "--ui-snapshot", screen, "--ui-landscape", "--ui-snapshot-run", runID
        ]
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

        let marker = app.descendants(matching: .any)
            .matching(identifier: "yejian.snapshot.ready").firstMatch
        XCTAssertTrue(
            marker.waitForExistence(timeout: 90),
            "The landscape renderer never acknowledged loaded content for \(screen)"
        )
        let json = try XCTUnwrap(marker.value as? String)
        let data = try XCTUnwrap(json.data(using: .utf8))
        let report = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        XCTAssertEqual(report["run_id"] as? String, runID, "Stale readiness report")
        XCTAssertEqual(report["screen"] as? String, screen)
        let width = try XCTUnwrap(report["width"] as? Double)
        let height = try XCTUnwrap(report["height"] as? Double)
        XCTAssertGreaterThan(width, height)
        let books = try XCTUnwrap(report["books"] as? Int)
        XCTAssertEqual(books, 5, "The original imported library must survive reinstallation")
        XCTAssertEqual(report["archives_available"] as? Int, books)
        XCTAssertEqual(report["cover_files_available"] as? Int, books)
        XCTAssertEqual(report["covers_pending"] as? Int, 0)
        XCTAssertEqual(report["covers_failed"] as? Int, 0)
        if screen == "reader" {
            XCTAssertEqual(report["page_loaded"] as? Bool, true)
        } else {
            XCTAssertGreaterThan(try XCTUnwrap(report["covers_loaded"] as? Int), 0)
        }
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
        try data.write(
            to: documents.appendingPathComponent("\(screen)-landscape.json"),
            options: .atomic
        )
    }
}
