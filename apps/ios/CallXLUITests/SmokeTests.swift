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
        // The peer section is below the fold of a lazy List: scroll until it is loaded.
        let switches = labeled(app, "Pair switches")
        for _ in 0 ..< 6 where !switches.exists {
            app.swipeUp()
        }
        XCTAssertTrue(switches.waitForExistence(timeout: 5), "peer stats section")
        back(app)

        // Leave
        app.buttons["call.leave"].tap()
        XCTAssertTrue(newSession.waitForExistence(timeout: 5))
    }

    /// Real core, launched by XCUITest. XCUITest's own launch procedure is included in the
    /// process-start clock (about 1.5 s on the CI runner, before any app code runs), so this
    /// test reports the phases; the §1.5 limit is asserted on a plain `simctl launch` of the
    /// same Release build in CI (packaging/ios/measure-launch.sh).
    func testLaunchReachesHome() {
        for run in ["cold", "warm"] {
            let app = XCUIApplication()
            app.launchArguments = ["-uiTestReportLaunch"]
            app.launch()
            let label = element(app, "home.launchMs")
            XCTAssertTrue(label.waitForExistence(timeout: 10))
            XCTAssertTrue(app.buttons["home.newSession"].exists)
            print("launch-to-home-ms (XCUITest, \(run)): \(label.label); launch phases: \(element(app, "home.launchBreakdown").label)")
            app.terminate()
        }
    }
}
