import Foundation
import Darwin
import XCTest

final class AIConstructorUITests: XCTestCase {
    private func waitUntilEnabled(_ element: XCUIElement, timeout: TimeInterval = 15) -> Bool {
        let condition = XCTNSPredicateExpectation(predicate: NSPredicate(format: "isEnabled == true"), object: element)
        return XCTWaiter.wait(for: [condition], timeout: timeout) == .completed
    }
    private func waitForFrame(_ project: URL, name: String, width: Double? = nil, height: Double? = nil) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            guard let data = try? Data(contentsOf: project.appendingPathComponent("document.json")),
                  let document = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
                  let nodes = document["nodes"] as? [[String: Any]],
                  let node = nodes.first,
                  let layout = node["layout"] as? [String: Any],
                  node["name"] as? String == name else { return false }
            return (width == nil || (layout["w"] as? NSNumber)?.doubleValue == width)
                && (height == nil || (layout["h"] as? NSNumber)?.doubleValue == height)
        }, object: nil)
        return XCTWaiter.wait(for: [expectation], timeout: 15) == .completed
    }

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
        XCTAssertTrue(waitUntilEnabled(name))
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
        let project = base.appendingPathComponent("projects/Новый проект")
        XCTAssertTrue(waitForFrame(project, name: "Изменённый фрейм", width: 1024, height: 768))
        XCTAssertTrue(app.staticTexts["Сохранено"].waitForExistence(timeout: 10))

        app.terminate()
        app.launch()
        let recent = app.staticTexts["Новый проект"].firstMatch
        XCTAssertTrue(recent.waitForExistence(timeout: 10))
        recent.click()
        let reopenedName = app.textFields["frame-name"]
        XCTAssertTrue(reopenedName.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(reopenedName))
        XCTAssertEqual(reopenedName.value as? String, "Изменённый фрейм")
        let reopenedWidth = app.textFields["frame-width"]
        let reopenedHeight = app.textFields["frame-height"]
        XCTAssertEqual((reopenedWidth.value as? String)?.filter(\.isNumber), "1024")
        XCTAssertEqual((reopenedHeight.value as? String)?.filter(\.isNumber), "768")
    }

    func testNumericDraftRetainsDecimalSeparatorUntilTypingContinues() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.buttons["new-project"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        let width = app.textFields["frame-width"]
        XCTAssertTrue(width.waitForExistence(timeout: 10))
        width.click()
        width.typeKey("a", modifierFlags: .command)
        width.typeText("1440.")
        let pause = expectation(description: "debounce passes while the field stays focused")
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { pause.fulfill() }
        wait(for: [pause], timeout: 3)
        XCTAssertEqual(width.value as? String, "1440.")
        width.typeText("5")
        XCTAssertTrue(waitForFrame(base.appendingPathComponent("projects/Новый проект"), name: "Frame", width: 1440.5))
    }

    func testAutosaveOnlyChangesEditedPropertyMetadataThroughUndoRedo() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.buttons["new-project"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        let height = app.textFields["frame-height"]
        XCTAssertTrue(height.waitForExistence(timeout: 10))
        let project = base.appendingPathComponent("projects/Новый проект")
        func metadata() throws -> [String: Any] {
            let data = try Data(contentsOf: project.appendingPathComponent("document.json"))
            let doc = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
            let nodes = try XCTUnwrap(doc["nodes"] as? [[String: Any]])
            return try XCTUnwrap(nodes.first?["propertyMeta"] as? [String: Any])
        }
        func replace(_ field: XCUIElement, _ value: String) {
            field.click()
            field.typeKey("a", modifierFlags: .command)
            field.typeText(value)
        }
        replace(height, "700")
        XCTAssertTrue(waitForFrame(project, name: "Frame", width: 1440, height: 700))
        let heightMeta = try XCTUnwrap(metadata()["/layout/h"] as? NSDictionary)
        let width = app.textFields["frame-width"]
        replace(width, "1200")
        XCTAssertTrue(waitForFrame(project, name: "Frame", width: 1200, height: 700))
        let afterWidth = try metadata()
        XCTAssertEqual(afterWidth["/layout/h"] as? NSDictionary, heightMeta)
        let widthMeta = try XCTUnwrap(afterWidth["/layout/w"] as? NSDictionary)
        replace(app.textFields["frame-name"], "Renamed")
        XCTAssertTrue(waitForFrame(project, name: "Renamed", width: 1200, height: 700))
        let afterName = try metadata()
        XCTAssertEqual(afterName["/layout/w"] as? NSDictionary, widthMeta)
        XCTAssertEqual(afterName["/layout/h"] as? NSDictionary, heightMeta)
        app.textFields["Поиск проектов"].click()
        let undo = app.buttons["Отменить"]
        XCTAssertTrue(waitUntilEnabled(undo))
        undo.click()
        XCTAssertTrue(waitForFrame(project, name: "Frame", width: 1200, height: 700))
        XCTAssertEqual(try metadata()["/layout/w"] as? NSDictionary, widthMeta)
        XCTAssertEqual(try metadata()["/layout/h"] as? NSDictionary, heightMeta)
        let redo = app.buttons["Повторить"]
        XCTAssertTrue(waitUntilEnabled(redo))
        redo.click()
        XCTAssertTrue(waitForFrame(project, name: "Renamed", width: 1200, height: 700))
        XCTAssertEqual(try metadata()["/layout/w"] as? NSDictionary, widthMeta)
        XCTAssertEqual(try metadata()["/layout/h"] as? NSDictionary, heightMeta)
    }

    func testInvalidDraftSurvivesPanelChangesAndCanBeCorrected() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.buttons["new-project"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        let width = app.textFields["frame-width"]
        XCTAssertTrue(width.waitForExistence(timeout: 10))
        width.click()
        width.typeKey("a", modifierFlags: .command)
        width.typeText("0")
        XCTAssertTrue(app.staticTexts["Укажите название и положительные размеры."].waitForExistence(timeout: 5))
        let project = base.appendingPathComponent("projects/Новый проект")
        XCTAssertTrue(waitForFrame(project, name: "Frame", width: 1440, height: 900))
        app.descendants(matching: .any)["panel-chats"].firstMatch.click()
        app.descendants(matching: .any)["panel-inspector"].firstMatch.click()
        XCTAssertTrue(width.waitForExistence(timeout: 5))
        XCTAssertEqual(width.value as? String, "0")
        width.click()
        width.typeKey("a", modifierFlags: .command)
        width.typeText("800")
        XCTAssertTrue(waitForFrame(project, name: "Frame", width: 800, height: 900))
    }

    func testCancelWhileLockedKeepsDraftAndAllowsRetry() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.buttons["new-project"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        let name = app.textFields["frame-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 10))
        let project = base.appendingPathComponent("projects/Новый проект")
        let fd = Darwin.open(project.appendingPathComponent(".aic-local/write.lock").path, O_RDWR)
        XCTAssertGreaterThanOrEqual(fd, 0)
        defer { _ = Darwin.close(fd) }
        XCTAssertEqual(Darwin.flock(fd, LOCK_EX), 0)
        name.click()
        name.typeKey("a", modifierFlags: .command)
        name.typeText("Pending")
        let cancel = app.buttons["Отменить операцию"]
        XCTAssertTrue(cancel.waitForExistence(timeout: 10))
        XCTAssertTrue(name.isEnabled)
        XCTAssertFalse(app.buttons["new-project"].isEnabled)
        XCTAssertFalse(app.descendants(matching: .any)["panel-chats"].firstMatch.isEnabled)
        cancel.click()
        let acknowledge = app.buttons["OK"]
        XCTAssertTrue(acknowledge.waitForExistence(timeout: 10))
        acknowledge.click()
        XCTAssertEqual(name.value as? String, "Pending")
        XCTAssertTrue(waitForFrame(project, name: "Frame"))
        XCTAssertEqual(Darwin.flock(fd, LOCK_UN), 0)
        app.buttons["apply-frame"].click()
        XCTAssertTrue(waitForFrame(project, name: "Pending"))
    }

    func testRemovingRecentAndCancellingTrashPreserveProjectFiles() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }

        let create = app.buttons["new-project"]
        XCTAssertTrue(create.waitForExistence(timeout: 10))
        create.click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))

        let project = base.appendingPathComponent("projects/Новый проект").standardizedFileURL.resolvingSymlinksInPath()
        let row = app.descendants(matching: .any)["recent-project-\(project.path)"].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.rightClick()
        app.menuItems["Переместить в Корзину…"].click()
        XCTAssertTrue(app.buttons["Переместить в Корзину"].waitForExistence(timeout: 5))
        app.buttons["Отмена"].click()
        XCTAssertTrue(FileManager.default.fileExists(atPath: project.appendingPathComponent("project.json").path))

        row.rightClick()
        app.menuItems["Убрать из списка"].click()
        XCTAssertTrue(FileManager.default.fileExists(atPath: project.appendingPathComponent("project.json").path))
    }

    func testChatAndPanelSelectionSurviveReopening() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }

        let create = app.buttons["new-project"]
        XCTAssertTrue(create.waitForExistence(timeout: 10))
        create.click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))

        let chats = app.descendants(matching: .any)["panel-chats"].firstMatch
        XCTAssertTrue(chats.waitForExistence(timeout: 5))
        chats.click()
        let newChat = app.buttons["Новый чат"]
        XCTAssertTrue(newChat.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(newChat))
        newChat.click()
        let title = app.textFields["chat-title"]
        XCTAssertTrue(title.waitForExistence(timeout: 5))
        title.click()
        title.typeText("Project notes")
        app.buttons["confirm-chat"].click()
        let message = app.textViews["chat-message"]
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(message))
        message.click()
        message.typeText("Persistent message")
        app.buttons["save-chat-message"].click()
        XCTAssertTrue(app.staticTexts["Persistent message"].waitForExistence(timeout: 10))

        app.terminate()
        app.launch()
        let project = base.appendingPathComponent("projects/Новый проект").standardizedFileURL.resolvingSymlinksInPath()
        let row = app.descendants(matching: .any)["recent-project-\(project.path)"].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.click()
        XCTAssertTrue(app.staticTexts["Persistent message"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["Новый чат"].exists)
    }

    func testExternalChangeRequiresExplicitVersionChoice() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let projects = base.appendingPathComponent("real-projects", isDirectory: true)
        try FileManager.default.createDirectory(at: projects, withIntermediateDirectories: true)
        let projectsAlias = base.appendingPathComponent("projects", isDirectory: true)
        try FileManager.default.createSymbolicLink(at: projectsAlias, withDestinationURL: projects)
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }

        let create = app.buttons["new-project"]
        XCTAssertTrue(create.waitForExistence(timeout: 10))
        create.click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(app.textFields["frame-name"]))
        let width = app.textFields["frame-width"]
        width.click()
        width.typeKey("a", modifierFlags: .command)
        width.typeText("0")
        XCTAssertEqual(width.value as? String, "0")

        let manifest = base.appendingPathComponent("projects/Новый проект/project.json")
        let original = try Data(contentsOf: manifest)
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: original) as? [String: Any])
        object["name"] = "Внешнее изменение"
        let changed = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys])
        try changed.write(to: manifest, options: .atomic)

        let diagnostics = app.descendants(matching: .any)["panel-diagnostics"].firstMatch
        XCTAssertTrue(diagnostics.waitForExistence(timeout: 5))
        diagnostics.click()
        let review = app.buttons["Просмотреть и разрешить…"]
        XCTAssertTrue(review.waitForExistence(timeout: 20))
        XCTAssertFalse(review.isEnabled)
        let discard = app.buttons["discard-frame-draft"]
        XCTAssertTrue(discard.waitForExistence(timeout: 5))
        discard.click()
        let confirmDiscard = app.buttons["Отбросить черновик"]
        XCTAssertTrue(confirmDiscard.waitForExistence(timeout: 5))
        confirmDiscard.click()
        XCTAssertTrue(waitUntilEnabled(review))
        review.click()
        let apply = app.buttons["Применить выбор…"]
        XCTAssertTrue(apply.waitForExistence(timeout: 5))
        XCTAssertFalse(apply.isEnabled)
        app.buttons["Закрыть"].click()
        XCTAssertEqual(try Data(contentsOf: manifest), changed)
    }

    func testExternalDocumentChoiceRequiresExplicitDraftDecision() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.buttons["new-project"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        let width = app.textFields["frame-width"]
        XCTAssertTrue(width.waitForExistence(timeout: 10))
        width.click()
        width.typeKey("a", modifierFlags: .command)
        width.typeText("0")
        let project = base.appendingPathComponent("projects/Новый проект")
        let path = project.appendingPathComponent("document.json")
        var document = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: path)) as? [String: Any])
        var nodes = try XCTUnwrap(document["nodes"] as? [[String: Any]])
        var layout = try XCTUnwrap(nodes[0]["layout"] as? [String: Any])
        nodes[0]["name"] = "External"
        layout["h"] = 1200
        nodes[0]["layout"] = layout
        document["nodes"] = nodes
        let external = try JSONSerialization.data(withJSONObject: document, options: [.prettyPrinted, .sortedKeys])
        try external.write(to: path, options: .atomic)
        app.descendants(matching: .any)["panel-diagnostics"].firstMatch.click()
        let review = app.buttons["Просмотреть и разрешить…"]
        XCTAssertTrue(review.waitForExistence(timeout: 20))
        XCTAssertFalse(review.isEnabled)
        XCTAssertEqual(try Data(contentsOf: path), external)
        app.descendants(matching: .any)["panel-inspector"].firstMatch.click()
        XCTAssertEqual(width.value as? String, "0")
        app.descendants(matching: .any)["panel-diagnostics"].firstMatch.click()
        app.buttons["discard-frame-draft"].click()
        let discard = app.buttons["Отбросить черновик"]
        XCTAssertTrue(discard.waitForExistence(timeout: 5))
        discard.click()
        XCTAssertTrue(waitUntilEnabled(review))
        review.click()
        let picker = app.popUpButtons["Использовать"].firstMatch
        XCTAssertTrue(picker.waitForExistence(timeout: 10))
        picker.click()
        app.menuItems["Версия на диске"].click()
        app.buttons["Применить выбор…"].click()
        let confirm = app.buttons["Применить"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        confirm.click()
        XCTAssertTrue(waitForFrame(project, name: "External", width: 1440, height: 1200))
    }

    func testTwoProjectsRetainIndependentStateAfterRestart() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }

        func create(_ projectName: String) {
            let button = app.buttons["new-project"]
            XCTAssertTrue(button.waitForExistence(timeout: 10))
            XCTAssertTrue(waitUntilEnabled(button))
            button.click()
            XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
            app.typeKey("a", modifierFlags: .command)
            app.typeText(projectName)
            app.typeKey(.return, modifierFlags: [])
            let name = app.textFields["frame-name"]
            XCTAssertTrue(name.waitForExistence(timeout: 10))
            XCTAssertTrue(waitUntilEnabled(name))
        }

        func setFrame(_ value: String, projectName: String) {
            let name = app.textFields["frame-name"]
            name.click()
            name.typeKey("a", modifierFlags: .command)
            name.typeText(value)
            app.buttons["apply-frame"].click()
            XCTAssertTrue(waitForFrame(base.appendingPathComponent("projects/\(projectName)"), name: value))
            XCTAssertTrue(app.staticTexts["Сохранено"].waitForExistence(timeout: 10))
        }

        func open(_ projectName: String) {
            let url = base.appendingPathComponent("projects/\(projectName)").standardizedFileURL.resolvingSymlinksInPath()
            let row = app.descendants(matching: .any)["recent-project-\(url.path)"].firstMatch
            XCTAssertTrue(row.waitForExistence(timeout: 10))
            XCTAssertTrue(waitUntilEnabled(row))
            row.click()
            XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))
            XCTAssertTrue(waitUntilEnabled(app.textFields["frame-name"]))
        }

        create("Project A")
        setFrame("Frame A", projectName: "Project A")
        create("Project B")
        setFrame("Frame B", projectName: "Project B")
        open("Project A")
        XCTAssertEqual(app.textFields["frame-name"].value as? String, "Frame A")
        open("Project B")
        XCTAssertEqual(app.textFields["frame-name"].value as? String, "Frame B")

        app.terminate()
        app.launch()
        open("Project A")
        XCTAssertEqual(app.textFields["frame-name"].value as? String, "Frame A")
        open("Project B")
        XCTAssertEqual(app.textFields["frame-name"].value as? String, "Frame B")
    }

    func testRetentionLimitRequiresConfirmationAndPersistsOnlyAfterAcceptance() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-retention-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let settings = base.appendingPathComponent("support/AIConstructor/retention-settings.json")
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.typeKey(",", modifierFlags: .command)
        let finite = app.radioButtons["Ограничить количеством"]
        XCTAssertTrue(finite.waitForExistence(timeout: 10))
        finite.click()
        let count = app.textFields["retention-count"]
        XCTAssertTrue(count.waitForExistence(timeout: 5))
        count.click()
        count.typeKey("a", modifierFlags: .command)
        count.typeText("2")
        app.buttons["retention-apply"].click()
        let warning = app.alerts.firstMatch
        XCTAssertTrue(warning.waitForExistence(timeout: 5))
        XCTAssertTrue(warning.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "поздние изменения")).firstMatch.exists)
        warning.buttons["Отмена"].click()
        XCTAssertFalse(FileManager.default.fileExists(atPath: settings.path))
        app.buttons["retention-apply"].click()
        XCTAssertTrue(warning.waitForExistence(timeout: 5))
        warning.buttons["Применить лимит"].click()
        let predicate = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            guard let data = try? Data(contentsOf: settings),
                  let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return false }
            return object["limit"] as? Int == 2 && object["riskAccepted"] as? Bool == true
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [predicate], timeout: 5), .completed)
        let accepted = try Data(contentsOf: settings)
        count.click()
        count.typeKey("a", modifierFlags: .command)
        count.typeText("0")
        app.buttons["retention-apply"].click()
        XCTAssertTrue(warning.waitForExistence(timeout: 5))
        warning.buttons["Отмена"].click()
        XCTAssertEqual(try Data(contentsOf: settings), accepted)
        app.terminate()
        app.launch()
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.typeKey(",", modifierFlags: .command)
        XCTAssertTrue(app.textFields["retention-count"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.textFields["retention-count"].value as? String, "2")
        app.typeKey("w", modifierFlags: .command)
        let create = app.buttons["new-project"]
        XCTAssertTrue(create.waitForExistence(timeout: 5))
        create.click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        let frame = app.textFields["frame-name"]
        XCTAssertTrue(frame.waitForExistence(timeout: 10))
        let project = base.appendingPathComponent("projects/Новый проект")
        for value in ["First", "Second", "Third"] {
            XCTAssertTrue(waitUntilEnabled(frame))
            frame.click()
            frame.typeKey("a", modifierFlags: .command)
            frame.typeText(value)
            app.buttons["apply-frame"].click()
            XCTAssertTrue(waitForFrame(project, name: value))
            XCTAssertTrue(app.staticTexts["Сохранено"].waitForExistence(timeout: 10))
        }
        let retained = project.appendingPathComponent(".aic-local/retained")
        let limited = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            guard let entries = try? FileManager.default.contentsOfDirectory(at: retained, includingPropertiesForKeys: nil) else { return false }
            return entries.filter { FileManager.default.fileExists(atPath: $0.appendingPathComponent("completed.json").path) }.count == 2
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [limited], timeout: 10), .completed)
    }
}
