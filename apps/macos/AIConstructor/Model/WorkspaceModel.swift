import AppKit
import CoreServices
import SwiftUI


@MainActor
final class WorkspaceModel: ObservableObject {
    @Published var snapshot: Snapshot?
    @Published var checkpoints: [Checkpoint] = []
    @Published var checkpointAwaitingModeConfirmation: Checkpoint?
    @Published var chats: [Chat] = []
    @Published var messages: [ChatMessage] = []
    @Published var chatSearchResults: [ChatSearchHit]?
    @Published var chatDrafts: [String: String] = [:]
    @Published var chatAttachmentDrafts: [String: String] = [:]
    @Published var selectedChatId: String?
    @Published var diagnostics: ProjectDiagnostics?
    @Published var diagnosticsError: String?
    @Published var error: String?
    @Published var notice: String?
    @Published var watchWarning: String?
    @Published var search = ""
    @Published private(set) var operation: String?
    @Published private(set) var canCancelOperation = false
    @Published private(set) var cancellationMessage: String?
    @Published private(set) var diagnosticsInFlight = false
    @Published var conflictReviewPresented = false
    @Published var chatEditorPresented = false
    @Published private(set) var frameDraft: FrameDraft?
    @Published private(set) var frameDraftNeedsReview = false
    @Published private(set) var deferredFrameDraft: DeferredFrameDraft?
    @Published private(set) var frameEditorId = UUID()
    @Published private(set) var canvas: CanvasContent = .empty
    @Published private(set) var canvasGenerating = false
    @Published var selectedNodeIds: Set<String> = []
    @Published var canvasHoveredId: String?
    @Published var canvasNotice: String?
    @Published private(set) var canvasPreview: CanvasRect?
    @Published private(set) var canvasRotation: Double?
    private var canvasTaskState: CanvasTaskState?
    private var canvasQueued = false
    private var canvasScale = 0.0
    private var pendingGesture: CanvasGesture?
    private var operationControl: OperationControl?
    private var diagnosticControl: OperationControl?
    private var possibleUnsaved = false

    var writesBlocked: Bool { snapshot?.readOnly == true || diagnostics?.readOnly == true }

    var hasBlockingFrameDraft: Bool {
        guard let frameDraft else { return false }
        guard let root = snapshot?.root else { return true }
        return frameDraftNeedsReview || frameDraft != FrameDraft(root) || snapshot?.dirty == true || possibleUnsaved
    }

    var hasUnsavedData: Bool {
        if snapshot?.dirty == true || possibleUnsaved || deferredFrameDraft != nil || frameDraftNeedsReview { return true }
        guard let frameDraft else { return false }
        guard let root = snapshot?.root else { return true }
        return !frameDraft.matches(root)
    }

    var closeBlockReason: String? {
        if operation != nil { return "Дождитесь завершения операции перед закрытием окна или приложения." }
        if diagnosticsInFlight { return "Проверка проекта завершается. Дождитесь результата и повторите закрытие." }
        if deferredFrameDraft != nil {
            return "Верните отложенный черновик фрейма и завершите ввод перед закрытием."
        }
        if frameDraftNeedsReview {
            return "Проверьте и примените отложенный черновик фрейма либо явно отбросьте его перед закрытием."
        }
        if let frameDraft, snapshot?.root.map({ frameDraft != FrameDraft($0) }) ?? true {
            return "Завершите ввод или явно отбросьте черновик фрейма перед закрытием."
        }
        if hasUnsavedData {
            return "Проект содержит несохранённые изменения. Сохраните их перед закрытием окна или приложения."
        }
        if chatDrafts.values.contains(where: { !$0.isEmpty }) || chatAttachmentDrafts.values.contains(where: { !$0.isEmpty }) {
            return "В чатах есть несохранённые сообщения или вложения. Сохраните их либо очистите черновики перед закрытием."
        }
        return nil
    }
    private var sessions: [String: ProjectSession] = [:]
    private var sessionProjectIds: [String: String] = [:]
    private var currentPath: URL?
    private var deferredChatLoad = false
    private var deferredChatId: String?
    private var projectWatcher: ProjectFileWatcher?
    private var observationTask: Task<Void, Never>?
    private var observationPending = false
    private let library = ProjectLibrary.shared

    private var session: ProjectSession? {
        guard let currentPath else { return nil }
        return sessions[currentPath.path]
    }

    func setRetentionLimit(_ limit: UInt32?) {
        for session in sessions.values { session.setRetentionLimit(limit: limit) }
    }

    deinit {
        observationTask?.cancel()
        projectWatcher?.stop()
        let paths = Array(sessions.keys)
        Task { @MainActor in
            for path in paths { ProjectLibrary.shared.release(path) }
        }
    }

    private func perform<T>(
        _ title: String,
        control: OperationControl? = nil,
        errorContext: String? = nil,
        work: @escaping () throws -> T,
        failure: @escaping (Error) -> Void = { _ in },
        success: @escaping (T) -> Void
    ) {
        guard operation == nil else { return }
        operation = title
        operationControl = control
        canCancelOperation = control != nil
        cancellationMessage = nil
        Task {
            do {
                let value = try await Task.detached(priority: .userInitiated) { try work() }.value
                operation = nil
                operationControl = nil
                canCancelOperation = false
                cancellationMessage = nil
                success(value)
                drainDeferredWork()
            } catch {
                operation = nil
                operationControl = nil
                canCancelOperation = false
                cancellationMessage = nil
                failure(error)
                if wasCanceled(error) {
                    self.error = "Операция отменена до публикации изменений. Несохранённые данные остаются в памяти."
                } else {
                    self.error = [errorContext, readableError(error)].compactMap { $0 }.joined(separator: "\n")
                }
                drainDeferredWork()
            }
        }
    }

    private func drainDeferredWork() {
        guard operation == nil else { return }
        if canvasQueued { refreshCanvas() }
        if deferredChatLoad {
            let id = deferredChatId
            deferredChatLoad = false
            deferredChatId = nil
            refreshChats(select: id)
        } else if observationPending {
            watchChanged()
        }
    }

    func requestChats(select chatId: String? = nil) {
        if operation != nil {
            deferredChatLoad = true
            deferredChatId = chatId
        } else {
            refreshChats(select: chatId)
        }
    }

    func cancelOperation() {
        guard let control = operationControl, canCancelOperation else { return }
        switch control.requestCancel() {
        case .accepted:
            cancellationMessage = "Отмена запрошена. Ожидаем завершения операции."
        case .tooLate:
            cancellationMessage = "Публикация уже началась; дождитесь её завершения."
        case .finished:
            cancellationMessage = "Операция уже завершена; ожидаем результат."
        }
        canCancelOperation = false
    }

