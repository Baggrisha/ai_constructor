import Foundation
import Darwin
import CryptoKit
import XCTest

final class AIConstructorUITests: XCTestCase {
    private func waitUntilEnabled(_ element: XCUIElement, timeout: TimeInterval = 15) -> Bool {
        let condition = XCTNSPredicateExpectation(predicate: NSPredicate(format: "isEnabled == true"), object: element)
        return XCTWaiter.wait(for: [condition], timeout: timeout) == .completed
    }

    private func chooseFile(_ url: URL, in app: XCUIApplication) {
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey("g", modifierFlags: [.command, .shift])
        app.typeText(url.path)
        app.typeKey(.return, modifierFlags: [])
        if app.dialogs.firstMatch.exists || app.sheets.firstMatch.exists {
            app.typeKey(.return, modifierFlags: [])
        }
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

    func testRecentProjectRejectsDifferentIdentityAtSamePath() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-identity-ui-\(UUID().uuidString)", isDirectory: true)
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
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))

        let project = base.appendingPathComponent("projects/Новый проект").standardizedFileURL.resolvingSymlinksInPath()
        let manifest = project.appendingPathComponent("project.json")
        var replacement = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: manifest)) as? [String: Any])
        replacement["projectId"] = UUID().uuidString.lowercased()
        let changed = try JSONSerialization.data(withJSONObject: replacement, options: [.prettyPrinted, .sortedKeys])
        try changed.write(to: manifest, options: .atomic)
        let row = app.descendants(matching: .any)["recent-project-\(project.path)"].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(row))
        row.click()
        let alert = app.alerts["Ошибка"]
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        XCTAssertTrue(alert.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Идентификатор проекта не совпадает")).firstMatch.exists)
        XCTAssertEqual(try Data(contentsOf: manifest), changed)
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

    func testDeferredFrameDraftSurvivesExternalResolution() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-draft-conflict-ui-\(UUID().uuidString)", isDirectory: true)
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
        nodes[0]["name"] = "External"
        document["nodes"] = nodes
        let external = try JSONSerialization.data(withJSONObject: document, options: [.prettyPrinted, .sortedKeys])
        try external.write(to: path, options: .atomic)
        app.descendants(matching: .any)["panel-diagnostics"].firstMatch.click()
        let review = app.buttons["Просмотреть и разрешить…"]
        XCTAssertTrue(review.waitForExistence(timeout: 20))
        XCTAssertFalse(review.isEnabled)
        let deferDraft = app.buttons["defer-frame-draft"]
        XCTAssertTrue(deferDraft.waitForExistence(timeout: 5))
        deferDraft.click()
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
        XCTAssertTrue(waitForFrame(project, name: "External", width: 1440, height: 900))
        app.descendants(matching: .any)["panel-inspector"].firstMatch.click()
        XCTAssertEqual(app.textFields["frame-name"].value as? String, "External")
        XCTAssertEqual(width.value as? String, "0")
        XCTAssertTrue(app.staticTexts["Проверьте отложенный черновик и нажмите «Применить»."].exists)
        XCTAssertTrue(waitForFrame(project, name: "External", width: 1440, height: 900))
        width.click()
        width.typeKey("a", modifierFlags: .command)
        width.typeText("800")
        let pause = expectation(description: "a rebased draft is not autosaved before review")
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { pause.fulfill() }
        wait(for: [pause], timeout: 3)
        XCTAssertTrue(waitForFrame(project, name: "External", width: 1440, height: 900))
        app.buttons["apply-frame"].click()
        XCTAssertTrue(waitForFrame(project, name: "External", width: 800, height: 900))
    }

    func testUnreadablePanelSettingsDoNotBlockNavigationOrGetOverwritten() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-panel-settings-ui-\(UUID().uuidString)", isDirectory: true)
        let support = base.appendingPathComponent("support/AIConstructor", isDirectory: true)
        try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let settings = support.appendingPathComponent("panel-settings.json")
        let original = Data("{invalid settings".utf8)
        try original.write(to: settings)
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }
        let initialAlert = app.alerts["Ошибка"]
        XCTAssertTrue(initialAlert.waitForExistence(timeout: 10))
        initialAlert.buttons["OK"].click()
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.buttons["new-project"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))
        app.descendants(matching: .any)["panel-diagnostics"].firstMatch.click()
        let warning = app.alerts["Ошибка"]
        XCTAssertTrue(warning.waitForExistence(timeout: 10))
        warning.buttons["OK"].click()
        XCTAssertTrue(app.staticTexts["Диагностика проекта"].waitForExistence(timeout: 10))
        XCTAssertEqual(try Data(contentsOf: settings), original)
    }

    func testRejectedUndoReportsErrorWithoutMarkingCleanProjectDirty() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-undo-error-ui-\(UUID().uuidString)", isDirectory: true)
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
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))
        let undo = app.buttons["Отменить"]
        XCTAssertTrue(waitUntilEnabled(undo))
        undo.click()
        let alert = app.alerts["Ошибка"]
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        XCTAssertTrue(alert.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "UNSUPPORTED_FEATURE")).firstMatch.exists)
        alert.buttons["OK"].click()
        XCTAssertTrue(app.staticTexts["Сохранено"].waitForExistence(timeout: 10))
    }

    func testFutureManifestOpensReadOnlyWithoutDowngrade() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-future-ui-\(UUID().uuidString)", isDirectory: true)
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
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))
        app.terminate()

        let project = base.appendingPathComponent("projects/Новый проект").standardizedFileURL.resolvingSymlinksInPath()
        let manifest = project.appendingPathComponent("project.json")
        var fields = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: manifest)) as? [String: Any])
        fields["schemaVersion"] = 999
        let future = try JSONSerialization.data(withJSONObject: fields, options: [.prettyPrinted, .sortedKeys])
        try future.write(to: manifest, options: .atomic)
        app.launch()
        let row = app.descendants(matching: .any)["recent-project-\(project.path)"].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.click()
        XCTAssertTrue(app.staticTexts["Только чтение"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.textFields["frame-name"].isEnabled)
        XCTAssertEqual(try Data(contentsOf: manifest), future)
    }

    func testPortableSnapshotPreservesIdentityAndChatsWithoutLocalState() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-portable-ui-\(UUID().uuidString)", isDirectory: true)
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
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))
        app.descendants(matching: .any)["panel-chats"].firstMatch.click()
        let newChat = app.buttons["Новый чат"]
        XCTAssertTrue(newChat.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(newChat))
        newChat.click()
        let title = app.textFields["chat-title"]
        XCTAssertTrue(title.waitForExistence(timeout: 5))
        title.click()
        title.typeText("Portable notes")
        app.buttons["confirm-chat"].click()
        let message = app.textViews["chat-message"]
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(message))
        message.click()
        message.typeText("Portable message")
        app.buttons["save-chat-message"].click()
        XCTAssertTrue(app.staticTexts["Portable message"].waitForExistence(timeout: 10))

        let actions = app.descendants(matching: .any)["project-actions"].firstMatch
        XCTAssertTrue(actions.waitForExistence(timeout: 10))
        actions.click()
        app.menuItems["Создать переносимый снимок…"].click()
        let warning = app.alerts["Создать переносимый снимок?"]
        XCTAssertTrue(warning.waitForExistence(timeout: 5))
        XCTAssertTrue(warning.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "приватные")).firstMatch.exists)
        XCTAssertTrue(warning.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "архивы конфликтов")).firstMatch.exists)
        warning.buttons["Продолжить"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        let completed = app.alerts["Переносимый снимок создан"]
        XCTAssertTrue(completed.waitForExistence(timeout: 20))
        completed.buttons["OK"].click()

        let original = base.appendingPathComponent("projects/Новый проект")
        let portable = base.appendingPathComponent("projects/Новый проект — снимок")
        let source = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: original.appendingPathComponent("project.json"))) as? [String: Any])
        let copy = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: portable.appendingPathComponent("project.json"))) as? [String: Any])
        XCTAssertEqual(source["projectId"] as? String, copy["projectId"] as? String)
        for name in [".git", ".aic-local", ".cache"] {
            XCTAssertFalse(FileManager.default.fileExists(atPath: portable.appendingPathComponent(name).path))
        }
        let chats = try FileManager.default.contentsOfDirectory(at: portable.appendingPathComponent("chats"), includingPropertiesForKeys: nil)
        let texts = try chats.map { try String(contentsOf: $0.appendingPathComponent("messages.jsonl"), encoding: .utf8) }
        XCTAssertTrue(texts.contains { $0.contains("Portable message") })
    }

    func testSavedFocusedFrameDoesNotDisableProjectActions() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-focused-actions-ui-\(UUID().uuidString)", isDirectory: true)
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
        name.click()
        name.typeKey("a", modifierFlags: .command)
        name.typeText("Saved focused frame")
        XCTAssertTrue(waitForFrame(base.appendingPathComponent("projects/Новый проект"), name: "Saved focused frame"))
        XCTAssertTrue(app.staticTexts["Сохранено"].waitForExistence(timeout: 10))
        app.descendants(matching: .any)["project-actions"].firstMatch.click()
        XCTAssertTrue(app.menuItems["Дублировать проект…"].isEnabled)
        XCTAssertTrue(app.menuItems["Создать переносимый снимок…"].isEnabled)
        XCTAssertTrue(app.menuItems["Создать контрольную точку"].isEnabled)
    }
    func testCheckpointRestoreCreatesRevisionAndPreservesChatsAfterReopen() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-checkpoint-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let attachment = base.appendingPathComponent("attachment.svg")
        let attachmentBytes = Data("<svg xmlns=\"http://www.w3.org/2000/svg\"><rect width=\"10\" height=\"10\"/></svg>".utf8)
        try attachmentBytes.write(to: attachment)
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.buttons["new-project"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])

        let project = base.appendingPathComponent("projects/Новый проект").standardizedFileURL.resolvingSymlinksInPath()
        let name = app.textFields["frame-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 10))
        name.click()
        name.typeKey("a", modifierFlags: .command)
        name.typeText("Checkpoint frame")
        XCTAssertTrue(waitForFrame(project, name: "Checkpoint frame"))
        XCTAssertTrue(app.staticTexts["Сохранено"].waitForExistence(timeout: 10))

        let actions = app.descendants(matching: .any)["project-actions"].firstMatch
        XCTAssertTrue(waitUntilEnabled(actions))
        actions.click()
        let create = app.menuItems["Создать контрольную точку"]
        XCTAssertTrue(create.waitForExistence(timeout: 5))
        XCTAssertTrue(create.isEnabled)
        create.click()
        let checkpointDirectory = project.appendingPathComponent("checkpoints")
        func checkpointFiles() -> [URL] {
            ((try? FileManager.default.contentsOfDirectory(at: checkpointDirectory, includingPropertiesForKeys: nil)) ?? [])
                .filter { $0.pathExtension == "json" }
        }
        let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            checkpointFiles().count == 1
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 15), .completed)
        let checkpoint = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: try XCTUnwrap(checkpointFiles().first))) as? [String: Any])
        let checkpointId = try XCTUnwrap(checkpoint["id"] as? String)
        let checkpointRevision = try XCTUnwrap(checkpoint["revision"] as? NSNumber).uint64Value
        let assetsPanel = app.descendants(matching: .any)["panel-assets"].firstMatch
        XCTAssertTrue(waitUntilEnabled(assetsPanel))
        assetsPanel.click()
        let importButton = app.buttons["import-asset"]
        XCTAssertTrue(importButton.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(importButton))
        importButton.click()
        chooseFile(attachment, in: app)
        let documentFile = project.appendingPathComponent("document.json")
        func firstAsset() -> [String: Any]? {
            guard let data = try? Data(contentsOf: documentFile),
                  let document = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
                  let assets = document["assets"] as? [[String: Any]] else { return nil }
            return assets.first
        }
        let imported = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in firstAsset() != nil }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [imported], timeout: 15), .completed)
        XCTAssertTrue(waitUntilEnabled(importButton))
        let savedAsset = try XCTUnwrap(firstAsset())
        let assetId = try XCTUnwrap(savedAsset["id"] as? String)
        let assetPath = try XCTUnwrap(savedAsset["path"] as? String)

        app.descendants(matching: .any)["panel-chats"].firstMatch.click()
        let newChat = app.buttons["Новый чат"]
        XCTAssertTrue(newChat.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(newChat))
        newChat.click()
        let title = app.textFields["chat-title"]
        XCTAssertTrue(title.waitForExistence(timeout: 5))
        title.click()
        title.typeText("After checkpoint")
        app.buttons["confirm-chat"].click()
        let message = app.textViews["chat-message"]
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(message))
        message.click()
        message.typeText("Keep this message")
        let attachmentPicker = app.popUpButtons["Вложение"]
        XCTAssertTrue(attachmentPicker.waitForExistence(timeout: 5))
        attachmentPicker.click()
        app.menuItems[assetPath].click()
        app.buttons["save-chat-message"].click()
        XCTAssertTrue(app.staticTexts["Keep this message"].waitForExistence(timeout: 10))
        let chatDirectories = try FileManager.default.contentsOfDirectory(at: project.appendingPathComponent("chats"), includingPropertiesForKeys: nil)
        let chat = try XCTUnwrap(chatDirectories.first)
        let messagesFile = chat.appendingPathComponent("messages.jsonl")
        let messagesBefore = try Data(contentsOf: messagesFile)
        XCTAssertTrue(String(decoding: messagesBefore, as: UTF8.self).contains("Keep this message"))

        app.descendants(matching: .any)["panel-inspector"].firstMatch.click()
        XCTAssertTrue(name.waitForExistence(timeout: 10))
        name.click()
        name.typeKey("a", modifierFlags: .command)
        name.typeText("Later frame")
        XCTAssertTrue(waitForFrame(project, name: "Later frame"))
        XCTAssertTrue(app.staticTexts["Сохранено"].waitForExistence(timeout: 10))
        let before = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: documentFile)) as? [String: Any])
        let revisionBefore = try XCTUnwrap(before["revision"] as? NSNumber).uint64Value
        XCTAssertGreaterThan(revisionBefore, checkpointRevision)

        func chooseCheckpoint() {
            XCTAssertTrue(waitUntilEnabled(actions))
            actions.click()
            let restore = app.menuItems["Восстановить контрольную точку"]
            XCTAssertTrue(restore.waitForExistence(timeout: 5))
            XCTAssertTrue(restore.isEnabled)
            restore.click()
            let entry = app.menuItems["Ревизия \(checkpointRevision) · \(checkpointId.prefix(8))"]
            XCTAssertTrue(entry.waitForExistence(timeout: 5))
            entry.click()
        }
        chooseCheckpoint()
        let confirmation = app.alerts["Восстановить контрольную точку?"]
        XCTAssertTrue(confirmation.waitForExistence(timeout: 5))
        confirmation.buttons["Отмена"].click()
        XCTAssertTrue(waitForFrame(project, name: "Later frame"))
        chooseCheckpoint()
        XCTAssertTrue(confirmation.waitForExistence(timeout: 5))
        confirmation.buttons["Восстановить"].click()
        XCTAssertTrue(waitForFrame(project, name: "Checkpoint frame"))
        XCTAssertTrue(app.staticTexts["Сохранено"].waitForExistence(timeout: 10))
        let restoredName = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == %@", "Checkpoint frame"), object: name)
        XCTAssertEqual(XCTWaiter.wait(for: [restoredName], timeout: 10), .completed)
        let restored = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: documentFile)) as? [String: Any])
        XCTAssertGreaterThan(try XCTUnwrap(restored["revision"] as? NSNumber).uint64Value, revisionBefore)
        let restoredRevision = try XCTUnwrap(restored["revision"] as? NSNumber).uint64Value
        let settle = expectation(description: "old focused field cannot autosave after restore")
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { settle.fulfill() }
        wait(for: [settle], timeout: 3)
        let settled = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: documentFile)) as? [String: Any])
        XCTAssertEqual(try XCTUnwrap(settled["revision"] as? NSNumber).uint64Value, restoredRevision)
        XCTAssertTrue(waitForFrame(project, name: "Checkpoint frame"))
        XCTAssertEqual(try Data(contentsOf: messagesFile), messagesBefore)
        let restoredAssets = try XCTUnwrap(restored["assets"] as? [[String: Any]])
        XCTAssertTrue(restoredAssets.contains { $0["id"] as? String == assetId && $0["path"] as? String == assetPath })
        XCTAssertEqual(try Data(contentsOf: project.appendingPathComponent(assetPath)), attachmentBytes)
        XCTAssertEqual(name.value as? String, "Checkpoint frame")
        let undo = app.buttons["Отменить"]
        XCTAssertTrue(waitUntilEnabled(undo))
        undo.click()
        XCTAssertTrue(waitForFrame(project, name: "Later frame"))
        let redo = app.buttons["Повторить"]
        XCTAssertTrue(waitUntilEnabled(redo))
        redo.click()
        XCTAssertTrue(waitForFrame(project, name: "Checkpoint frame"))
        let redoSettled = expectation(description: "old focused field cannot autosave after redo")
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { redoSettled.fulfill() }
        wait(for: [redoSettled], timeout: 3)
        XCTAssertTrue(waitForFrame(project, name: "Checkpoint frame"))
        XCTAssertEqual(try Data(contentsOf: messagesFile), messagesBefore)

        app.terminate()
        app.launch()
        let row = app.descendants(matching: .any)["recent-project-\(project.path)"].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.click()
        XCTAssertTrue(name.waitForExistence(timeout: 10))
        XCTAssertEqual(name.value as? String, "Checkpoint frame")
        app.descendants(matching: .any)["panel-chats"].firstMatch.click()
        XCTAssertTrue(app.staticTexts["Keep this message"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts[assetPath].waitForExistence(timeout: 10))
    }

    func testMissingAssetCanBeRelinkedAfterReopen() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-asset-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let original = base.appendingPathComponent("original.svg")
        let replacement = base.appendingPathComponent("replacement.svg")
        try Data("<svg xmlns=\"http://www.w3.org/2000/svg\"><rect width=\"10\" height=\"10\"/></svg>".utf8).write(to: original)
        let replacementData = Data("<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>".utf8)
        try replacementData.write(to: replacement)
        let app = XCUIApplication()
        app.launchEnvironment["AIC_APPLICATION_SUPPORT"] = base.appendingPathComponent("support").path
        app.launchEnvironment["AIC_PROJECTS_DIRECTORY"] = base.appendingPathComponent("projects").path
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["new-project"].waitForExistence(timeout: 10))
        app.buttons["new-project"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))
        let project = base.appendingPathComponent("projects/Новый проект").standardizedFileURL.resolvingSymlinksInPath()
        let actions = app.descendants(matching: .any)["project-actions"].firstMatch
        XCTAssertTrue(waitUntilEnabled(actions))
        actions.click()
        app.menuItems["Создать контрольную точку"].click()
        let checkpoint = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let paths = (try? FileManager.default.contentsOfDirectory(at: project.appendingPathComponent("checkpoints"), includingPropertiesForKeys: nil)) ?? []
            return paths.contains { $0.pathExtension == "json" }
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [checkpoint], timeout: 15), .completed)
        XCTAssertTrue(waitUntilEnabled(actions))
        actions.click()
        XCTAssertTrue(waitUntilEnabled(app.menuItems["Восстановить контрольную точку"]))
        app.typeKey(.escape, modifierFlags: [])

        app.descendants(matching: .any)["panel-assets"].firstMatch.click()
        let importButton = app.buttons["import-asset"]
        XCTAssertTrue(importButton.waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntilEnabled(importButton))
        importButton.click()
        chooseFile(original, in: app)
        let documentFile = project.appendingPathComponent("document.json")
        func asset() -> [String: Any]? {
            guard let data = try? Data(contentsOf: documentFile),
                  let document = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
                  let assets = document["assets"] as? [[String: Any]] else { return nil }
            return assets.first
        }
        let imported = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in asset() != nil }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [imported], timeout: 15), .completed)
        XCTAssertTrue(waitUntilEnabled(importButton))
        let importedAsset = try XCTUnwrap(asset())
        let assetId = try XCTUnwrap(importedAsset["id"] as? String)
        let oldPath = try XCTUnwrap(importedAsset["path"] as? String)
        app.terminate()
        try FileManager.default.removeItem(at: project.appendingPathComponent(oldPath))

        app.launch()
        let row = app.descendants(matching: .any)["recent-project-\(project.path)"].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.click()
        let repair = app.buttons["Восстановить…"]
        XCTAssertTrue(repair.waitForExistence(timeout: 20))
        XCTAssertTrue(waitUntilEnabled(repair))
        repair.click()
        let confirmation = app.alerts["Восстановить ресурс?"]
        XCTAssertTrue(confirmation.waitForExistence(timeout: 5))
        confirmation.buttons["Выбрать замену…"].click()
        chooseFile(replacement, in: app)
        let relinked = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            guard let current = asset() else { return false }
            return current["id"] as? String == assetId && current["path"] as? String != oldPath
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [relinked], timeout: 15), .completed)
        let newPath = try XCTUnwrap(asset()?["path"] as? String)
        XCTAssertEqual(try Data(contentsOf: project.appendingPathComponent(newPath)), replacementData)
        XCTAssertTrue(app.buttons["Перепривязать…"].waitForExistence(timeout: 15))
        XCTAssertTrue(waitUntilEnabled(actions))
        actions.click()
        XCTAssertTrue(waitUntilEnabled(app.menuItems["Восстановить контрольную точку"]))
    }

    func testPortableSnapshotKeepsGeneratedProtectionAfterConflictResolution() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-generated-ui-\(UUID().uuidString)", isDirectory: true)
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
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))
        let project = base.appendingPathComponent("projects/Новый проект")
        let external = Data("external generated source".utf8)
        try external.write(to: project.appendingPathComponent("src/generated/external.tsx"), options: .atomic)

        app.descendants(matching: .any)["panel-diagnostics"].firstMatch.click()
        let review = app.buttons["Просмотреть и разрешить…"]
        XCTAssertTrue(review.waitForExistence(timeout: 20))
        XCTAssertTrue(waitUntilEnabled(review))
        let actions = app.descendants(matching: .any)["project-actions"].firstMatch
        actions.click()
        XCTAssertFalse(app.menuItems["Создать переносимый снимок…"].isEnabled)
        app.typeKey(.escape, modifierFlags: [])
        review.click()
        let picker = app.popUpButtons["Использовать"].firstMatch
        XCTAssertTrue(picker.waitForExistence(timeout: 10))
        picker.click()
        app.menuItems["Версия на диске"].click()
        app.buttons["Применить выбор…"].click()
        let apply = app.buttons["Применить"]
        XCTAssertTrue(apply.waitForExistence(timeout: 5))
        apply.click()
        let warning = app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "защита сохраняется в переносимом снимке")).firstMatch
        XCTAssertTrue(warning.waitForExistence(timeout: 20))
        XCTAssertTrue(waitUntilEnabled(actions))
        actions.click()
        let snapshot = app.menuItems["Создать переносимый снимок…"]
        XCTAssertTrue(snapshot.waitForExistence(timeout: 5))
        XCTAssertTrue(snapshot.isEnabled)
        snapshot.click()
        let confirmation = app.alerts["Создать переносимый снимок?"]
        XCTAssertTrue(confirmation.waitForExistence(timeout: 5))
        confirmation.buttons["Продолжить"].click()
        XCTAssertTrue(app.dialogs.firstMatch.waitForExistence(timeout: 5) || app.sheets.firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.return, modifierFlags: [])
        let completed = app.alerts["Переносимый снимок создан"]
        XCTAssertTrue(completed.waitForExistence(timeout: 20))
        completed.buttons["OK"].click()

        let portable = base.appendingPathComponent("projects/Новый проект — снимок")
        XCTAssertEqual(try Data(contentsOf: portable.appendingPathComponent("src/generated/external.tsx")), external)
        XCTAssertFalse(FileManager.default.fileExists(atPath: portable.appendingPathComponent(".aic-local").path))
        let manifest = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: portable.appendingPathComponent("project.json"))) as? [String: Any])
        let files = try XCTUnwrap(manifest["files"] as? [String: Any])
        XCTAssertTrue(files.keys.contains { $0.hasPrefix("history/generated-blocks") })
    }

    func testLegacyCheckpointRequiresConfirmationForMissingFileModes() throws {
        continueAfterFailure = false
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("aic-legacy-mode-ui-\(UUID().uuidString)", isDirectory: true)
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
        XCTAssertTrue(app.textFields["frame-name"].waitForExistence(timeout: 10))
        let project = base.appendingPathComponent("projects/Новый проект").standardizedFileURL.resolvingSymlinksInPath()
        let manifestFile = project.appendingPathComponent("project.json")
        let relative = "src/user/missing-script.sh"
        let source = project.appendingPathComponent(relative)
        let contents = Data("#!/bin/sh\necho fixture\n".utf8)
        try contents.write(to: source, options: .atomic)
        app.descendants(matching: .any)["panel-diagnostics"].firstMatch.click()

        func manifestContainsSource() -> Bool {
            guard let data = try? Data(contentsOf: manifestFile),
                  let manifest = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
                  let files = manifest["files"] as? [String: Any] else { return false }
            return files[relative] != nil
        }
        func selectDiskVersion() {
            let review = app.buttons["Просмотреть и разрешить…"]
            XCTAssertTrue(review.waitForExistence(timeout: 20))
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
        }
        selectDiskVersion()
        let tracked = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in manifestContainsSource() }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [tracked], timeout: 15), .completed)
        let actions = app.descendants(matching: .any)["project-actions"].firstMatch
        XCTAssertTrue(waitUntilEnabled(actions))
        actions.click()
        let create = app.menuItems["Создать контрольную точку"]
        XCTAssertTrue(waitUntilEnabled(create))
        create.click()
        let checkpointDirectory = project.appendingPathComponent("checkpoints")
        func checkpointFiles() -> [URL] {
            ((try? FileManager.default.contentsOfDirectory(at: checkpointDirectory, includingPropertiesForKeys: nil)) ?? [])
                .filter { $0.pathExtension == "json" }
        }
        let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in checkpointFiles().count == 1 }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 15), .completed)
        XCTAssertTrue(waitUntilEnabled(actions))
        try FileManager.default.removeItem(at: source)
        selectDiskVersion()
        let removed = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in !manifestContainsSource() }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [removed], timeout: 15), .completed)
        XCTAssertTrue(waitUntilEnabled(actions))
        app.terminate()

        let checkpointFile = try XCTUnwrap(checkpointFiles().first)
        var checkpoint = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: checkpointFile)) as? [String: Any])
        let checkpointId = try XCTUnwrap(checkpoint["id"] as? String)
        let checkpointRevision = try XCTUnwrap(checkpoint["revision"] as? NSNumber).uint64Value
        XCTAssertNotNil(checkpoint.removeValue(forKey: "executableModes"))
        let legacy = try JSONSerialization.data(withJSONObject: checkpoint, options: [.prettyPrinted, .sortedKeys])
        try legacy.write(to: checkpointFile, options: .atomic)
        var manifest = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: manifestFile)) as? [String: Any])
        var files = try XCTUnwrap(manifest["files"] as? [String: Any])
        files["checkpoints/\(checkpointId).json"] = SHA256.hash(data: legacy).map { String(format: "%02x", $0) }.joined()
        manifest["files"] = files
        try JSONSerialization.data(withJSONObject: manifest, options: [.prettyPrinted, .sortedKeys]).write(to: manifestFile, options: .atomic)
        let localState = project.appendingPathComponent(".aic-local")
        if FileManager.default.fileExists(atPath: localState.path) { try FileManager.default.removeItem(at: localState) }

        app.launch()
        let row = app.descendants(matching: .any)["recent-project-\(project.path)"].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.click()
        func startRestore() {
            XCTAssertTrue(waitUntilEnabled(actions))
            actions.click()
            let restore = app.menuItems["Восстановить контрольную точку"]
            XCTAssertTrue(waitUntilEnabled(restore))
            restore.click()
            let entry = app.menuItems["Ревизия \(checkpointRevision) · \(checkpointId.prefix(8))"]
            XCTAssertTrue(entry.waitForExistence(timeout: 5))
            entry.click()
            let confirmation = app.alerts["Восстановить контрольную точку?"]
            XCTAssertTrue(confirmation.waitForExistence(timeout: 5))
            confirmation.buttons["Восстановить"].click()
        }
        startRestore()
        let modeConfirmation = app.alerts["Восстановить файлы без права запуска?"]
        XCTAssertTrue(modeConfirmation.waitForExistence(timeout: 10))
        XCTAssertTrue(modeConfirmation.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Права существующих файлов сохранятся")).firstMatch.exists)
        modeConfirmation.buttons["Отмена"].click()
        XCTAssertFalse(FileManager.default.fileExists(atPath: source.path))
        startRestore()
        XCTAssertTrue(modeConfirmation.waitForExistence(timeout: 10))
        modeConfirmation.buttons["Восстановить без права запуска"].click()
        let restored = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            (try? Data(contentsOf: source)) == contents
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [restored], timeout: 15), .completed)
        let attributes = try FileManager.default.attributesOfItem(atPath: source.path)
        XCTAssertEqual((try XCTUnwrap(attributes[.posixPermissions] as? NSNumber).intValue) & 0o111, 0)
        XCTAssertTrue(waitUntilEnabled(app.buttons["Отменить"]))
        app.buttons["Отменить"].click()
        let undone = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            !FileManager.default.fileExists(atPath: source.path)
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [undone], timeout: 15), .completed)
        XCTAssertTrue(waitUntilEnabled(app.buttons["Повторить"]))
        app.buttons["Повторить"].click()
        let redone = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            (try? Data(contentsOf: source)) == contents
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [redone], timeout: 15), .completed)
    }
}
