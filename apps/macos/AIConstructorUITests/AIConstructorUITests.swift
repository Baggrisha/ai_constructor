import Foundation
import XCTest

final class AIConstructorUITests: XCTestCase {
    func testCreateEditAndReopenProject() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }

        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()

        let create = app.buttons["new-project"]
        XCTAssertTrue(create.waitForExistence(timeout: 10))
        create.click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])

        let name = app.textFields["frame-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 10))
        name.click()
        name.typeKey("a", modifierFlags: .command)
        name.typeText("Изменённый фрейм")
        let width = app.textFields["frame-width"]
        let height = app.textFields["frame-height"]
        for (field, value) in [(width, "1024"), (height, "768")] {
            XCTAssertTrue(field.waitForExistence(timeout: 5))
            field.click()
            field.typeKey("a", modifierFlags: .command)
            field.typeText(value)
        }
        app.buttons["apply-frame"].click()
        XCTAssertTrue(app.staticTexts["Сохранено"].waitForExistence(timeout: 10))

        app.terminate()
        app.launch()
        let recent = app.staticTexts["Новый проект"].firstMatch
        XCTAssertTrue(recent.waitForExistence(timeout: 10))
        recent.click()
        let reopenedName = app.textFields["frame-name"]
        XCTAssertTrue(reopenedName.waitForExistence(timeout: 10))
        XCTAssertEqual(reopenedName.value as? String, "Изменённый фрейм")
        let reopenedWidth = app.textFields["frame-width"]
        let reopenedHeight = app.textFields["frame-height"]
        XCTAssertEqual((reopenedWidth.value as? String)?.filter(\.isNumber), "1024")
        XCTAssertEqual((reopenedHeight.value as? String)?.filter(\.isNumber), "768")
    }
}