    private func watchChanged() {
        observationPending = true
        if diagnosticsInFlight {
            cancelBackgroundDiagnostics()
            return
        }
        guard operation == nil else { return }
        observationTask?.cancel()
        observationTask = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: 700_000_000) } catch { return }
            guard let self, self.operation == nil else { return }
            self.observationPending = false
            await self.pollDiagnostics()
        }
    }

    private func watchProject(_ url: URL) {
        observationTask?.cancel()
        projectWatcher?.stop()
        let watcher = ProjectFileWatcher(root: url.path) { [weak self] in
            Task { @MainActor in self?.watchChanged() }
        }
        projectWatcher = watcher
        watchWarning = watcher.start() ? nil : "Наблюдатель файлов недоступен; изменения проверяются периодически."
        watchChanged()
    }

    func cancelBackgroundDiagnostics() {
        if let diagnosticControl { _ = diagnosticControl.requestCancel() }
    }

    private func remember(_ url: URL, replacing oldPath: String? = nil) {
        guard let snapshot else { return }
        do {
            try library.updateRecents { recents in
                let previous = recents.first { $0.path == url.path || $0.path == oldPath }
                let projectId = snapshot.document.projectId.isEmpty ? previous?.projectId : snapshot.document.projectId
                let archived = previous?.archived ?? false
                recents.removeAll { $0.path == url.path || $0.path == oldPath }
                recents.insert(RecentProject(path: url.path, name: snapshot.name, projectId: projectId, archived: archived), at: 0)
            }
        } catch {
            self.error = "Проект открыт, но список последних проектов не сохранён: \(readableError(error))"
        }
    }

    private func install(_ loaded: LoadedProject, at url: URL, replacing oldPath: String? = nil, reserved: Bool = false) {
        if sessions[url.path] == nil && !reserved { _ = library.retain(url.path) }
        loaded.session.setRetentionLimit(limit: library.retentionLimit)
        sessions[url.path] = loaded.session
        sessionProjectIds[url.path] = loaded.snapshot.document.projectId
        currentPath = url
        invalidateFrameEditor()
        snapshot = loaded.snapshot
        possibleUnsaved = loaded.snapshot.dirty
        checkpoints = []
        checkpointAwaitingModeConfirmation = nil
        deferredChatLoad = false
        deferredChatId = nil
        chats = []
        chatSearchResults = nil
        messages = []
        selectedChatId = nil
        diagnostics = nil
        diagnosticsError = nil
        selectedNodeIds = []
        canvasHoveredId = nil
        canvasNotice = nil
        pendingGesture = nil
        canvasPreview = nil
        canvasTaskState?.task?.cancel()
        canvasTaskState = nil
        canvasQueued = false
        canvasGenerating = false
        canvas = .empty
        watchProject(url)
        remember(url, replacing: oldPath)
        refreshCheckpoints()
    }

    private func select(_ url: URL, expected: RecentProject? = nil, create: Bool = false) {
        guard operation == nil else { return }
        clearSavedFrameDraft()
        if frameDraft != nil || deferredFrameDraft != nil {
            error = "Дождитесь сохранения фрейма или исправьте его поля перед переключением проекта."
            return
        }
        let url = url.standardizedFileURL.resolvingSymlinksInPath()
        if (snapshot?.dirty == true || possibleUnsaved), let previous = session {
            let saveControl = newOperationControl()
            perform("Сохранение перед переключением…", control: saveControl, work: {
                try saveSnapshot(previous, control: saveControl)
            }) { result in
                self.accept(result)
                if result.error == nil && !result.snapshot.dirty {
                    self.select(url, expected: expected, create: create)
                }
            }
            return
        }
        let existing = sessions[url.path]
        let control = create || existing == nil ? newOperationControl() : nil
        let reserved = existing == nil
        if reserved && !library.retain(url.path) {
            error = "Проект сейчас перемещается в Корзину. Дождитесь завершения операции."
            return
        }
        perform(create ? "Создание проекта…" : "Открытие проекта…", control: control,
                errorContext: create ? "Не удалось создать проект. Можно выбрать другое расположение." : nil, work: {
            if let expected {
                guard let expectedId = expected.projectId, !expectedId.isEmpty else {
                    throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Идентификатор записи неизвестен. Откройте папку через «Открыть», чтобы подтвердить проект."])
                }
                let identity: ProjectIdentity
                do {
                    let manifest = try Data(contentsOf: url.appendingPathComponent("project.json"))
                    identity = try JSONDecoder().decode(ProjectIdentity.self, from: manifest)
                } catch {
                    throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Не удалось проверить идентификатор по манифесту. Для восстановления откройте папку непосредственно через «Открыть»: \(readableError(error))"])
                }
                guard identity.projectId == expectedId else {
                    throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Идентификатор проекта не совпадает; исходная запись сохранена."])
                }
            }
            let next: ProjectSession
            if let existing, !create {
                next = existing
            } else if let control {
                if create {
                    next = try createProjectControlled(path: url.path, name: url.lastPathComponent, control: control)
                } else {
                    next = try openProjectControlled(path: url.path, control: control)
                }
            } else {
                throw CocoaError(.fileReadUnknown)
            }
            let loaded = try loadProject(next, at: url)
            if let expected, expected.projectId != loaded.snapshot.document.projectId {
                throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Идентификатор проекта не совпадает; исходная запись сохранена."])
            }
            return loaded
        }, failure: { _ in
            if reserved { self.library.release(url.path) }
        }) { loaded in
            self.install(loaded, at: url, replacing: expected?.path, reserved: reserved)
        }
    }

    func openFirstRecent() {
        guard operation == nil, snapshot == nil, let first = library.recents.first(where: { $0.archived != true }) else { return }
        select(URL(fileURLWithPath: first.path), expected: first)
    }

    func openPath(_ url: URL) {
        guard operation == nil else { return }
        select(url)
    }

    func createAt(_ url: URL) {
        guard operation == nil else { return }
        select(url, create: true)
    }

    func create() {
        guard operation == nil else { return }
        let defaultFolder = ProcessInfo.processInfo.environment["AIC_PROJECTS_DIRECTORY"].map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("AI Constructor/Projects", isDirectory: true)
        let panel = NSSavePanel()
        panel.title = "Новый проект"
        panel.message = "Выберите имя и расположение папки проекта."
        do {
            try FileManager.default.createDirectory(at: defaultFolder, withIntermediateDirectories: true)
            guard FileManager.default.isWritableFile(atPath: defaultFolder.path) else {
                throw CocoaError(.fileWriteNoPermission)
            }
            panel.directoryURL = defaultFolder
        } catch {
            panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser
            panel.message = "Стандартная папка недоступна. Выберите другое расположение: \(error.localizedDescription)"
        }
        panel.nameFieldStringValue = "Новый проект"
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        select(url, create: true)
    }

    func chooseProject() {
        guard operation == nil else { return }
        let panel = NSOpenPanel()
        panel.title = "Открыть папку проекта"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        select(url)
    }

    func open(_ project: RecentProject) {
        select(URL(fileURLWithPath: project.path), expected: project)
    }

    func locate(_ project: RecentProject) {
        guard operation == nil else { return }
        let panel = NSOpenPanel()
        panel.title = "Указать расположение проекта"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        select(url, expected: project)
    }

    func setArchived(_ archived: Bool, project: RecentProject) {
        do {
            try library.updateRecents { recents in
                if let index = recents.firstIndex(where: { $0.path == project.path }) {
                    recents[index].archived = archived
                }
            }
        } catch { self.error = readableError(error) }
    }

    func archiveCurrent() {
        guard let currentPath, let project = library.recents.first(where: { $0.path == currentPath.path }) else { return }
        setArchived(true, project: project)
    }

    func removeRecent(_ project: RecentProject) {
        do { try library.updateRecents { $0.removeAll { $0.id == project.id } } }
        catch { self.error = readableError(error) }
    }

    func isSessionOpen(_ project: RecentProject) -> Bool {
        let path = URL(fileURLWithPath: project.path).standardizedFileURL.resolvingSymlinksInPath().path
        return sessions[path] != nil
    }

    private func finishClosing(_ path: String) {
        guard sessions.removeValue(forKey: path) != nil else { return }
        sessionProjectIds.removeValue(forKey: path)
        library.release(path)
        if currentPath?.path == path {
            observationTask?.cancel()
            projectWatcher?.stop()
            projectWatcher = nil
            observationPending = false
            watchWarning = nil
            currentPath = nil
            invalidateFrameEditor()
            snapshot = nil
            possibleUnsaved = false
            checkpoints = []
            checkpointAwaitingModeConfirmation = nil
            chats = []
            messages = []
            chatSearchResults = nil
            selectedChatId = nil
            diagnostics = nil
            diagnosticsError = nil
            selectedNodeIds = []
            canvasHoveredId = nil
            canvasNotice = nil
            pendingGesture = nil
            canvasPreview = nil
            canvasTaskState?.task?.cancel()
            canvasTaskState = nil
            canvasQueued = false
            canvasGenerating = false
            canvas = .empty
        }
    }

    func closeProject(_ project: RecentProject? = nil) {
        guard operation == nil, let path = project.map({ URL(fileURLWithPath: $0.path).standardizedFileURL.resolvingSymlinksInPath().path }) ?? currentPath?.path,
              let closing = sessions[path] else { return }
        clearSavedFrameDraft()
        if currentPath?.path == path && (frameDraft != nil || deferredFrameDraft != nil) {
            error = "Дождитесь сохранения фрейма или исправьте его поля перед закрытием проекта."
            return
        }
        if diagnosticsInFlight {
            cancelBackgroundDiagnostics()
            error = "Проверка проекта завершается. Дождитесь результата и повторите закрытие."
            return
        }
        if let projectId = sessionProjectIds[path] {
            let prefix = "\(projectId):"
            if chatDrafts.contains(where: { $0.key.hasPrefix(prefix) && !$0.value.isEmpty })
                || chatAttachmentDrafts.contains(where: { $0.key.hasPrefix(prefix) && !$0.value.isEmpty }) {
                error = "В чатах проекта есть несохранённые сообщения или вложения. Сохраните их либо очистите черновики перед закрытием."
                return
            }
        }
        if currentPath?.path == path && (snapshot?.dirty == true || possibleUnsaved) {
            let control = newOperationControl()
            perform("Сохранение перед закрытием…", control: control, work: { try saveSnapshot(closing, control: control) }) { result in
                self.snapshot = result.snapshot
                self.possibleUnsaved = result.snapshot.dirty
                if let error = result.error {
                    self.error = "Проект не закрыт: изменения не сохранены. \(error)"
                } else {
                    self.finishClosing(path)
                }
            }
        } else {
            finishClosing(path)
        }
    }

    func trash(_ project: RecentProject) {
        guard operation == nil else { return }
        let url = URL(fileURLWithPath: project.path).standardizedFileURL
        let canonicalPath = url.resolvingSymlinksInPath().path
        guard library.beginTrash(canonicalPath) else {
            error = "Сначала закройте этот проект во всех окнах приложения. Файлы не перемещены."
            return
        }
        guard let projectId = project.projectId, !projectId.isEmpty else {
            library.endTrash(canonicalPath)
            error = "Невозможно проверить идентификатор проекта. Сначала откройте его, чтобы обновить запись."
            return
        }
        perform("Перемещение в Корзину…", work: {
            let values = try url.resourceValues(forKeys: [.isSymbolicLinkKey])
            guard values.isSymbolicLink != true else {
                throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Путь проекта является символической ссылкой. Укажите фактическое расположение проекта."])
            }
            let before = try FileManager.default.attributesOfItem(atPath: url.path)
            guard before[.type] as? FileAttributeType == .typeDirectory,
                  let fileNumber = before[.systemFileNumber] as? NSNumber,
                  let systemNumber = before[.systemNumber] as? NSNumber else {
                throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Не удалось проверить папку проекта; файлы не перемещены."])
            }
            let manifest = try Data(contentsOf: url.appendingPathComponent("project.json"))
            let candidate = try JSONDecoder().decode(ProjectIdentity.self, from: manifest)
            guard candidate.projectId == projectId else {
                throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "По этому пути находится другой проект; файлы не перемещены."])
            }
            let after = try FileManager.default.attributesOfItem(atPath: url.path)
            guard after[.type] as? FileAttributeType == .typeDirectory,
                  after[.systemFileNumber] as? NSNumber == fileNumber,
                  after[.systemNumber] as? NSNumber == systemNumber else {
                throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Папка проекта изменилась во время проверки; файлы не перемещены."])
            }
            var result: NSURL?
            try FileManager.default.trashItem(at: url, resultingItemURL: &result)
            return result as URL?
        }, failure: { _ in
            self.library.endTrash(canonicalPath)
        }) { destination in
            self.library.endTrash(canonicalPath)
            do { try self.library.updateRecents { $0.removeAll { $0.id == project.id } } }
            catch {
                self.error = "Проект перемещён в Корзину\(destination.map { " (\($0.path))" } ?? ""), но запись не удалена из списка: \(readableError(error))"
            }
        }
    }

    private func accept(_ result: SaveResult, context: String? = nil) {
        snapshot = result.snapshot
        possibleUnsaved = result.snapshot.dirty
        if let error = result.error {
            self.error = [context, "Операция не завершена: \(error)\nСостояние редактора сохранено в памяти."].compactMap { $0 }.joined(separator: "\n")
        } else if let currentPath {
            remember(currentPath)
        }
    }

    private func invalidateFrameEditor() {
        frameEditorId = UUID()
    }

    private func acceptHistoryChange(_ result: SaveResult) {
        frameDraft = nil
        frameDraftNeedsReview = false
        invalidateFrameEditor()
        accept(result)
    }

    func updateFrameDraft(_ field: WritableKeyPath<FrameDraft, String>, value: String) {
        guard deferredFrameDraft == nil, let snapshot, !snapshot.readOnly, let root = snapshot.root else { return }
        var draft = frameDraft ?? FrameDraft(root)
        draft[keyPath: field] = value
        frameDraft = draft
        if writesBlocked { frameDraftNeedsReview = true }
    }

    func discardFrameDraft() {
        guard operation == nil else { return }
        frameDraft = nil
        frameDraftNeedsReview = false
    }

    func deferFrameDraft() {
        guard operation == nil, deferredFrameDraft == nil, let draft = frameDraft,
              let root = snapshot?.root else { return }
                deferredFrameDraft = DeferredFrameDraft(draft: draft, base: FrameDraft(root), rootId: root.id)
        frameDraft = nil
        frameDraftNeedsReview = false
    }

    func restoreDeferredFrameDraft() {
        guard operation == nil, frameDraft == nil, let deferred = deferredFrameDraft else { return }
        guard snapshot?.root?.id == deferred.rootId else {
            error = "Исходный фрейм больше недоступен. Скопируйте значения отложенного черновика в диагностике."
            return
        }
        frameDraft = deferred.draft
        frameDraftNeedsReview = true
        deferredFrameDraft = nil
    }

    func discardDeferredFrameDraft() {
        guard operation == nil else { return }
        deferredFrameDraft = nil
    }

    private func clearSavedFrameDraft() {
        guard let draft = frameDraft, let root = snapshot?.root,
              draft == FrameDraft(root), !hasUnsavedData else { return }
        frameDraft = nil
    }

    func commitFrameDraft(reviewed: Bool = false) {
        guard let draft = frameDraft, let (width, height) = draft.dimensions else { return }
        guard !writesBlocked, !frameDraftNeedsReview || reviewed else { return }
        guard operation == nil else { return }
        guard let session, let snapshot, let root = snapshot.root, !snapshot.readOnly else { return }
        frameDraftNeedsReview = false
        if draft.matches(root) {
            if snapshot.dirty || possibleUnsaved {
                let control = newOperationControl()
                perform("Сохранение изменений…", control: control, errorContext: "Фрейм «\(root.name)» · \(root.id)", work: {
                    try saveSnapshot(session, control: control)
                }, failure: { _ in
                }) { result in
                    self.accept(result, context: "Фрейм «\(root.name)» · \(root.id)")
                    if result.error == nil && !result.snapshot.dirty && self.frameDraft == draft {
                        self.frameDraft = nil
                    }
                }
            } else {
                frameDraft = nil
            }
            return
        }
        var operations: [[String: Any]] = []
        if draft.name != root.name {
            operations.append(["type": "setProperty", "nodeId": root.id, "path": "/name", "value": draft.name])
        }
        let widthChanged = width != root.layout.w
        let heightChanged = height != root.layout.h
        if widthChanged && heightChanged {
            operations.append(["type": "setViewport", "nodeId": root.id, "width": width, "height": height])
        } else if widthChanged {
            operations.append(["type": "setProperty", "nodeId": root.id, "path": "/layout/w", "value": width])
        } else if heightChanged {
            operations.append(["type": "setProperty", "nodeId": root.id, "path": "/layout/h", "value": height])
        }
        let transaction: [String: Any] = [
            "transactionId": UUID().uuidString.lowercased(),
            "baseRevision": snapshot.document.revision,
            "baseFingerprint": snapshot.fingerprint,
            "origin": "user",
            "scopeNodeIds": [root.id],
            "operations": operations
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: transaction, options: [.sortedKeys])
            guard let json = String(data: data, encoding: .utf8) else { throw CocoaError(.fileWriteInapplicableStringEncoding) }
            let previousUnsaved = possibleUnsaved
            possibleUnsaved = true
            let control = newOperationControl()
            perform("Сохранение изменений…", control: control, errorContext: "Фрейм «\(root.name)» · \(root.id)", work: {
                try editAndSaveSnapshot(session) {
                    try session.applyTransactionAndSaveControlled(transactionJson: json, control: control)
                }
            }, failure: { error in
                if wasCanceled(error) { self.possibleUnsaved = previousUnsaved }
            }) { result in
                self.accept(result, context: "Фрейм «\(root.name)» · \(root.id)")
                if result.error == nil && !result.snapshot.dirty && self.frameDraft == draft {
                    self.frameDraft = nil
                }
            }
        } catch { self.error = readableError(error) }
    }

    func save() {
        guard operation == nil, !writesBlocked, deferredFrameDraft == nil, let session else { return }
        if frameDraftNeedsReview {
            error = "Проверьте отложенный черновик и нажмите «Применить»."
            return
        }
        if let draft = frameDraft {
            guard draft.dimensions != nil else {
                error = "Исправьте название и размеры фрейма перед сохранением."
                return
            }
            commitFrameDraft()
            return
        }
        let control = newOperationControl()
        perform("Сохранение проекта…", control: control, work: { try saveSnapshot(session, control: control) }) { self.accept($0) }
    }

    func undo() {
        guard operation == nil else { return }
        clearSavedFrameDraft()
        guard !writesBlocked, frameDraft == nil, deferredFrameDraft == nil, let session else { return }
        let previousUnsaved = possibleUnsaved
        possibleUnsaved = true
        let control = newOperationControl()
        perform("Отмена изменения…", control: control, work: {
            try editAndSaveSnapshot(session) { try session.undoAndSaveControlled(control: control) }
        }, failure: { error in
            if wasCanceled(error) { self.possibleUnsaved = previousUnsaved }
        }) { self.acceptHistoryChange($0) }
    }

    func redo() {
        guard operation == nil else { return }
        clearSavedFrameDraft()
        guard !writesBlocked, frameDraft == nil, deferredFrameDraft == nil, let session else { return }
        let previousUnsaved = possibleUnsaved
        possibleUnsaved = true
        let control = newOperationControl()
        perform("Повтор изменения…", control: control, work: {
            try editAndSaveSnapshot(session) { try session.redoAndSaveControlled(control: control) }
        }, failure: { error in
            if wasCanceled(error) { self.possibleUnsaved = previousUnsaved }
        }) { self.acceptHistoryChange($0) }
    }

    func createCheckpoint() {
        guard operation == nil else { return }
        clearSavedFrameDraft()
        guard !writesBlocked, frameDraft == nil, deferredFrameDraft == nil, let session else { return }
        let previousUnsaved = possibleUnsaved
        possibleUnsaved = true
        let control = newOperationControl()
        perform("Создание контрольной точки…", control: control, work: {
            try editAndSaveSnapshot(session) {
                _ = try session.createCheckpointControlled(control: control)
                return try session.snapshotJson()
            }
        }, failure: { error in
            if wasCanceled(error) { self.possibleUnsaved = previousUnsaved }
        }) { result in
            self.accept(result)
            if result.error == nil { self.refreshCheckpoints() }
        }
    }

    func restoreCheckpoint(_ checkpoint: Checkpoint, allowMissingModes: Bool = false) {
        guard operation == nil else { return }
        clearSavedFrameDraft()
        guard !writesBlocked, frameDraft == nil, deferredFrameDraft == nil, let session else { return }
        let previousUnsaved = possibleUnsaved
        possibleUnsaved = true
        let control = newOperationControl()
        perform("Восстановление контрольной точки…", control: control, work: {
            try editAndSaveSnapshot(session) {
                if allowMissingModes {
                    return try session.restoreCheckpointWithMissingModesControlled(id: checkpoint.id, control: control)
                }
                return try session.restoreCheckpointControlled(id: checkpoint.id, control: control)
            }
        }, failure: { error in
            if wasCanceled(error) { self.possibleUnsaved = previousUnsaved }
        }) { result in
            if !allowMissingModes && result.errorCode == "CHECKPOINT_MODE_CONFIRMATION_REQUIRED" {
                self.snapshot = result.snapshot
                self.possibleUnsaved = result.snapshot.dirty
                self.checkpointAwaitingModeConfirmation = checkpoint
            } else {
                self.acceptHistoryChange(result)
            }
        }
    }

    func refreshCheckpoints() {
        guard operation == nil, let session, snapshot?.readOnly == false else { return }
        let control = newOperationControl()
        perform("Обновление контрольных точек…", control: control, work: {
            try decodeJSON([Checkpoint].self, session.checkpointsJsonControlled(control: control))
        }) { self.checkpoints = $0 }
    }

    func duplicate() {
        guard operation == nil else { return }
        clearSavedFrameDraft()
        guard !writesBlocked, frameDraft == nil, deferredFrameDraft == nil, let session, let snapshot else { return }
        let panel = NSSavePanel()
        panel.title = "Дублировать проект"
        panel.directoryURL = currentPath?.deletingLastPathComponent()
        panel.nameFieldStringValue = "\(snapshot.name) — копия"
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let selected = panel.url else { return }
        let url = selected.standardizedFileURL.resolvingSymlinksInPath()
        guard library.retain(url.path) else {
            error = "Выбранное расположение сейчас перемещается в Корзину."
            return
        }
        let control = newOperationControl()
        perform("Дублирование проекта…", control: control, work: {
            let next = try session.duplicateToControlled(destination: url.path, name: url.lastPathComponent, control: control)
            return try loadProject(next, at: url)
        }, failure: { _ in self.library.release(url.path) }) {
            self.install($0, at: url, reserved: true)
        }
    }

    func createPortableSnapshot() {
        guard operation == nil else { return }
        clearSavedFrameDraft()
        guard !writesBlocked, !hasUnsavedData, frameDraft == nil, deferredFrameDraft == nil,
              let session, let snapshot else { return }
        let panel = NSSavePanel()
        panel.title = "Переносимый снимок проекта"
        panel.message = "Выберите новое расположение снимка."
        panel.directoryURL = currentPath?.deletingLastPathComponent()
        panel.nameFieldStringValue = "\(snapshot.name) — снимок"
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let selected = panel.url else { return }
        let url = selected.standardizedFileURL.resolvingSymlinksInPath()
        let control = newOperationControl()
        perform("Создание переносимого снимка…", control: control, work: {
            try session.createPortableSnapshotControlled(destination: url.path, control: control)
        }) {
            self.notice = "Снимок сохранён в \(url.path)."
        }
    }

    func rename(_ name: String, completion: @escaping () -> Void) {
        guard operation == nil else { return }
        clearSavedFrameDraft()
        guard !writesBlocked, frameDraft == nil, deferredFrameDraft == nil, let session else { return }
        let previousUnsaved = possibleUnsaved
        possibleUnsaved = true
        let control = newOperationControl()
        perform("Переименование проекта…", control: control, work: {
            try editAndSaveSnapshot(session) { try session.renameAndSaveControlled(name: name, control: control) }
        }, failure: { error in
            if wasCanceled(error) { self.possibleUnsaved = previousUnsaved }
        }) { result in
            self.accept(result)
            if result.error == nil { completion() }
        }
    }

    func refreshChats(select chatId: String? = nil) {
        guard operation == nil, let session else { return }
        let control = newOperationControl()
        perform("Загрузка чатов…", control: control, work: {
            try decodeJSON([Chat].self, session.chatsJsonControlled(control: control))
        }) { chats in
            self.chats = chats
            let selected = chatId.flatMap { id in chats.first(where: { $0.id == id })?.id }
                ?? chats.first(where: { !$0.archived })?.id
            self.selectedChatId = selected
            self.messages = []
            guard let selected else { return }
            let messageControl = newOperationControl()
            self.perform("Загрузка сообщений…", control: messageControl, work: {
                try decodeJSON([ChatMessage].self, session.chatMessagesJsonControlled(id: selected, control: messageControl))
            }) { self.messages = $0 }
        }
    }

    private func replaceChat(_ chat: Chat) {
        if let index = chats.firstIndex(where: { $0.id == chat.id }) {
            chats[index] = chat
        }
    }

    func createChat(title: String, mode: String, completion: @escaping () -> Void) {
        guard !writesBlocked, let session else { return }
        let control = newOperationControl()
        perform("Создание чата…", control: control, work: {
            try decodeJSON(Chat.self, session.createChatControlled(title: title, mode: mode, control: control))
        }) { chat in
            self.chats.insert(chat, at: 0)
            self.selectedChatId = chat.id
            self.messages = []
            self.chatSearchResults = nil
            completion()
        }
    }

    func renameChat(_ chat: Chat, title: String, completion: @escaping () -> Void) {
        guard !writesBlocked, let session else { return }
        let control = newOperationControl()
        perform("Переименование чата…", control: control, work: {
            try decodeJSON(Chat.self, session.renameChatControlled(chatId: chat.id, title: title, control: control))
        }) { chat in
            self.replaceChat(chat)
            completion()
        }
    }

    func archiveChat(_ chat: Chat, archived: Bool) {
        guard !writesBlocked, let session else { return }
        let control = newOperationControl()
        perform("Обновление чата…", control: control, work: {
            try decodeJSON(Chat.self, session.archiveChatControlled(chatId: chat.id, archived: archived, control: control))
        }) { self.replaceChat($0) }
    }

    func setChatMode(_ chat: Chat, mode: String) {
        guard !writesBlocked, let session else { return }
        let control = newOperationControl()
        perform("Обновление режима чата…", control: control, work: {
            try decodeJSON(Chat.self, session.setChatModeControlled(chatId: chat.id, mode: mode, control: control))
        }) { self.replaceChat($0) }
    }

    func searchChats(_ query: String) {
        guard let session else { return }
        let control = newOperationControl()
        perform("Поиск в чатах…", control: control, work: {
            try decodeJSON([ChatSearchHit].self, session.searchChatsJsonControlled(query: query, control: control))
        }) { self.chatSearchResults = $0 }
    }

    func forkChat(_ chat: Chat, through message: ChatMessage, title: String, completion: @escaping () -> Void) {
        guard !writesBlocked, let session else { return }
        let control = newOperationControl()
        perform("Ответвление чата…", control: control, work: {
            try decodeJSON(Chat.self, session.forkChatControlled(chatId: chat.id, messageId: message.id, title: title, control: control))
        }) { chat in
            self.chats.insert(chat, at: 0)
            self.selectedChatId = chat.id
            self.messages = []
            self.chatSearchResults = nil
            completion()
            self.refreshChats(select: chat.id)
        }
    }

    func deleteChat(_ chat: Chat) {
        guard !writesBlocked, let session else { return }
        let draftKey = "\(snapshot?.document.projectId ?? ""):\(chat.id)"
        let control = newOperationControl()
        perform("Удаление чата…", control: control, work: { try session.deleteChatControlled(chatId: chat.id, control: control) }) {
            self.chats.removeAll { $0.id == chat.id }
            self.chatDrafts.removeValue(forKey: draftKey)
            self.chatAttachmentDrafts.removeValue(forKey: draftKey)
            self.chatSearchResults = nil
            if self.selectedChatId == chat.id {
                self.selectedChatId = nil
                self.messages = []
                self.refreshChats()
            }
        }
    }

    func appendMessage(_ text: String, attachmentIds: [String], completion: @escaping () -> Void) {
        guard !writesBlocked, let session, let chatId = selectedChatId else { return }
        do {
            let input: [String: Any] = [
                "role": "user",
                "content": text.isEmpty ? [] : [["type": "text", "text": text]],
                "attachments": attachmentIds.map { ["assetId": $0] }
            ]
            let json = String(decoding: try JSONSerialization.data(withJSONObject: input), as: UTF8.self)
            let control = newOperationControl()
            perform("Сохранение сообщения…", control: control, work: {
                try decodeJSON(ChatMessage.self, session.appendChatMessageControlled(id: chatId, json: json, control: control))
            }) { message in
                self.messages.append(message)
                completion()
            }
        } catch { self.error = readableError(error) }
    }

    func importAsset() {
        guard operation == nil, !writesBlocked, let session else { return }
        let panel = NSOpenPanel()
        panel.title = "Добавить ресурс"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let control = newOperationControl()
        perform("Импорт ресурса…", control: control, work: {
            _ = try session.importAssetControlled(path: url.path, control: control)
            return try decodeJSON(Snapshot.self, session.snapshotJson())
        }) {
            self.snapshot = $0
            self.watchChanged()
        }
    }

    func relinkAsset(_ asset: Snapshot.Document.Asset) {
        guard operation == nil, let session else { return }
        let panel = NSOpenPanel()
        panel.title = "Перепривязать ресурс"
        panel.message = asset.path
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let control = newOperationControl()
        perform("Перепривязка ресурса…", control: control, work: {
            _ = try session.relinkAssetControlled(id: asset.id, path: url.path, control: control)
            return try decodeJSON(Snapshot.self, session.snapshotJson())
        }) {
            let wasReadOnly = self.snapshot?.readOnly == true
            self.snapshot = $0
            if wasReadOnly && !$0.readOnly { self.refreshCheckpoints() }
            self.watchChanged()
        }
    }

    func resolveConflict(_ review: ConflictReview, selections: [String: String], completion: @escaping () -> Void) {
        clearSavedFrameDraft()
        guard frameDraft == nil else {
            error = "Сначала завершите ввод фрейма или явно отбросьте его черновик в диагностике. Выбор конфликта не будет применён."
            return
        }
        guard operation == nil, let session, ["externalChange", "retainedWrite"].contains(review.conflict.kind ?? ""),
              let paths = review.conflict.paths, !paths.isEmpty,
              let allowed = review.conflict.choices else { return }
        var chosen: [[String: String]] = []
        for path in paths {
            guard let take = selections[path.path], ["base", "local", "disk"].contains(take),
                  path.choices.contains(take), allowed.contains(take) else {
                error = "Выберите доступную версию для каждого файла."
                return
            }
            chosen.append(["path": path.path, "take": take])
        }
        do {
            let request: [String: Any] = [
                "schemaVersion": 1,
                "conflictId": review.conflict.id,
                "expectedDiskToken": review.diskToken,
                "expectedLocalGeneration": review.localGeneration,
                "selections": chosen
            ]
            let json = String(decoding: try JSONSerialization.data(withJSONObject: request, options: [.sortedKeys]), as: UTF8.self)
            let control = newOperationControl()
            perform("Разрешение конфликта…", control: control, work: {
                try decodeJSON(Snapshot.self, session.resolveExternalConflictControlled(requestJson: json, control: control))
            }, failure: { _ in
                self.watchChanged()
            }) { snapshot in
                self.applyResolvedSnapshot(snapshot, completion: completion)
            }
        } catch { self.error = readableError(error) }
    }

    func resolveRecovery(_ review: ConflictReview, take: String, completion: @escaping () -> Void) {
        clearSavedFrameDraft()
        guard frameDraft == nil else {
            error = "Сначала завершите ввод фрейма или явно отбросьте его черновик в диагностике. Выбор восстановления не будет применён."
            return
        }
        guard operation == nil, let session, review.conflict.kind == "recovery",
              let snapshots = review.conflict.snapshots, review.conflict.choices?.contains(take) == true,
              (take == "old" && snapshots.old.available) || (take == "new" && snapshots.new.available) else {
            error = "Выберите доступный целый снимок для восстановления."
            return
        }
        do {
            let request: [String: Any] = [
                "schemaVersion": 1,
                "conflictId": review.conflict.id,
                "expectedDiskToken": review.diskToken,
                "expectedLocalGeneration": review.localGeneration,
                "take": take
            ]
            let json = String(decoding: try JSONSerialization.data(withJSONObject: request, options: [.sortedKeys]), as: UTF8.self)
            let control = newOperationControl()
            perform("Восстановление снимка…", control: control, work: {
                try decodeJSON(Snapshot.self, session.resolveRecoveryConflictControlled(requestJson: json, control: control))
            }, failure: { _ in
                self.watchChanged()
            }) { snapshot in
                self.applyResolvedSnapshot(snapshot, completion: completion)
            }
        } catch { self.error = readableError(error) }
    }

    private func applyResolvedSnapshot(_ snapshot: Snapshot, completion: () -> Void) {
        invalidateFrameEditor()
        self.snapshot = snapshot
        possibleUnsaved = snapshot.dirty
        diagnostics = nil
        diagnosticsError = nil
        if let deferred = deferredFrameDraft, let root = snapshot.root, root.id == deferred.rootId {
            var draft = FrameDraft(root)
            if deferred.draft.name != deferred.base.name { draft.name = deferred.draft.name }
            if deferred.draft.width != deferred.base.width { draft.width = deferred.draft.width }
            if deferred.draft.height != deferred.base.height { draft.height = deferred.draft.height }
            frameDraft = draft
            frameDraftNeedsReview = true
            deferredFrameDraft = nil
        }
        checkpoints = []
        checkpointAwaitingModeConfirmation = nil
        chats = []
        messages = []
        selectedChatId = nil
        completion()
        if !snapshot.readOnly { refreshCheckpoints() }
        watchChanged()
    }

    func revealConflictVersion(_ relativePath: String) {
        guard let root = currentPath, relativePath.hasPrefix("conflicts/"),
              !relativePath.split(separator: "/").contains("..") else {
            error = "Недопустимый путь копии конфликта."
            return
        }
        let url = root.appendingPathComponent(relativePath).standardizedFileURL.resolvingSymlinksInPath()
        let prefix = root.standardizedFileURL.resolvingSymlinksInPath().path + "/conflicts/"
        guard url.path.hasPrefix(prefix), FileManager.default.fileExists(atPath: url.path) else {
            error = "Копия конфликта недоступна или находится вне папки проекта."
            return
        }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    func pollDiagnostics() async {
        guard let session, operation == nil, !diagnosticsInFlight else { return }
        let control = newOperationControl()
        diagnosticControl = control
        diagnosticsInFlight = true
        defer {
            diagnosticControl = nil
            diagnosticsInFlight = false
            if observationPending { watchChanged() }
        }
        do {
            let result = try await withTaskCancellationHandler(operation: {
                try await Task.detached(priority: .utility) {
                    try decodeJSON(ProjectDiagnostics.self, session.diagnosticsJsonControlled(control: control))
                }.value
            }, onCancel: {
                _ = control.requestCancel()
            })
            guard self.session === session, operation == nil, !Task.isCancelled else { return }
            diagnostics = result
            if result.readOnly {
                clearSavedFrameDraft()
                if frameDraft != nil {
                                        frameDraftNeedsReview = true
                }
                pendingGesture = nil
                canvasPreview = nil
            }
            diagnosticsError = nil
        } catch {
            guard self.session === session, operation == nil, !Task.isCancelled else { return }
            diagnosticsError = wasCanceled(error) ? "Проверка отменена. Нажмите «Обновить», чтобы повторить её." : readableError(error)
        }
    }

    // MARK: Canvas (C.3, C.4, C.9)

    /// Renders the current page to HTML and computed bounds. Revisions are
    /// requested in order and every request re-reads the session, so a stale
    /// result can never replace a newer one.
    func refreshCanvas() {
        guard let session, let path = currentPath, session === self.session else { return }
        guard let page = snapshot?.document.pages.first else {
            canvas = .empty
            canvasTaskState = nil
            canvasGenerating = false
            return
        }
        canvasTaskState?.task?.cancel()
        canvasTaskState = nil
        guard operation == nil else {
            canvasQueued = true
            return
        }
        canvasQueued = false
        let request = CanvasTaskRequest(session: session, page: CanvasPage(id: page.id, rootId: page.rootId), scale: canvasScale)
        let item = CanvasTaskState(request: request)
        canvasTaskState = item
        canvasGenerating = true
        item.task = Task { [weak self] in
            let result = await Task.detached(priority: .userInitiated) {
                Result {
                    let html = try request.session.previewHtml(pageId: request.page.id)
                    let boundsJSON = try request.session.layoutJson(pageId: request.page.id)
                    guard let bounds = try? decodeJSON([CanvasNodeBounds].self, boundsJSON) else {
                        throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Не удалось прочитать вычисленные границы узлов."])
                    }
                    return try CanvasContent.prepared(html: html, nodes: bounds, bridge: CanvasBridge.script, scale: request.scale)
                }
            }.value
            guard !Task.isCancelled else { return }
            self?.finishCanvas(result, request: request, path: path)
        }
    }

    private func finishCanvas(_ result: Result<CanvasContent, Error>, request: CanvasTaskRequest, path: URL) {
        guard let state = canvasTaskState, state.request.session === request.session, currentPath?.path == path.path else { return }
        canvasTaskState = nil
        canvasGenerating = false
        switch result {
        case .success(let content):
            canvas = content
            canvasNotice = nil
            selectedNodeIds = selectedNodeIds.filter { id in content.nodes.contains { $0.id == id } }
        case .failure(let error):
            // A missing font or asset is a document problem that the user
            // fixes; keep the last rendered page and say why it is stale.
            if canvas.html.isEmpty { canvas = .empty }
            canvasNotice = "Холст не обновлён: \(canvasReadableError(error))"
        }
        if canvasQueued { refreshCanvas() }
    }

    /// The page is pre-scaled into its HTML, so a new scale means new HTML.
    func setCanvasScale(_ scale: Double) {
        let rounded = (scale * 1000).rounded() / 1000
        guard rounded > 0, rounded != canvasScale else { return }
        canvasScale = rounded
        refreshCanvas()
    }

    func selectNode(_ id: String?, extending: Bool) {
        guard let id else {
            if extending { selectedNodeIds.formIntersection([]) }
            selectedNodeIds = []
            canvasPreview = nil
            return
        }
        if extending {
            if selectedNodeIds.contains(id) { selectedNodeIds.remove(id) } else { selectedNodeIds.insert(id) }
        } else if !selectedNodeIds.contains(id) || selectedNodeIds.count > 1 {
            selectedNodeIds = [id]
        }
        canvasPreview = nil
    }

    func clearSelection() {
        selectedNodeIds = []
        canvasPreview = nil
    }

    private func canvasNode(_ id: String) -> CanvasNodeBounds? {
        canvas.nodes.first { $0.id == id }
    }

    /// Starts a drag gesture from the topmost visible node under the point.
    /// The gesture is committed once on release, so it is one undo step.
    func beginCanvasGesture(_ kind: CanvasGestureKind, at point: CGPoint, hit: CanvasHit, extending: Bool) {
        guard !writesBlocked, operation == nil, !canvasGenerating, let snapshot, let root = snapshot.root else { return }
        let ids: [String]
        switch kind {
        case .move:
            selectNode(hit.id, extending: extending)
            guard selectedNodeIds.contains(hit.id) else { return }
            ids = orderedSelection()
            if ids.count > 1, canvas.nodes.first(where: { $0.id == ids[0] })?.parentId != canvas.nodes.first(where: { $0.id == hit.id })?.parentId {
                canvasNotice = "Перемещать вместе можно только элементы одного родителя."
                return
            }
        case .resize, .rotate:
            if !selectedNodeIds.contains(hit.id) { selectNode(hit.id, extending: false) }
            guard selectedNodeIds == [hit.id] else {
                canvasNotice = "Изменение размера и поворот применяются к одному выделенному элементу."
                return
            }
            ids = [hit.id]
        }
        let nodes = ids.compactMap { canvasNode($0) }
        guard nodes.count == ids.count, let primary = nodes.first(where: { $0.id == hit.id }) else { return }
        for id in ids where id == root.id {
            if kind != .move {
                canvasNotice = "Корневой фрейм нельзя изменять этим жестом."
                return
            }
        }
        let flowParent = primary.parentId.flatMap { parent in canvas.nodes.first { $0.id == parent } }
        if kind == .move, nodes.count > 1, flowParent != nil, primary.inFlow {
            canvasNotice = "Внутри потока перемещается один элемент: он меняет порядок среди соседей."
            return
        }
        pendingGesture = CanvasGesture(kind: kind, ids: ids, start: point, bounds: nodes, baseRevision: snapshot.document.revision,
                                       baseFingerprint: snapshot.fingerprint, inFlow: primary.inFlow, rotation: primary.rotation)
        canvasPreview = CanvasRect(x: primary.x, y: primary.y, w: primary.w, h: primary.h)
        canvasNotice = nil
    }

    func updateCanvasGesture(_ kind: CanvasGestureKind, at point: CGPoint) {
        guard let gesture = pendingGesture, gesture.kind == kind else { return }
        let dx = point.x - gesture.start.x
        let dy = point.y - gesture.start.y
        switch kind {
        case .move:
            let base = gesture.bounds.first { gesture.ids.first == $0.id } ?? gesture.bounds[0]
            canvasPreview = CanvasRect(x: base.x + dx, y: base.y + dy, w: base.w, h: base.h)
        case .resize:
            let base = gesture.bounds[0]
            canvasPreview = CanvasRect(x: base.x, y: base.y, w: max(1, base.w + dx), h: max(1, base.h + dy))
        case .rotate:
            let base = gesture.bounds[0]
            let startAngle = atan2(gesture.start.y - (base.y + base.h / 2), gesture.start.x - (base.x + base.w / 2))
            let angle = atan2(point.y - (base.y + base.h / 2), point.x - (base.x + base.w / 2))
            let degrees = (angle - startAngle) * 180 / .pi
            canvasRotation = (base.rotation + degrees).truncatingRemainder(dividingBy: 360)
        }
    }

    func endCanvasGesture(_ kind: CanvasGestureKind, at point: CGPoint) {
        guard let gesture = pendingGesture, gesture.kind == kind else { return }
        updateCanvasGesture(kind, at: point)
        let preview = canvasPreview
        let rotation = canvasRotation
        pendingGesture = nil
        canvasPreview = nil
        canvasRotation = nil
        guard let preview, let operations = gestureOperations(gesture, preview: preview, rotation: rotation) else { return }
        if operations.isEmpty { return }
        applyCanvasOperations(operations, scope: gesture.ids, baseRevision: gesture.baseRevision, baseFingerprint: gesture.baseFingerprint)
    }

    func cancelCanvasGesture() {
        pendingGesture = nil
        canvasPreview = nil
        canvasRotation = nil
    }

    var canvasEditing: Bool {
        pendingGesture != nil
    }

    private func orderedSelection() -> [String] {
        canvas.nodes.filter { selectedNodeIds.contains($0.id) }.map(\.id)
    }

    private func gestureOperations(_ gesture: CanvasGesture, preview: CanvasRect, rotation: Double?) -> [[String: Any]]? {
        switch gesture.kind {
        case .move:
            var operations: [[String: Any]] = []
            for bounds in gesture.bounds {
                let x = round(bounds.x + preview.x - (gesture.bounds.first(where: { $0.id == gesture.ids.first })?.x ?? preview.x))
                let y = round(bounds.y + preview.y - (gesture.bounds.first(where: { $0.id == gesture.ids.first })?.y ?? preview.y))
                if x != bounds.x {
                    operations.append(["type": "setProperty", "nodeId": bounds.id, "path": "/layout/x", "value": x])
                }
                if y != bounds.y {
                    operations.append(["type": "setProperty", "nodeId": bounds.id, "path": "/layout/y", "value": y])
                }
            }
            return operations
        case .resize:
            guard let bounds = gesture.bounds.first else { return nil }
            let w = round(preview.w)
            let h = round(preview.h)
            var operations: [[String: Any]] = []
            if w != bounds.w { operations.append(["type": "setProperty", "nodeId": bounds.id, "path": "/layout/w", "value": w]) }
            if h != bounds.h { operations.append(["type": "setProperty", "nodeId": bounds.id, "path": "/layout/h", "value": h]) }
            return operations
        case .rotate:
            guard let bounds = gesture.bounds.first, let rotation else { return nil }
            let value = ((rotation.truncatingRemainder(dividingBy: 360)) + 360).truncatingRemainder(dividingBy: 360)
            return [["type": "setProperty", "nodeId": bounds.id, "path": "/style/rotate", "value": value]]
        }
    }

    private func applyCanvasOperations(_ operations: [[String: Any]], scope: [String], baseRevision: UInt64, baseFingerprint: String) {
        guard let session else { return }
        let transaction: [String: Any] = [
            "transactionId": UUID().uuidString.lowercased(),
            "baseRevision": baseRevision,
            "baseFingerprint": baseFingerprint,
            "origin": "user",
            "scopeNodeIds": scope,
            "operations": operations
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: transaction, options: [.sortedKeys])
            guard let json = String(data: data, encoding: .utf8) else { throw CocoaError(.fileWriteInapplicableStringEncoding) }
            let previousUnsaved = possibleUnsaved
            possibleUnsaved = true
            let control = newOperationControl()
            perform("Сохранение изменений…", control: control, work: {
                try editAndSaveSnapshot(session) {
                    try session.applyTransactionAndSaveControlled(transactionJson: json, control: control)
                }
            }, failure: { error in
                if wasCanceled(error) { self.possibleUnsaved = previousUnsaved }
            }) { result in
                if let error = result.error {
                    // The core already refuses conflicting transactions, so the
                    // snapshot is authoritative and the canvas must follow it.
                    self.accept(result)
                    self.error = [error, "Действие не сохранено; холст показывает состояние проекта."].joined(separator: "\n")
                } else {
                    self.accept(result)
                }
                self.refreshCanvas()
            }
        } catch {
            self.error = readableError(error)
        }
    }


    private func canvasReadableError(_ error: Error) -> String {
        if let coreError = error as? CoreError, case let .Failure(code, message) = coreError {
            switch code {
            case "MISSING_FONT": return "не найден шрифт проекта. \(message)"
            case "MISSING_ASSET": return "не найден ресурс страницы. \(message)"
            default: break
            }
        }
        return readableError(error)
    }
}
