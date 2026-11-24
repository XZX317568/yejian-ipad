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
    func test04FiltersLandscape() throws { try capture("filters") }

    /// Native search prompt round-trip: open by tapping the real rendered
    /// button (coordinates come from the DEBUG snapshot report, never
    /// guessed), inject Unicode directly — this path intentionally marks
    /// text without an IME so it only proves the UITextField → Rust wire —
    /// cancel must not commit, and a submitted Chinese query must come back
    /// as the prompt's prefilled initial text.
    func test05NativeSearchRoundtrip() throws {
        let (app, report) = try launchBrowser()
        defer { app.terminate() }
        let rect = try searchRect(report)

        var field = try openNativeSearch(app, rect: rect)
        XCTAssertEqual(searchFieldCount(app), 1, "搜索框不允许叠开")

        field.tap()
        field.typeText("航海王草稿")
        tapButton(app, "取消")
        XCTAssertTrue(field.waitForNonExistence(timeout: 10))
        field = try openNativeSearch(app, rect: rect)
        let reopened = field.value as? String ?? ""
        XCTAssertTrue(reopened.isEmpty || reopened == "关键词",
                      "取消不能把草稿提交给 Rust：\(reopened)")
        tapButton(app, "取消")
        XCTAssertTrue(field.waitForNonExistence(timeout: 10))

        field = try openNativeSearch(app, rect: rect)
        field.tap()
        field.typeText("海贼王")
        tapButton(app, "搜索")
        XCTAssertTrue(field.waitForNonExistence(timeout: 10))

        field = try openNativeSearch(app, rect: rect)
        XCTAssertEqual(searchFieldCount(app), 1, "搜索框不允许叠开")
        XCTAssertEqual(field.value as? String, "海贼王",
                       "提交的关键词没有回传到 Rust 再回填")
    }

    /// Pinyin candidate path: letters are tapped on the real software
    /// keyboard and the Chinese text must be committed by tapping a
    /// candidate, which exercises marked-text handling that typeText can
    /// never touch. Fails loudly (with hierarchy + screenshot) when the
    /// simulator lacks a Chinese Pinyin keyboard — never faked by typeText.
    func test06PinyinCandidate() throws {
        let (app, report) = try launchBrowser()
        defer { app.terminate() }
        let rect = try searchRect(report)
        let field = try openNativeSearch(app, rect: rect)
        field.tap()
        guard app.keys["h"].waitForExistence(timeout: 10) else {
            return failWithContext(app, """
                屏幕键盘没有出现。测试需要模拟器软键盘：\
                Simulator ▸ I/O ▸ Keyboard ▸ Toggle Software Keyboard。
                """)
        }
        attachHierarchy(app, name: "test06-keyboard-open")
        // 已加载中文键盘但停在 Roman 模式：直接点「Switch from Roman keys」
        // 切到拼音（真实 XCTest 保存的 keyboard.txt 里该键 label 如此；空格
        // 键 label 只是单个空格，不能用来猜输入法）。键不在说明已是拼音。
        let roman = app.keys["Switch from Roman keys"]
        if roman.exists {
            tapKeyboardElement(roman, app: app)
        }
        for letter in ["h", "a", "i"] {
            let key = app.keys[letter]
            guard key.exists else {
                return failWithContext(app, "键盘缺少字母键 \(letter)，请确认当前为系统键盘。")
            }
            tapKeyboardElement(key, app: app)
        }
        let candidate = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "海"))
            .firstMatch
        guard candidate.waitForExistence(timeout: 10) else {
            return failWithContext(app, """
                输入拼音 hai 后候选条没有「海」。当前键盘不是中文拼音输入法；\
                请在模拟器 Settings ▸ General ▸ Keyboard ▸ Keyboards 添加\
                「简体中文 - 拼音」，切到该输入法后重跑本测试。
                """)
        }
        attachHierarchy(app, name: "pre-candidate-hierarchy")
        let preCommit = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        preCommit.name = "pre-candidate-tap"   // 候选出现后的证据，成功也保留
        preCommit.lifetime = .keepAlways
        add(preCommit)
        tapKeyboardElement(candidate, app: app)
        XCTAssertEqual(field.value as? String, "海", "候选字没有写进搜索框")
        // 键盘 Return 键 identifier 为 "Search"（label 是中文「搜索」，真实
        // keyboard.txt 已确认）；经 alert 坐标点击走真实 Return 提交路径。
        let keyboardSearch = app.keyboards.buttons
            .matching(identifier: "Search").firstMatch
        XCTAssertTrue(keyboardSearch.waitForExistence(timeout: 10),
                      "键盘 Search 键未出现")
        tapKeyboardElement(keyboardSearch, app: app)
        XCTAssertTrue(field.waitForNonExistence(timeout: 10))
        let again = try openNativeSearch(app, rect: rect)
        XCTAssertEqual(again.value as? String, "海",
                       "候选提交的关键词没有回传到 Rust 再回填")
    }

    // MARK: - Native search helpers

    /// Launch into the fake-fixture source browser (no network) and wait for
    /// the DEBUG snapshot report that carries the search button's rect.
    private func launchBrowser(
        file: StaticString = #filePath, line: UInt = #line
    ) throws -> (XCUIApplication, [String: Any]) {
        let app = XCUIApplication(bundleIdentifier: "app.yejian.reader.rust")
        let runID = UUID().uuidString
        app.launchArguments = [
            "--seed-demo", "--ui-snapshot", "browse", "--ui-snapshot-run", runID
        ]
        app.launch()
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 30), file: file, line: line)
        let marker = app.descendants(matching: .any)
            .matching(identifier: "yejian.snapshot.ready").firstMatch
        XCTAssertTrue(marker.waitForExistence(timeout: 90),
                      "browse 就绪标记未出现", file: file, line: line)
        let json = try XCTUnwrap(marker.value as? String, file: file, line: line)
        let report = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any],
            file: file, line: line
        )
        XCTAssertEqual(report["run_id"] as? String, runID, "过期就绪报告", file: file, line: line)
        let browser = try XCTUnwrap(report["source_browser"] as? [String: Any], file: file, line: line)
        XCTAssertEqual(browser["source"] as? String, "local.filter-acceptance", file: file, line: line)
        return (app, report)
    }

    /// [left, top, right, bottom] in app-window points (egui points match).
    private func searchRect(_ report: [String: Any]) throws -> [Double] {
        let browser = try XCTUnwrap(report["source_browser"] as? [String: Any])
        return try XCTUnwrap(
            browser["native_search_rect"] as? [Double],
            "DEBUG 报告缺少 source_browser.native_search_rect"
        )
    }

    /// Report coordinates share the app window's coordinate system, so a tap
    /// is the window origin plus the point offset.
    private func tapWindowPoint(_ app: XCUIApplication, x: Double, y: Double) {
        app.windows.firstMatch
            .coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
            .withOffset(CGVector(dx: x, dy: y))
            .tap()
    }

    /// Tap near the button's leading edge — safe even after the label grew
    /// ("搜索漫画…" → "搜索：海贼王") — and retry until the prompt appears.
    private func openNativeSearch(
        _ app: XCUIApplication, rect: [Double]
    ) throws -> XCUIElement {
        let field = app.descendants(matching: .any)
            .matching(identifier: "yejian.source.search.field").firstMatch
        if field.exists { return field }
        let x = rect[0] + min((rect[2] - rect[0]) / 2, 24)
        let y = (rect[1] + rect[3]) / 2
        let deadline = Date().addingTimeInterval(15)
        repeat {
            tapWindowPoint(app, x: x, y: y)
            if field.waitForExistence(timeout: 2) { return field }
        } while Date() < deadline
        XCTFail("点击 source-native-search (\(x), \(y)) 后原生搜索框未出现")
        return field
    }

    private func searchFieldCount(_ app: XCUIApplication) -> Int {
        app.descendants(matching: .any)
            .matching(identifier: "yejian.source.search.field").count
    }

    /// Tap a keyboard key or candidate by an alert-relative coordinate: the
    /// tap's associated element is our own alert, so XCTest's default
    /// interruption handler can never mistake it for an outside-alert tap
    /// and auto-dismiss the prompt (which silently cancels the query).
    private func tapKeyboardElement(_ element: XCUIElement, app: XCUIApplication) {
        XCTAssertTrue(element.exists, "键盘元素不存在")
        XCTAssertFalse(element.frame.isEmpty, "键盘元素 frame 为空")
        let alert = app.alerts["yejian.source.search.alert"]
        XCTAssertTrue(alert.exists, "搜索 alert 消失，键盘点击失去基准")
        let c = CGPoint(x: element.frame.midX, y: element.frame.midY)
        let o = alert.frame.origin
        alert.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: c.x - o.x, dy: c.y - o.y)).tap()
    }

    private func attachHierarchy(_ app: XCUIApplication, name: String) {
        let h = XCTAttachment(string: app.debugDescription)
        h.name = name; h.lifetime = .keepAlways; add(h)
    }

    private func tapButton(_ app: XCUIApplication, _ title: String) {
        let button = app.alerts["yejian.source.search.alert"].buttons[title].firstMatch
        XCTAssertTrue(button.waitForExistence(timeout: 10), "按钮「\(title)」未出现")
        button.tap()
    }

    /// Environment failures (missing keyboard/candidate IME) attach the
    /// element hierarchy and a screenshot so the fix is a config change,
    /// not a guessed workaround.
    private func failWithContext(_ app: XCUIApplication, _ message: String) {
        let hierarchy = XCTAttachment(string: app.debugDescription)
        hierarchy.name = "app-debugDescription"
        hierarchy.lifetime = .keepAlways
        add(hierarchy)
        let shot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        shot.name = "failure-context"
        shot.lifetime = .keepAlways
        add(shot)
        XCTFail(message)
    }

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
        } else if screen != "filters" {
            XCTAssertGreaterThan(try XCTUnwrap(report["covers_loaded"] as? Int), 0)
        }
        if screen == "filters" {
            let browser = try XCTUnwrap(report["source_browser"] as? [String: Any])
            XCTAssertEqual(browser["source"] as? String, "local.filter-acceptance")
            XCTAssertEqual(browser["filter_count"] as? Int, 7)
            XCTAssertEqual(browser["filter_editor"] as? Bool, true)
            XCTAssertEqual(browser["busy"] as? Bool, false)
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
