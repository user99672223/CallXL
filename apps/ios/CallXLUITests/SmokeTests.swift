import XCTest

/// M2 UI smoke: every screen reachable, key controls present (fake core), and launch
/// to Home under 1 s with the real core (§1.5).
final class SmokeTests: XCTestCase {
    override func setUp() {
        continueAfterFailure = false
    }

    private func element(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    /// Any element whose accessibility label contains `text` (tiles and rows combine
    /// their children into one label for VoiceOver).
    private func labeled(_ app: XCUIApplication, _ text: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", text)).firstMatch
    }

    private func back(_ app: XCUIApplication) {
        app.navigationBars.buttons.element(boundBy: 0).tap()
    }

    func testEveryScreen() {
        let app = XCUIApplication()
        app.launchArguments = ["-uiTestFakeCore"]
        app.launch()

        let newSession = app.buttons["home.newSession"]
        XCTAssertTrue(newSession.waitForExistence(timeout: 10))
        XCTAssertTrue(labeled(app, "Kitchen").waitForExistence(timeout: 5), "LAN session listed")

        // Settings
        app.buttons["home.settings"].tap()
        let rawMic = app.switches["settings.rawMic"]
        XCTAssertTrue(rawMic.waitForExistence(timeout: 5))
        XCTAssertTrue(element(app, "settings.bitrate").exists)
        XCTAssertTrue(element(app, "settings.displayName").exists)
        back(app)

        // Diagnostics + export
        app.buttons["home.diagnostics"].tap()
        let export = app.buttons["diag.export"]
        XCTAssertTrue(export.waitForExistence(timeout: 5))
        export.tap()
        XCTAssertTrue(element(app, "diag.exported").waitForExistence(timeout: 5))
        back(app)

        // New session -> In call
        newSession.tap()
        let create = app.buttons["newSession.create"]
        XCTAssertTrue(create.waitForExistence(timeout: 5))
        element(app, "newSession.name").tap()
        app.typeText("Smoke")
        create.tap()
        let mute = app.buttons["call.mute"]
        XCTAssertTrue(mute.waitForExistence(timeout: 10))
        XCTAssertTrue(labeled(app, "Desk PC").waitForExistence(timeout: 5), "peer tile")
        mute.tap()
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "identifier == 'call.mute' AND label == 'Unmute'")).firstMatch.waitForExistence(timeout: 3))
        XCTAssertTrue(element(app, "call.route").exists)

        // Chat drawer
        app.buttons["call.chat"].tap()
        let input = element(app, "chat.input")
        XCTAssertTrue(input.waitForExistence(timeout: 5))
        input.tap()
        app.typeText("hello")
        app.buttons["chat.send"].tap()
        XCTAssertTrue(labeled(app, "hello").waitForExistence(timeout: 5))
        XCTAssertTrue(labeled(app, "re: hello").waitForExistence(timeout: 5), "reply received")
        app.buttons["chat.close"].tap()

        // Diagnostics during the call shows peer stats
        app.buttons["call.diagnostics"].tap()
        XCTAssertTrue(app.buttons["diag.export"].waitForExistence(timeout: 5))
        XCTAssertTrue(labeled(app, "Pair switches").waitForExistence(timeout: 5))
        back(app)

        // Leave
        app.buttons["call.leave"].tap()
        XCTAssertTrue(newSession.waitForExistence(timeout: 5))
    }

    /// Real core. The first launch after install includes the simulator's one-time dyld
    /// work for the new framework, so the warm (second) launch is the measured one;
    /// both are printed.
    func testLaunchToHomeUnderOneSecond() throws {
        var results: [Double] = []
        for _ in 0 ..< 2 {
            let app = XCUIApplication()
            app.launchArguments = ["-uiTestReportLaunch"]
            app.launch()
            let label = element(app, "home.launchMs")
            XCTAssertTrue(label.waitForExistence(timeout: 10))
            let text = label.label.isEmpty ? (label.value as? String ?? "") : label.label
            let ms = try XCTUnwrap(Double(text), "launch label \(text)")
            results.append(ms)
            XCTAssertTrue(app.buttons["home.newSession"].exists)
            app.terminate()
        }
        print("launch-to-home-ms cold=\(results[0]) warm=\(results[1])")
        XCTAssertLessThan(results[1], 1000, "launch to Home took \(results[1]) ms")
    }
}
