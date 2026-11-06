import AppKit
import CoreServices
import SwiftUI

// Use SwiftUI’s property wrapper when the CLT does not include SwiftUIMacros.
private typealias StoredState<Value> = SwiftUI.State<Value>

@main
struct AIConstructorApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        WindowGroup("AI Constructor") {
            WorkspaceView()
                .frame(minWidth: 900, minHeight: 620)
        }
        .windowStyle(.hiddenTitleBar)
        Settings {
            RetentionSettingsView()
        }
    }
}

@MainActor
private final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let workspace = ProjectLibrary.shared.blockingWorkspace, let reason = workspace.closeBlockReason else {
            return .terminateNow
        }
        workspace.cancelBackgroundDiagnostics()
        let alert = NSAlert()
        alert.messageText = "Приложение не закрыто"
        alert.informativeText = reason
        alert.addButton(withTitle: "OK")
        alert.runModal()
        return .terminateCancel
    }
}

@MainActor
private final class WindowCloseDelegate: NSObject, NSWindowDelegate {
    weak var model: WorkspaceModel?
    weak var previous: NSWindowDelegate?

    init(model: WorkspaceModel, previous: NSWindowDelegate?) {
        self.model = model
        self.previous = previous
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if let model, let reason = model.closeBlockReason {
            model.cancelBackgroundDiagnostics()
            model.error = reason
            return false
        }
        return previous?.windowShouldClose?(sender) ?? true
    }

    override func responds(to selector: Selector!) -> Bool {
        super.responds(to: selector) || (previous?.responds(to: selector) ?? false)
    }

    override func forwardingTarget(for selector: Selector!) -> Any? {
        if previous?.responds(to: selector) == true { return previous }
        return super.forwardingTarget(for: selector)
    }
}

@MainActor
private final class WindowCloseGuardView: NSView {
    weak var model: WorkspaceModel?
    private weak var attachedWindow: NSWindow?
    private var closeDelegate: WindowCloseDelegate?

    override func viewDidMoveToWindow() {
        if let attachedWindow, attachedWindow.delegate === closeDelegate {
            attachedWindow.delegate = closeDelegate?.previous
        }
        super.viewDidMoveToWindow()
        guard let window, let model else { return }
        let delegate = WindowCloseDelegate(model: model, previous: window.delegate)
        closeDelegate = delegate
        attachedWindow = window
        window.delegate = delegate
    }
}

private struct WindowCloseGuard: NSViewRepresentable {
    let model: WorkspaceModel
    func makeNSView(context: Context) -> WindowCloseGuardView {
        let view = WindowCloseGuardView()
        view.model = model
        return view
    }
    func updateNSView(_ view: WindowCloseGuardView, context: Context) { view.model = model }
}

private final class ProjectFileWatcher {
    private let root: String
    private let tracked: [(exact: String, descendants: String)]
    private let changed: () -> Void
    private var stream: FSEventStreamRef?

    init(root: String, changed: @escaping () -> Void) {
        // FSEvents emits real paths; Foundation preserves aliases such as /tmp.
        let root = root.withCString { path -> String in
            guard let resolved = realpath(path, nil) else { return root }
            defer { free(resolved) }
            return String(cString: resolved)
        }
        self.root = root
        tracked = ["project.json", ".gitignore", "document.json", "src", "assets", "chats", "agent", "history", "checkpoints", "conflicts", ".aic-local/retained"].map {
            let path = root + "/" + $0
            return (path, path + "/")
        }
        self.changed = changed
    }

    func start() -> Bool {
        var context = FSEventStreamContext(version: 0, info: Unmanaged.passUnretained(self).toOpaque(), retain: nil, release: nil, copyDescription: nil)
        let flags = UInt32(kFSEventStreamCreateFlagUseCFTypes | kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagWatchRoot)
        guard let stream = FSEventStreamCreate(kCFAllocatorDefault, { _, info, count, paths, flags, _ in
            guard let info, let paths = unsafeBitCast(paths, to: NSArray.self) as? [String] else { return }
            let watcher = Unmanaged<ProjectFileWatcher>.fromOpaque(info).takeUnretainedValue()
            let lost = UInt32(kFSEventStreamEventFlagMustScanSubDirs | kFSEventStreamEventFlagUserDropped | kFSEventStreamEventFlagKernelDropped | kFSEventStreamEventFlagRootChanged)
            for index in 0..<min(count, paths.count) {
                if flags[index] & lost != 0 || watcher.includes(paths[index]) {
                    watcher.changed()
                    break
                }
            }
        }, &context, [root] as CFArray, FSEventStreamEventId(kFSEventStreamEventIdSinceNow), 1.0, flags) else {
            return false
        }
        FSEventStreamSetDispatchQueue(stream, .main)
        guard FSEventStreamStart(stream) else {
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
            return false
        }
        self.stream = stream
        return true
    }

    private func includes(_ path: String) -> Bool {
        if path == root { return true }
        return tracked.contains { path == $0.exact || path.hasPrefix($0.descendants) }
    }

    func stop() {
        guard let stream else { return }
        FSEventStreamStop(stream)
        FSEventStreamInvalidate(stream)
        FSEventStreamRelease(stream)
        self.stream = nil
    }

    deinit { stop() }
}

private struct Snapshot: Decodable {
    struct Document: Decodable {
        struct Page: Decodable { let rootId: String }
        struct Node: Decodable {
            struct Layout: Decodable { let x: Double; let y: Double; let w: Double; let h: Double }
            let id: String
            let name: String
            let layout: Layout
        }
        struct Asset: Decodable, Identifiable {
            let id: String
            let path: String
        }
        var projectId: String
        let revision: UInt64
        let pages: [Page]
        let nodes: [Node]
        let assets: [Asset]

        private enum CodingKeys: String, CodingKey { case projectId, revision, pages, nodes, assets }
        init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            projectId = (try? values.decode(String.self, forKey: .projectId)) ?? ""
            revision = (try? values.decode(UInt64.self, forKey: .revision)) ?? 0
            pages = (try? values.decode([Page].self, forKey: .pages)) ?? []
            nodes = (try? values.decode([Node].self, forKey: .nodes)) ?? []
            assets = (try? values.decode([Asset].self, forKey: .assets)) ?? []
        }
    }
    var document: Document
    let fingerprint: String
    let name: String
    let readOnly: Bool
    let readOnlyReason: String?
    let dirty: Bool

    var root: Document.Node? {
        guard let rootId = document.pages.first?.rootId else { return nil }
        return document.nodes.first { $0.id == rootId }
    }
}

private struct RecentProject: Codable, Identifiable {
    var id: String { path }
    var path: String
    var name: String
    var projectId: String?
    var archived: Bool?
}

private struct Checkpoint: Decodable, Identifiable {
    let id: String
    let revision: UInt64
}

private struct Chat: Decodable, Identifiable {
    let id: String
    let title: String
    let mode: String
    let archived: Bool
}

private struct ChatMessage: Decodable, Identifiable {
    struct Content: Decodable {
        let type: String
        let text: String?
    }
    struct Attachment: Decodable {
        let assetId: String
    }
    let id: String
    let role: String
    let content: [Content]
    let attachments: [Attachment]
}

private struct ChatSearchHit: Decodable, Identifiable {
    let chat: Chat
    let message: ChatMessage
    var id: String { "\(chat.id):\(message.id)" }
}

private struct ProjectDiagnostics: Decodable {
    struct FileChange: Decodable, Identifiable {
        let path: String
        let kind: String
        let expectedHash: String?
        let actualHash: String?
        var id: String { path }
    }
    struct Version: Decodable {
        let state: String
        let hash: String?
        let size: UInt64?
        let archiveRelativePath: String?
        let expectedHash: String?
    }
    struct ConflictPath: Decodable, Identifiable {
        let path: String
        let base: Version
        let local: Version
        let disk: Version
        let choices: [String]
        var id: String { path }
    }
    struct RecoveryFile: Decodable, Identifiable {
        let path: String
        let hash: String?
        let archiveRelativePath: String?
        var id: String { path }
    }
    struct RecoverySnapshot: Decodable {
        let available: Bool
        let manifestHash: String?
        let revision: UInt64?
        let storageGeneration: UInt64?
        let manifestPath: String?
        let files: [RecoveryFile]?
    }
    struct RecoverySnapshots: Decodable {
        let old: RecoverySnapshot
        let new: RecoverySnapshot
    }
    struct Conflict: Decodable, Identifiable {
        let id: String
        let reason: String
        let kind: String?
        let paths: [ConflictPath]?
        let blockedPaths: [String]?
        let choices: [String]?
        let snapshots: RecoverySnapshots?
    }
    let readOnly: Bool
    let readOnlyReason: String?
    let manifestChanged: Bool
    let files: [FileChange]
    let conflicts: [Conflict]
    let baselineAvailable: Bool
    let schemaVersion: UInt32?
    let localGeneration: UInt64?
    let observedDiskToken: String?
    let baseManifestHash: String?
    let blockedGenerated: [String]?
}

private enum WorkspacePanel: String, CaseIterable, Codable {
    case inspector, chats, assets, diagnostics

    var title: String {
        switch self {
        case .inspector: return "Инспектор"
        case .chats: return "Чаты"
        case .assets: return "Ресурсы"
        case .diagnostics: return "Диагностика"
        }
    }
}

private struct RetentionSettings: Codable {
    let schemaVersion: Int
    let limit: UInt32?
    let riskAccepted: Bool
}

@MainActor
private final class WeakWorkspace {
    weak var value: WorkspaceModel?
    init(_ value: WorkspaceModel) { self.value = value }
}

@MainActor
private final class ProjectLibrary: ObservableObject {
    static let shared = ProjectLibrary()
    @Published private(set) var recents: [RecentProject] = []
    @Published private(set) var panels: [String: WorkspacePanel] = [:]
    @Published private(set) var retentionLimit: UInt32?
    @Published var error: String?
    private var recentsReadable = true
    private var panelsReadable = true
    private var retentionReadable = true
    private var openPaths: [String: Int] = [:]
    private var trashing: Set<String> = []
    private var workspaces: [WeakWorkspace] = []

    private let directory: URL

    private init() {
        let base = ProcessInfo.processInfo.environment["AIC_APPLICATION_SUPPORT"].map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        directory = base.appendingPathComponent("AIConstructor", isDirectory: true)
        do { recents = try read("recent-projects.json", default: []) }
        catch { recentsReadable = false; self.error = "Не удалось прочитать список проектов; файл не будет перезаписан: \(error.localizedDescription)" }
        do { panels = try read("panel-settings.json", default: [:]) }
        catch { panelsReadable = false; self.error = "Не удалось прочитать настройки панелей; файл не будет перезаписан: \(error.localizedDescription)" }
        do { retentionLimit = try readRetention() }
        catch { retentionReadable = false; self.error = "Не удалось прочитать настройки хранения; действует неограниченное хранение, файл не будет перезаписан: \(error.localizedDescription)" }
    }

    private func read<T: Decodable>(_ name: String, default defaultValue: T) throws -> T {
        do { return try JSONDecoder().decode(T.self, from: Data(contentsOf: directory.appendingPathComponent(name))) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { return defaultValue }
    }

    private func readRetention() throws -> UInt32? {
        let path = directory.appendingPathComponent("retention-settings.json")
        let data: Data
        do { data = try Data(contentsOf: path) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { return nil }
        guard let fields = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(fields.keys).isSubset(of: ["schemaVersion", "limit", "riskAccepted"]) else {
            throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Неизвестный формат настроек хранения."])
        }
        let settings = try JSONDecoder().decode(RetentionSettings.self, from: data)
        guard settings.schemaVersion == 1, settings.limit == nil || settings.riskAccepted else {
            throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Неизвестная версия или отсутствует подтверждение риска."])
        }
        return settings.limit
    }

    private func write<T: Encodable>(_ value: T, to name: String) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(value).write(to: directory.appendingPathComponent(name), options: .atomic)
    }

    func setRetentionLimit(_ limit: UInt32?) throws {
        guard retentionReadable else {
            throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Настройки хранения недоступны для записи. Исправьте или восстановите retention-settings.json."])
        }
        guard retentionLimit != limit else { return }
        try write(RetentionSettings(schemaVersion: 1, limit: limit, riskAccepted: limit != nil), to: "retention-settings.json")
        retentionLimit = limit
        for workspace in workspaces.compactMap(\.value) {
            workspace.setRetentionLimit(limit)
        }
    }

    func updateRecents(_ change: (inout [RecentProject]) -> Void) throws {
        guard recentsReadable else {
            throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Список проектов недоступен для записи. Исправьте или восстановите recent-projects.json."])
        }
        var next = recents
        change(&next)
        try write(next, to: "recent-projects.json")
        recents = next
    }

    func setPanel(_ panel: WorkspacePanel, for projectId: String) {
        var next = panels
        next[projectId] = panel
        panels = next
        guard panelsReadable else {
            error = "Настройки панелей недоступны для записи. Выбор действует до закрытия приложения; исправьте или восстановите panel-settings.json."
            return
        }
        do {
            try write(next, to: "panel-settings.json")
        } catch {
            self.error = "Не удалось сохранить настройки панелей. Выбор действует до закрытия приложения: \(error.localizedDescription)"
        }
    }

    func retain(_ path: String) -> Bool {
        guard !trashing.contains(path) else { return false }
        openPaths[path, default: 0] += 1
        return true
    }
    func release(_ path: String) {
        guard let count = openPaths[path] else { return }
        if count <= 1 { openPaths.removeValue(forKey: path) } else { openPaths[path] = count - 1 }
    }
    func beginTrash(_ path: String) -> Bool {
        guard openPaths[path, default: 0] == 0, !trashing.contains(path) else { return false }
        trashing.insert(path)
        return true
    }
    func endTrash(_ path: String) { trashing.remove(path) }

    func register(_ workspace: WorkspaceModel) {
        workspaces.removeAll { $0.value == nil }
        if !workspaces.contains(where: { $0.value === workspace }) {
            workspaces.append(WeakWorkspace(workspace))
        }
    }

    var blockingWorkspace: WorkspaceModel? {
        workspaces.compactMap(\.value).first { $0.closeBlockReason != nil }
    }
}

@MainActor
private struct RetentionSettingsView: View {
    @ObservedObject private var library = ProjectLibrary.shared
    @StoredState private var unlimited = true
    @StoredState private var count = "10"
    @StoredState private var pending: UInt32?
    @StoredState private var confirming = false
    @StoredState private var error: String?

    private func apply(_ limit: UInt32?) {
        do { try library.setRetentionLimit(limit); error = nil }
        catch { self.error = "Не удалось сохранить настройку: \(readableError(error))" }
    }

    var body: some View {
        Form {
            Text("Вытеснённые версии файлов").font(.headline)
            Picker("Хранение", selection: $unlimited) {
                Text("Без ограничения").tag(true)
                Text("Ограничить количеством").tag(false)
            }
            .pickerStyle(.radioGroup)
            .accessibilityIdentifier("retention-mode")
            TextField("Число завершённых сохранений на проект", text: $count)
                .disabled(unlimited)
                .accessibilityIdentifier("retention-count")
            Text("Учитываются проверенные завершённые сохранения, а не отдельные файлы. Защищённые и нераспознанные записи остаются сверх лимита.")
                .font(.caption).foregroundStyle(.secondary)
            Text("Очистка выполняется только после последующих сохранений проекта, не при открытии и не при изменении настройки.")
                .font(.caption).foregroundStyle(.secondary)
            if let error { Text(error).foregroundStyle(.red) }
            HStack {
                Spacer()
                Button("Применить") {
                    if unlimited {
                        apply(nil)
                    } else if let limit = UInt32(count.trimmingCharacters(in: .whitespacesAndNewlines)) {
                        pending = limit
                        confirming = true
                    } else {
                        error = "Введите целое число от 0 до \(UInt32.max)."
                    }
                }
                .accessibilityIdentifier("retention-apply")
            }
        }
        .padding(24)
        .frame(width: 500)
        .onAppear {
            unlimited = library.retentionLimit == nil
            if let limit = library.retentionLimit { count = String(limit) }
        }
        .alert("Ограничить хранение вытеснённых версий?", isPresented: $confirming) {
            Button("Отмена", role: .cancel) { pending = nil }
            Button("Применить лимит", role: .destructive) {
                if let pending { apply(pending) }
                pending = nil
            }
            .accessibilityIdentifier("retention-confirm")
        } message: {
            Text("После следующих сохранений старые проверенные версии могут быть удалены. Сторонняя программа может всё ещё держать старый файл открытым: её поздние изменения могут быть безвозвратно потеряны. Проверить закрытие всех внешних дескрипторов атомарно невозможно.")
        }
    }
}

private func decodeJSON<T: Decodable>(_ type: T.Type, _ text: String) throws -> T {
    try JSONDecoder().decode(type, from: Data(text.utf8))
}

private func readableError(_ error: Error) -> String {
    if let coreError = error as? CoreError, case let .Failure(code, message) = coreError {
        let summary: String
        switch code {
        case "INVALID_DOCUMENT": summary = "Документ или изменение не прошло проверку."
        case "REVISION_CONFLICT": summary = "Состояние проекта изменилось. Проверьте диагностику и выберите версию."
        case "LOCKED_PROPERTY": summary = "Свойство защищено от этого изменения."
        case "UNSAVED_CHANGES": summary = "Сохраните изменения проекта перед созданием снимка."
        case "MISSING_ASSET": summary = "Не найден необходимый ресурс."
        case "UNSUPPORTED_FEATURE": summary = "Действие или формат не поддерживается этой версией."
        case "IO_ERROR": summary = "Ошибка чтения или записи файлов проекта."
        case "IMPORT_LIMIT_EXCEEDED": summary = "Ресурс превышает допустимый размер или разрешение."
        case "CANCELED": summary = "Операция отменена."
        default: summary = "Операция завершилась ошибкой."
        }
        return "\(summary)\nДетали: \(message)\nКод: \(code)"
    }
    return error.localizedDescription
}

private func wasCanceled(_ error: Error) -> Bool {
    guard let coreError = error as? CoreError, case let .Failure(code, _) = coreError else { return false }
    return code == "CANCELED"
}

private struct ProjectIdentity: Decodable {
    let projectId: String
}

private struct LoadedProject {
    let session: ProjectSession
    let snapshot: Snapshot
}

private func loadProject(_ session: ProjectSession, at url: URL) throws -> LoadedProject {
    var snapshot = try decodeJSON(Snapshot.self, session.snapshotJson())
    if snapshot.document.projectId.isEmpty,
       let data = try? Data(contentsOf: url.appendingPathComponent("project.json")),
       let identity = try? JSONDecoder().decode(ProjectIdentity.self, from: data) {
        snapshot.document.projectId = identity.projectId
    }
    return LoadedProject(session: session, snapshot: snapshot)
}

private struct SaveResult {
    let snapshot: Snapshot
    let error: String?
}

private func editAndSaveSnapshot(_ session: ProjectSession, work: () throws -> String) throws -> SaveResult {
    do {
        return SaveResult(snapshot: try decodeJSON(Snapshot.self, work()), error: nil)
    } catch {
        let operationError = error
        do {
            return SaveResult(snapshot: try decodeJSON(Snapshot.self, session.snapshotJson()), error: readableError(operationError))
        } catch {
            throw NSError(domain: "AIConstructor", code: 1, userInfo: [
                NSLocalizedDescriptionKey: "\(readableError(operationError))\nНе удалось получить состояние проекта: \(readableError(error))"
            ])
        }
    }
}

private func saveSnapshot(_ session: ProjectSession, control: OperationControl) throws -> SaveResult {
    try editAndSaveSnapshot(session) { try session.saveAndSnapshotControlled(control: control) }
}

private struct FrameDraft: Equatable {
    var name: String
    var width: String
    var height: String

    init(_ root: Snapshot.Document.Node) {
        name = root.name
        width = Self.number(root.layout.w)
        height = Self.number(root.layout.h)
    }

    private static func number(_ value: Double) -> String {
        value.rounded() == value && abs(value) < 1e15 ? String(format: "%.0f", value) : String(value)
    }

    var dimensions: (Double, Double)? {
        guard let w = Double(width.replacingOccurrences(of: ",", with: ".")),
              let h = Double(height.replacingOccurrences(of: ",", with: ".")),
              w.isFinite, h.isFinite, w > 0, h > 0,
              !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return (w, h)
    }

    func matches(_ root: Snapshot.Document.Node) -> Bool {
        guard let (w, h) = dimensions else { return false }
        return name == root.name && w == root.layout.w && h == root.layout.h
    }
}

private struct DeferredFrameDraft {
    let draft: FrameDraft
    let base: FrameDraft
    let rootId: String
}

@MainActor
private final class WorkspaceModel: ObservableObject {
    @Published var snapshot: Snapshot?
    @Published var checkpoints: [Checkpoint] = []
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
    @Published private(set) var frameAutosaving = false
    @Published private(set) var frameDraftNeedsReview = false
    @Published private(set) var deferredFrameDraft: DeferredFrameDraft?
    private var operationControl: OperationControl?
    private var diagnosticControl: OperationControl?
    private var possibleUnsaved = false
    private var frameAutosaveTask: Task<Void, Never>?
    private var frameSavePending = false
    private var frameFocused = false

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
        frameAutosaveTask?.cancel()
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
        if frameSavePending {
            frameSavePending = false
            commitFrameDraft()
            if operation != nil { return }
        }
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
        snapshot = loaded.snapshot
        possibleUnsaved = loaded.snapshot.dirty
        checkpoints = []
        deferredChatLoad = false
        deferredChatId = nil
        chats = []
        chatSearchResults = nil
        messages = []
        selectedChatId = nil
        diagnostics = nil
        diagnosticsError = nil
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
            snapshot = nil
            possibleUnsaved = false
            checkpoints = []
            chats = []
            messages = []
            chatSearchResults = nil
            selectedChatId = nil
            diagnostics = nil
            diagnosticsError = nil
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

    func updateFrameDraft(_ field: WritableKeyPath<FrameDraft, String>, value: String) {
        guard deferredFrameDraft == nil, let snapshot, !snapshot.readOnly, let root = snapshot.root else { return }
        var draft = frameDraft ?? FrameDraft(root)
        draft[keyPath: field] = value
        frameAutosaveTask?.cancel()
        frameSavePending = false
        frameDraft = draft
        if writesBlocked { frameDraftNeedsReview = true }
        guard draft.dimensions != nil, !writesBlocked, !frameDraftNeedsReview else { return }
        frameAutosaveTask = Task {
            do { try await Task.sleep(nanoseconds: 500_000_000) } catch { return }
            guard !Task.isCancelled else { return }
            frameSavePending = true
            drainDeferredWork()
        }
    }

    func setFrameFocused(_ focused: Bool) {
        frameFocused = focused
    }

    func discardFrameDraft() {
        guard operation == nil else { return }
        stopFrameAutosaveRetries()
        frameDraft = nil
        frameDraftNeedsReview = false
    }

    func deferFrameDraft() {
        guard operation == nil, deferredFrameDraft == nil, let draft = frameDraft,
              let root = snapshot?.root else { return }
        stopFrameAutosaveRetries()
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
        stopFrameAutosaveRetries()
        frameDraft = nil
    }

    private func stopFrameAutosaveRetries() {
        frameAutosaveTask?.cancel()
        frameSavePending = false
    }

    func commitFrameDraft(reviewed: Bool = false) {
        frameAutosaveTask?.cancel()
        frameSavePending = false
        guard let draft = frameDraft, let (width, height) = draft.dimensions else { return }
        guard !writesBlocked, !frameDraftNeedsReview || reviewed else { return }
        guard operation == nil else { frameSavePending = true; return }
        guard let session, let snapshot, let root = snapshot.root, !snapshot.readOnly else { return }
        frameDraftNeedsReview = false
        if draft.matches(root) {
            if snapshot.dirty || possibleUnsaved {
                let control = newOperationControl()
                frameAutosaving = true
                perform("Сохранение изменений…", control: control, errorContext: "Фрейм «\(root.name)» · \(root.id)", work: {
                    try saveSnapshot(session, control: control)
                }, failure: { _ in
                    self.frameAutosaving = false
                    self.stopFrameAutosaveRetries()
                }) { result in
                    self.frameAutosaving = false
                    if result.error != nil { self.stopFrameAutosaveRetries() }
                    self.accept(result, context: "Фрейм «\(root.name)» · \(root.id)")
                    if result.error == nil && !result.snapshot.dirty && self.frameDraft == draft && !self.frameFocused {
                        self.frameDraft = nil
                    }
                }
            } else {
                if !frameFocused { frameDraft = nil }
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
            frameAutosaving = true
            perform("Сохранение изменений…", control: control, errorContext: "Фрейм «\(root.name)» · \(root.id)", work: {
                try editAndSaveSnapshot(session) {
                    try session.applyTransactionAndSaveControlled(transactionJson: json, control: control)
                }
            }, failure: { error in
                self.frameAutosaving = false
                self.stopFrameAutosaveRetries()
                if wasCanceled(error) { self.possibleUnsaved = previousUnsaved }
            }) { result in
                self.frameAutosaving = false
                if result.error != nil { self.stopFrameAutosaveRetries() }
                self.accept(result, context: "Фрейм «\(root.name)» · \(root.id)")
                if result.error == nil && !result.snapshot.dirty && self.frameDraft == draft && !self.frameFocused {
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
        }) { self.accept($0) }
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
        }) { self.accept($0) }
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

    func restoreCheckpoint(_ checkpoint: Checkpoint) {
        guard operation == nil else { return }
        clearSavedFrameDraft()
        guard !writesBlocked, frameDraft == nil, deferredFrameDraft == nil, let session else { return }
        let previousUnsaved = possibleUnsaved
        possibleUnsaved = true
        let control = newOperationControl()
        perform("Восстановление контрольной точки…", control: control, work: {
            try editAndSaveSnapshot(session) { try session.restoreCheckpointControlled(id: checkpoint.id, control: control) }
        }, failure: { error in
            if wasCanceled(error) { self.possibleUnsaved = previousUnsaved }
        }) { self.accept($0) }
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
              diagnostics?.blockedGenerated?.isEmpty != false, let session, let snapshot else { return }
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
            self.snapshot = $0
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
                    stopFrameAutosaveRetries()
                    frameDraftNeedsReview = true
                }
            }
            diagnosticsError = nil
        } catch {
            guard self.session === session, operation == nil, !Task.isCancelled else { return }
            diagnosticsError = wasCanceled(error) ? "Проверка отменена. Нажмите «Обновить», чтобы повторить её." : readableError(error)
        }
    }
}

private struct WorkspaceView: View {
    @StateObject private var model = WorkspaceModel()
    @ObservedObject private var library = ProjectLibrary.shared
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @StoredState private var renameProject = false
    @StoredState private var projectName = ""
    @StoredState private var trashProject: RecentProject?
    @StoredState private var checkpointToRestore: Checkpoint?
    @StoredState private var confirmPortableSnapshot = false

    @ViewBuilder
    private func projectActions(_ project: RecentProject) -> some View {
        Button("Указать расположение…") { model.locate(project) }
        Button(project.archived == true ? "Вернуть из архива" : "В архив") {
            model.setArchived(project.archived != true, project: project)
        }
        if model.isSessionOpen(project) {
            Button("Закрыть сессию") { model.closeProject(project) }
        }
        Button("Убрать из списка") { model.removeRecent(project) }
        Divider()
        Button("Переместить в Корзину…", role: .destructive) { trashProject = project }
    }

    private func projectRow(_ project: RecentProject) -> some View {
        HStack {
            Button {
                model.open(project)
            } label: {
                VStack(alignment: .leading) {
                    Text(project.name)
                    Text(project.path).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("recent-project-\(project.path)")
            .contextMenu { projectActions(project) }
            Spacer(minLength: 0)
            Menu { projectActions(project) } label: { Image(systemName: "ellipsis.circle") }
                .menuStyle(.borderlessButton)
                .accessibilityLabel("Действия с проектом \(project.name)")
        }
    }

    @ViewBuilder
    private func panel(_ snapshot: Snapshot) -> some View {
        switch library.panels[snapshot.document.projectId] ?? .inspector {
        case .inspector:
            if let root = snapshot.root {
                FrameEditor(snapshot: snapshot, root: root, model: model)
                    .id(snapshot.document.projectId)
            } else {
                ContentUnavailableView("Нет корневого фрейма", systemImage: "exclamationmark.triangle")
            }
        case .chats:
            ChatPanel(model: model, snapshot: snapshot).id(snapshot.document.projectId)
        case .assets:
            AssetsPanel(model: model, snapshot: snapshot)
        case .diagnostics:
            DiagnosticsPanel(model: model)
        }
    }

    var body: some View {
        NavigationSplitView {
            VStack(alignment: .leading, spacing: 16) {
                Text("AI Constructor").font(.title2.bold()).padding(.top, 14)
                HStack {
                    Button("Новый", systemImage: "plus") { model.create() }
                        .accessibilityIdentifier("new-project")
                    Button("Открыть", systemImage: "folder") { model.chooseProject() }
                }
                TextField("Поиск проектов", text: $model.search)
                    .textFieldStyle(.roundedBorder)
                Text("НЕДАВНИЕ").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                List {
                    ForEach(library.recents.filter { $0.archived != true && (model.search.isEmpty || $0.name.localizedCaseInsensitiveContains(model.search)) }) { project in
                        projectRow(project)
                    }
                    let archived = library.recents.filter { $0.archived == true && (model.search.isEmpty || $0.name.localizedCaseInsensitiveContains(model.search)) }
                    if !archived.isEmpty {
                        Section("Архив") {
                            ForEach(archived) { project in projectRow(project) }
                        }
                    }
                }
                .listStyle(.sidebar)
            }
            .disabled(model.frameAutosaving)
            .padding(.horizontal, 16)
            .background(SidebarMaterial())
            .navigationSplitViewColumnWidth(min: 210, ideal: 226, max: 260)
        } detail: {
            if let snapshot = model.snapshot {
                VStack(spacing: 0) {
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(snapshot.name).font(.headline)
                            Text(model.hasUnsavedData ? "Не сохранено" : snapshot.readOnly ? "Только чтение" : model.diagnostics?.readOnly == true ? "Внешние изменения" : model.diagnosticsError != nil ? "Проверка не выполнена" : "Сохранено")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Отменить", systemImage: "arrow.uturn.backward") { model.undo() }.disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
                        Button("Повторить", systemImage: "arrow.uturn.forward") { model.redo() }.disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
                        Button("Сохранить", systemImage: "square.and.arrow.down") { model.save() }.disabled(model.writesBlocked || model.deferredFrameDraft != nil)
                        Menu {
                            Button("Закрыть проект") { model.closeProject() }
                            Button("Переименовать проект…") { projectName = snapshot.name; renameProject = true }
                                .disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
                            Button("В архив") { model.archiveCurrent() }
                            Button("Дублировать проект…") { model.duplicate() }.disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
                            Button("Создать переносимый снимок…") { confirmPortableSnapshot = true }
                                .disabled(model.writesBlocked || model.hasUnsavedData || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil || model.diagnostics?.blockedGenerated?.isEmpty == false)
                            Button("Создать контрольную точку") { model.createCheckpoint() }.disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
                            Button("Обновить список контрольных точек") { model.refreshCheckpoints() }
                                .disabled(model.writesBlocked)
                            Menu("Восстановить контрольную точку") {
                                ForEach(model.checkpoints) { checkpoint in
                                    Button("Ревизия \(checkpoint.revision) · \(checkpoint.id.prefix(8))") {
                                        checkpointToRestore = checkpoint
                                    }
                                }
                            }
                            .disabled(model.writesBlocked || model.checkpoints.isEmpty || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
                        } label: { Image(systemName: "ellipsis.circle") }
                        .accessibilityLabel("Действия с проектом")
                        .accessibilityIdentifier("project-actions")
                    }
                    .disabled(model.frameAutosaving)
                    .padding(20)
                    Divider()
                    if let reason = snapshot.readOnlyReason {
                        Text(reason).frame(maxWidth: .infinity, alignment: .leading).padding().background(.orange.opacity(0.12))
                    }
                    if let diagnostics = model.diagnostics, diagnostics.manifestChanged || !diagnostics.files.isEmpty || !diagnostics.conflicts.isEmpty || diagnostics.blockedGenerated?.isEmpty == false {
                        HStack {
                            Label("Обнаружены внешние изменения или конфликты.", systemImage: "exclamationmark.triangle")
                            Spacer()
                            Button("Открыть диагностику") { library.setPanel(.diagnostics, for: snapshot.document.projectId) }
                                .accessibilityIdentifier("open-diagnostics")
                        }
                        .padding(10)
                        .background(.orange.opacity(0.12))
                    }
                    if model.diagnosticsError != nil || model.watchWarning != nil {
                        HStack {
                            Label(model.diagnosticsError ?? model.watchWarning ?? "", systemImage: "exclamationmark.triangle")
                            Spacer()
                            Button("Открыть диагностику") { library.setPanel(.diagnostics, for: snapshot.document.projectId) }
                        }
                        .padding(10)
                        .background(.orange.opacity(0.12))
                    }
                    Picker("Панель", selection: Binding(
                        get: { library.panels[snapshot.document.projectId] ?? .inspector },
                        set: { library.setPanel($0, for: snapshot.document.projectId) }
                    )) {
                        ForEach(WorkspacePanel.allCases, id: \.self) { value in
                            Text(value.title).tag(value).accessibilityIdentifier("panel-\(value.rawValue)")
                        }
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("workspace-panel")
                    .disabled(model.frameAutosaving)
                    .padding([.horizontal, .top])
                    panel(snapshot)
                        .disabled(model.frameAutosaving && (library.panels[snapshot.document.projectId] ?? .inspector) != .inspector)
                    Spacer(minLength: 0)
                }
            } else {
                ContentUnavailableView {
                    Label("Выберите проект", systemImage: "square.stack.3d.up")
                } description: {
                    Text("Создайте проект или откройте существующую папку.")
                } actions: {
                    Button("Создать проект") { model.create() }
                        .buttonStyle(.borderedProminent)
                    Button("Открыть папку") { model.chooseProject() }
                }
            }
        }
        .background(WindowCloseGuard(model: model))
        .onAppear { library.register(model) }
        .disabled(model.operation != nil && !model.conflictReviewPresented && !model.frameAutosaving)
        .overlay(alignment: .bottom) {
            if let operation = model.operation {
                HStack {
                    ProgressView().controlSize(.small)
                    VStack(alignment: .leading) {
                        Text(operation)
                        if let message = model.cancellationMessage { Text(message).font(.caption) }
                    }
                    if model.canCancelOperation {
                        Button("Отменить операцию") { model.cancelOperation() }
                    }
                }
                .padding(12)
                .background {
                    if reduceTransparency {
                        RoundedRectangle(cornerRadius: 10).fill(Color(nsColor: .windowBackgroundColor))
                    } else {
                        RoundedRectangle(cornerRadius: 10).fill(.regularMaterial)
                    }
                }
                .padding()
            }
        }
        .task(id: model.snapshot?.document.projectId) {
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 30_000_000_000) } catch { break }
                await model.pollDiagnostics()
            }
        }
        .alert("Ошибка", isPresented: Binding(get: { !renameProject && !model.chatEditorPresented && !model.conflictReviewPresented && (model.error != nil || library.error != nil) }, set: {
            if !$0 { model.error = nil; library.error = nil }
        })) {
            Button("OK") { model.error = nil; library.error = nil }
        } message: { Text(model.error ?? library.error ?? "") }
        .alert("Переместить проект в Корзину?", isPresented: Binding(get: { trashProject != nil }, set: {
            if !$0 { trashProject = nil }
        }), presenting: trashProject) { project in
            Button("Переместить в Корзину", role: .destructive) { model.trash(project); trashProject = nil }
            Button("Отмена", role: .cancel) { trashProject = nil }
        } message: { project in
            Text("Папка \(project.path) будет перемещена в Корзину. Это отдельное действие и не совпадает с удалением записи из списка.")
        }
        .alert("Восстановить контрольную точку?", isPresented: Binding(get: { checkpointToRestore != nil }, set: {
            if !$0 { checkpointToRestore = nil }
        }), presenting: checkpointToRestore) { checkpoint in
            Button("Восстановить", role: .destructive) { model.restoreCheckpoint(checkpoint); checkpointToRestore = nil }
            Button("Отмена", role: .cancel) { checkpointToRestore = nil }
        } message: { checkpoint in
            Text("Будет восстановлена ревизия \(checkpoint.revision) вместе с файлами снимка. Текущие файлы проекта могут измениться.")
        }
        .alert("Создать переносимый снимок?", isPresented: $confirmPortableSnapshot) {
            Button("Продолжить") { model.createPortableSnapshot() }
            Button("Отмена", role: .cancel) {}
        } message: {
            Text("Снимок включает файлы проекта, чаты, визуальные образцы и архивы конфликтов со старыми версиями файлов. Они могут содержать приватные данные. Проверьте содержимое перед публикацией или передачей. Несохранённые черновики чатов не включаются.")
        }
        .alert("Переносимый снимок создан", isPresented: Binding(get: { model.notice != nil }, set: {
            if !$0 { model.notice = nil }
        })) {
            Button("OK") { model.notice = nil }
        } message: { Text(model.notice ?? "") }
        .sheet(isPresented: $renameProject) {
            VStack(alignment: .leading, spacing: 16) {
                Text("Переименовать проект").font(.headline)
                TextField("Название", text: $projectName).disabled(model.operation != nil)
                if model.writesBlocked { Text("Сначала разрешите внешние изменения в диагностике.").foregroundStyle(.orange) }
                if let error = model.error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
                if let operation = model.operation {
                    HStack {
                        ProgressView(operation)
                        if let message = model.cancellationMessage { Text(message).font(.caption) }
                        if model.canCancelOperation { Button("Отменить операцию") { model.cancelOperation() } }
                    }
                }
                HStack {
                    Spacer()
                    Button("Отмена") { model.error = nil; renameProject = false }.disabled(model.operation != nil)
                    Button("Сохранить") {
                        guard model.operation == nil else { return }
                        model.error = nil
                        model.rename(projectName) { renameProject = false }
                    }
                        .keyboardShortcut(.defaultAction)
                        .disabled(model.operation != nil || model.writesBlocked || projectName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }.padding(24).frame(width: 420)
                .interactiveDismissDisabled(model.operation != nil)
        }
    }
}

private struct ChatPanel: View {
    @ObservedObject var model: WorkspaceModel
    let snapshot: Snapshot
    @StoredState private var showChatSheet = false
    @StoredState private var editingChat: Chat?
    @StoredState private var forkOrigin: Chat?
    @StoredState private var forkMessage: ChatMessage?
    @StoredState private var deleteCandidate: Chat?
    @StoredState private var title = ""
    @StoredState private var mode = "discuss"
    @StoredState private var search = ""

    private var selected: Chat? {
        model.chats.first { $0.id == model.selectedChatId }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Picker("Чат", selection: Binding(
                    get: { model.selectedChatId ?? "" },
                    set: { model.chatSearchResults = nil; model.refreshChats(select: $0) }
                )) {
                    Text("Выберите чат").tag("")
                    ForEach(model.chats) { chat in
                        Text(chat.archived ? "\(chat.title) · архив" : chat.title).tag(chat.id)
                    }
                }
                .accessibilityIdentifier("chat-picker")
                Button("Новый чат", systemImage: "plus") {
                    editingChat = nil
                    forkOrigin = nil
                    forkMessage = nil
                    title = ""
                    mode = "discuss"
                    showChatSheet = true
                }
                .disabled(model.writesBlocked)
                if let selected {
                    Menu("Действия с чатом") {
                        Button("Переименовать…") {
                            editingChat = selected
                            forkOrigin = nil
                            forkMessage = nil
                            title = selected.title
                            mode = selected.mode
                            showChatSheet = true
                        }
                        Button(selected.archived ? "Вернуть из архива" : "В архив") {
                            model.archiveChat(selected, archived: !selected.archived)
                        }
                        Menu("Режим") {
                            Button("Обсудить") { model.setChatMode(selected, mode: "discuss") }
                            Button("План") { model.setChatMode(selected, mode: "plan") }
                            Button("Изменить") { model.setChatMode(selected, mode: "edit") }
                        }
                        Divider()
                        Button("Удалить чат…", role: .destructive) { deleteCandidate = selected }
                    }
                    .disabled(model.writesBlocked)
                }
            }
            Text("Локальная история чата. Ответы ИИ в этом этапе не подключены.")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                TextField("Поиск по всем чатам проекта", text: $search)
                    .textFieldStyle(.roundedBorder)
                Button("Искать") { model.searchChats(search.trimmingCharacters(in: .whitespacesAndNewlines)) }
                    .disabled(search.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                if model.chatSearchResults != nil {
                    Button("Закрыть поиск") { model.chatSearchResults = nil }
                }
            }
            if let results = model.chatSearchResults {
                if results.isEmpty {
                    ContentUnavailableView("Сообщения не найдены", systemImage: "magnifyingglass")
                } else {
                    List(results) { hit in
                        Button {
                            model.chatSearchResults = nil
                            model.refreshChats(select: hit.chat.id)
                        } label: {
                            VStack(alignment: .leading) {
                                Text(hit.chat.archived ? "\(hit.chat.title) · архив" : hit.chat.title).fontWeight(.medium)
                                Text(hit.message.content.compactMap(\.text).joined(separator: "\n")).lineLimit(3)
                            }
                        }
                    }
                }
            } else if let selected {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(model.messages) { message in
                            VStack(alignment: .leading, spacing: 5) {
                                HStack {
                                    Text(message.role == "user" ? "Вы" : message.role == "assistant" ? "Ассистент" : "Система")
                                        .font(.caption.weight(.semibold))
                                    Spacer()
                                    Button("Ответвить…") {
                                        forkOrigin = selected
                                        forkMessage = message
                                        editingChat = nil
                                        title = "\(selected.title) — ответвление"
                                        showChatSheet = true
                                    }
                                    .disabled(model.writesBlocked)
                                    .accessibilityLabel("Ответвить чат с этого сообщения")
                                }
                                ForEach(Array(message.content.enumerated()), id: \.offset) { item in
                                    if let text = item.element.text {
                                        Text(text).textSelection(.enabled)
                                    } else {
                                        Text("Содержимое: \(item.element.type)").foregroundStyle(.secondary)
                                    }
                                }
                                ForEach(message.attachments, id: \.assetId) { attachment in
                                    let path = snapshot.document.assets.first { $0.id == attachment.assetId }?.path ?? attachment.assetId
                                    Label(path, systemImage: "paperclip").font(.caption).textSelection(.enabled)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(10)
                            .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                if !selected.archived {
                    let draftKey = "\(snapshot.document.projectId):\(selected.id)"
                    TextEditor(text: Binding(
                        get: { model.chatDrafts[draftKey, default: ""] },
                        set: { model.chatDrafts[draftKey] = $0 }
                    ))
                    .frame(minHeight: 75, maxHeight: 120)
                    .accessibilityLabel("Текст сообщения")
                    .accessibilityIdentifier("chat-message")
                    .disabled(model.writesBlocked)
                    HStack {
                        Picker("Вложение", selection: Binding(
                            get: { model.chatAttachmentDrafts[draftKey, default: ""] },
                            set: { model.chatAttachmentDrafts[draftKey] = $0 }
                        )) {
                            Text("Без вложения").tag("")
                            ForEach(snapshot.document.assets) { asset in
                                Text(asset.path).tag(asset.id)
                            }
                        }
                        Button("Сохранить сообщение") {
                            let text = model.chatDrafts[draftKey, default: ""]
                            let attachment = model.chatAttachmentDrafts[draftKey, default: ""]
                            model.appendMessage(text, attachmentIds: attachment.isEmpty ? [] : [attachment]) {
                                model.chatDrafts[draftKey] = ""
                                model.chatAttachmentDrafts[draftKey] = ""
                            }
                        }
                        .accessibilityIdentifier("save-chat-message")
                        .disabled(model.writesBlocked || (model.chatDrafts[draftKey, default: ""].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && model.chatAttachmentDrafts[draftKey, default: ""].isEmpty))
                    }
                } else {
                    Text("Чат в архиве. Верните его из архива, чтобы добавлять сообщения.")
                        .foregroundStyle(.secondary)
                }
            } else {
                ContentUnavailableView("Нет открытого чата", systemImage: "bubble.left.and.bubble.right")
            }
        }
        .padding(20)
        .onAppear { model.requestChats(select: model.selectedChatId) }
        .onChange(of: showChatSheet) { _, presented in model.chatEditorPresented = presented }
        .onDisappear { model.chatEditorPresented = false }
        .sheet(isPresented: $showChatSheet) {
            VStack(alignment: .leading, spacing: 16) {
                Text(forkOrigin != nil ? "Ответвить чат" : editingChat == nil ? "Новый чат" : "Переименовать чат").font(.headline)
                TextField("Название", text: $title).accessibilityIdentifier("chat-title").disabled(model.operation != nil)
                if editingChat == nil && forkOrigin == nil {
                    Picker("Режим", selection: $mode) {
                        Text("Обсудить").tag("discuss")
                        Text("План").tag("plan")
                        Text("Изменить").tag("edit")
                    }
                    .disabled(model.operation != nil)
                }
                if model.writesBlocked { Text("Сначала разрешите внешние изменения в диагностике.").foregroundStyle(.orange) }
                if let error = model.error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
                if let operation = model.operation {
                    HStack {
                        ProgressView(operation)
                        if let message = model.cancellationMessage { Text(message).font(.caption) }
                        if model.canCancelOperation { Button("Отменить операцию") { model.cancelOperation() } }
                    }
                }
                HStack {
                    Spacer()
                    Button("Отмена") { model.error = nil; showChatSheet = false }.disabled(model.operation != nil)
                    Button("Сохранить") {
                        guard model.operation == nil else { return }
                        model.error = nil
                        if let forkOrigin, let forkMessage {
                            model.forkChat(forkOrigin, through: forkMessage, title: title) { showChatSheet = false }
                        } else if let editingChat {
                            model.renameChat(editingChat, title: title) { showChatSheet = false }
                        } else {
                            model.createChat(title: title, mode: mode) { showChatSheet = false }
                        }
                    }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("confirm-chat")
                    .disabled(model.operation != nil || model.writesBlocked || title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }.padding(24).frame(width: 420)
                .interactiveDismissDisabled(model.operation != nil)
        }
        .alert("Удалить чат?", isPresented: Binding(get: { deleteCandidate != nil }, set: {
            if !$0 { deleteCandidate = nil }
        }), presenting: deleteCandidate) { chat in
            Button("Удалить чат", role: .destructive) {
                guard model.operation == nil else { return }
                model.deleteChat(chat)
                deleteCandidate = nil
            }
            .disabled(model.operation != nil)
            Button("Отмена", role: .cancel) { deleteCandidate = nil }
        } message: { chat in
            Text("Переписка «\(chat.title)» будет удалена из проекта. Применённые изменения дизайна останутся в истории проекта.")
        }
    }
}

private struct AssetsPanel: View {
    @ObservedObject var model: WorkspaceModel
    let snapshot: Snapshot
    @StoredState private var repairAsset: Snapshot.Document.Asset?

    private func changed(_ asset: Snapshot.Document.Asset) -> Bool {
        model.diagnostics?.files.contains { $0.path == asset.path && ($0.kind == "missing" || $0.kind == "modified") } == true
    }

    private func canRepair(_ asset: Snapshot.Document.Asset) -> Bool {
        guard let diagnostics = model.diagnostics, !diagnostics.manifestChanged,
              !diagnostics.files.isEmpty else { return false }
        return diagnostics.files.allSatisfy { $0.path == asset.path && ($0.kind == "missing" || $0.kind == "modified") }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Ресурсы проекта").font(.headline)
                Spacer()
                Button("Добавить ресурс…") { model.importAsset() }
                    .accessibilityIdentifier("import-asset")
                    .disabled(model.writesBlocked)
            }
            if snapshot.document.assets.isEmpty {
                ContentUnavailableView("Нет ресурсов", systemImage: "photo")
            } else {
                List(snapshot.document.assets) { asset in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(asset.path).textSelection(.enabled)
                            Text(asset.id).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                        }
                        Spacer()
                        Button(changed(asset) ? "Восстановить…" : "Перепривязать…") {
                            if changed(asset) { repairAsset = asset } else { model.relinkAsset(asset) }
                        }
                        .disabled((snapshot.readOnly || model.diagnostics?.readOnly == true || changed(asset)) && !canRepair(asset))
                    }
                }
            }
            if model.diagnostics?.files.contains(where: { $0.path.hasPrefix("assets/") }) == true {
                Text("При восстановлении сохранённая повреждённая версия помещается в conflicts; другие одновременные конфликты необходимо разрешить отдельно.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(20)
        .alert("Восстановить ресурс?", isPresented: Binding(get: { repairAsset != nil }, set: {
            if !$0 { repairAsset = nil }
        }), presenting: repairAsset) { asset in
            Button("Выбрать замену…") { model.relinkAsset(asset); repairAsset = nil }
            Button("Отмена", role: .cancel) { repairAsset = nil }
        } message: { asset in
            Text("Выберите замену для \(asset.path). Повреждённая версия будет сохранена в conflicts, если она доступна. Если файл уже отсутствует, его содержимое восстановить нельзя; отмена через эту операцию будет недоступна.")
        }
    }
}

private struct ConflictReview: Identifiable {
    let conflict: ProjectDiagnostics.Conflict
    let diskToken: String
    let localGeneration: UInt64
    var id: String { conflict.id }
}

private struct DiagnosticsPanel: View {
    @ObservedObject var model: WorkspaceModel
    @StoredState private var review: ConflictReview?
    @StoredState private var confirmDiscardFrame = false
    @StoredState private var confirmDiscardDeferred = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Диагностика проекта").font(.headline)
                Spacer()
                if model.diagnosticsInFlight {
                    ProgressView("Проверка…").controlSize(.small)
                    Button("Отменить проверку") { model.cancelBackgroundDiagnostics() }
                } else {
                    Button("Обновить") { Task { await model.pollDiagnostics() } }
                }
            }
            if model.frameDraft != nil {
                Label("Есть неразрешённый черновик фрейма. Отложите его или явно отбросьте до выбора версии.", systemImage: "exclamationmark.triangle")
                Button("Отложить черновик до разрешения") { model.deferFrameDraft() }
                    .accessibilityIdentifier("defer-frame-draft")
                    .disabled(model.operation != nil)
                Button("Отбросить черновик фрейма…") { confirmDiscardFrame = true }
                    .accessibilityIdentifier("discard-frame-draft")
                    .disabled(model.operation != nil)
            }
            if let deferred = model.deferredFrameDraft {
                Label("Черновик отложен в памяти. После выбора версии проверьте его перед применением.", systemImage: "doc.text")
                VStack(alignment: .leading) {
                    Text("Название: \(deferred.draft.name)")
                    Text("Ширина: \(deferred.draft.width)")
                    Text("Высота: \(deferred.draft.height)")
                }
                .textSelection(.enabled)
                HStack {
                    Button("Вернуть черновик") { model.restoreDeferredFrameDraft() }
                    Button("Отбросить отложенный черновик…") { confirmDiscardDeferred = true }
                }
                .disabled(model.operation != nil)
            }
            if let warning = model.watchWarning {
                Label(warning, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary)
            }
            if let error = model.diagnosticsError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .textSelection(.enabled)
            } else if let diagnostics = model.diagnostics {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        if let reason = diagnostics.readOnlyReason {
                            Label(reason, systemImage: "lock").textSelection(.enabled)
                        }
                        if diagnostics.manifestChanged {
                            Label("Манифест изменён вне приложения.", systemImage: "exclamationmark.triangle")
                        }
                        if diagnostics.files.isEmpty && diagnostics.conflicts.isEmpty && !diagnostics.manifestChanged && diagnostics.blockedGenerated?.isEmpty != false {
                            Label("Внешние изменения не обнаружены.", systemImage: "checkmark.circle")
                        }
                        ForEach(diagnostics.files) { file in
                            VStack(alignment: .leading, spacing: 4) {
                                Text("\(file.kind): \(file.path)").fontWeight(.medium)
                                if let expected = file.expectedHash { Text("Ожидалось: \(expected)").font(.caption.monospaced()) }
                                if let actual = file.actualHash { Text("На диске: \(actual)").font(.caption.monospaced()) }
                            }
                            .textSelection(.enabled)
                        }
                        if let blocked = diagnostics.blockedGenerated, !blocked.isEmpty {
                            Label("Внешний сгенерированный код не синхронизирован с документом; повторная генерация и создание переносимого снимка заблокированы до снятия блокировки.", systemImage: "exclamationmark.triangle")
                            ForEach(blocked, id: \.self) { path in Text(path).textSelection(.enabled) }
                        }
                        ForEach(diagnostics.conflicts) { conflict in
                            VStack(alignment: .leading) {
                                Text("Конфликт \(conflict.id)").fontWeight(.medium)
                                Text(conflict.reason)
                                if conflict.kind == "retainedWrite" {
                                    Text("Обнаружена поздняя запись в вытесненный файл. Её сохранённая версия может отличаться от текущего файла проекта.")
                                        .foregroundStyle(.secondary)
                                }
                                if ["externalChange", "retainedWrite"].contains(conflict.kind ?? ""),
                                   let paths = conflict.paths, !paths.isEmpty,
                                   paths.allSatisfy({ !$0.choices.isEmpty }),
                                   conflict.choices?.isEmpty == false,
                                   diagnostics.schemaVersion == 1,
                                   let diskToken = diagnostics.observedDiskToken,
                                   let generation = diagnostics.localGeneration {
                                    Button("Просмотреть и разрешить…") {
                                        model.conflictReviewPresented = true
                                        review = ConflictReview(conflict: conflict, diskToken: diskToken, localGeneration: generation)
                                    }
                                    .disabled(model.frameDraft != nil || model.operation != nil)
                                }
                                if conflict.kind == "recovery",
                                   let snapshots = conflict.snapshots,
                                   let choices = conflict.choices,
                                   (choices.contains("old") && snapshots.old.available) || (choices.contains("new") && snapshots.new.available),
                                   diagnostics.schemaVersion == 1,
                                   let diskToken = diagnostics.observedDiskToken,
                                   let generation = diagnostics.localGeneration {
                                    Button("Просмотреть варианты восстановления…") {
                                        model.conflictReviewPresented = true
                                        review = ConflictReview(conflict: conflict, diskToken: diskToken, localGeneration: generation)
                                    }
                                    .disabled(model.frameDraft != nil || model.operation != nil)
                                }
                            }
                            .textSelection(.enabled)
                        }
                        if diagnostics.manifestChanged || !diagnostics.files.isEmpty || !diagnostics.conflicts.isEmpty {
                            Text("Перед выбором сохраните копии внешних версий. Для конфликтов без доступных вариантов используйте проверенную резервную копию и затем заново откройте проект.")
                                .foregroundStyle(.secondary)
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                ProgressView("Проверка проекта…")
            }
        }
        .padding(20)
        .sheet(item: $review, onDismiss: { model.conflictReviewPresented = false }) { item in
            if item.conflict.kind == "recovery" {
                RecoveryResolutionSheet(model: model, review: item)
            } else {
                ConflictResolutionSheet(model: model, review: item)
            }
        }
        .alert("Отбросить черновик фрейма?", isPresented: $confirmDiscardFrame) {
            Button("Отбросить черновик", role: .destructive) { model.discardFrameDraft() }
                .disabled(model.operation != nil)
            Button("Отмена", role: .cancel) {}
        } message: {
            Text("Не применённый ввод в полях фрейма будет потерян. Уже применённые изменения документа не отменяются.")
        }
        .alert("Отбросить отложенный черновик?", isPresented: $confirmDiscardDeferred) {
            Button("Отбросить черновик", role: .destructive) { model.discardDeferredFrameDraft() }
                .disabled(model.operation != nil)
            Button("Отмена", role: .cancel) {}
        } message: {
            Text("Отложенный ввод будет потерян. При необходимости сначала скопируйте значения из диагностики.")
        }
    }
}

private struct ConflictResolutionSheet: View {
    @ObservedObject var model: WorkspaceModel
    let review: ConflictReview
    @Environment(\.dismiss) private var dismiss
    @StoredState private var selections: [String: String] = [:]
    @StoredState private var confirm = false

    private var paths: [ProjectDiagnostics.ConflictPath] { review.conflict.paths ?? [] }

    private func title(_ choice: String) -> String {
        switch choice {
        case "base": return "Исходная версия"
        case "local": return "Версия редактора"
        case "disk": return review.conflict.kind == "retainedWrite" ? "Поздняя запись во внешний дескриптор" : "Версия на диске"
        default: return choice
        }
    }

    private func version(_ choice: String, in path: ProjectDiagnostics.ConflictPath) -> ProjectDiagnostics.Version {
        switch choice {
        case "base": return path.base
        case "local": return path.local
        default: return path.disk
        }
    }

    private var complete: Bool {
        !paths.isEmpty && paths.allSatisfy { path in
            guard let selected = selections[path.path] else { return false }
            return ["base", "local", "disk"].contains(selected) && path.choices.contains(selected) && review.conflict.choices?.contains(selected) == true
        }
    }

    private var removedPaths: [String] {
        paths.compactMap { path in
            guard let selected = selections[path.path], version(selected, in: path).state == "absent" else { return nil }
            return path.path
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Разрешение конфликта").font(.headline)
            Text("Выберите версию для каждого файла. Никакая версия не выбирается автоматически.")
                .foregroundStyle(.secondary)
            if review.conflict.kind == "retainedWrite" {
                Text("Поздняя запись получена из вытеснённого открытого файла, а не из текущего файла проекта.")
                    .foregroundStyle(.secondary)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    ForEach(paths) { path in
                        VStack(alignment: .leading, spacing: 8) {
                            Text(path.path).fontWeight(.semibold).textSelection(.enabled)
                            ForEach(["base", "local", "disk"], id: \.self) { choice in
                                let item = version(choice, in: path)
                                VStack(alignment: .leading, spacing: 3) {
                                    HStack {
                                        Text(title(choice)).fontWeight(.medium)
                                        Text(item.state == "absent" ? "Файл отсутствует" : item.state == "unavailable" ? "Версия недоступна" : "Сохранённая копия")
                                            .foregroundStyle(.secondary)
                                        Spacer()
                                        if let archive = item.archiveRelativePath {
                                            Button("Показать копию в Finder") { model.revealConflictVersion(archive) }
                                        }
                                    }
                                    if let hash = item.hash ?? item.expectedHash {
                                        Text("SHA-256: \(hash)").font(.caption.monospaced()).textSelection(.enabled)
                                    }
                                    if let size = item.size { Text("Размер: \(size) байт").font(.caption) }
                                }
                            }
                            Picker("Использовать", selection: Binding(
                                get: { selections[path.path] ?? "" },
                                set: { selections[path.path] = $0 }
                            )) {
                                Text("Выберите версию").tag("")
                                ForEach(path.choices.filter { ["base", "local", "disk"].contains($0) && review.conflict.choices?.contains($0) == true }, id: \.self) { choice in
                                    Text(title(choice)).tag(choice)
                                }
                            }
                        }
                        Divider()
                    }
                }
            }
            .disabled(model.operation != nil)
            if let error = model.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            if let operation = model.operation {
                HStack {
                    ProgressView(operation)
                    if let message = model.cancellationMessage { Text(message).font(.caption) }
                    if model.canCancelOperation {
                        Button("Отменить операцию") { model.cancelOperation() }
                    }
                }
            }
            HStack {
                Spacer()
                Button("Закрыть") { dismiss() }.disabled(model.operation != nil)
                Button("Применить выбор…") { confirm = true }
                    .buttonStyle(.borderedProminent)
                    .disabled(!complete || model.operation != nil)
            }
        }
        .padding(24)
        .frame(minWidth: 620, minHeight: 440)
        .interactiveDismissDisabled(model.operation != nil)
        .alert("Применить выбранные версии?", isPresented: $confirm) {
            Button("Применить") {
                model.resolveConflict(review, selections: selections) { dismiss() }
            }
            Button("Отмена", role: .cancel) {}
        } message: {
            if removedPaths.isEmpty {
                Text("Приложение проверит, что ни одна версия не изменилась после открытия этого окна.")
            } else {
                Text("Для выбранной отсутствующей версии будут удалены пути: \(removedPaths.joined(separator: ", ")). Приложение проверит актуальность версий перед записью.")
            }
        }
    }
}

private struct RecoveryResolutionSheet: View {
    @ObservedObject var model: WorkspaceModel
    let review: ConflictReview
    @Environment(\.dismiss) private var dismiss
    @StoredState private var take = ""
    @StoredState private var confirm = false

    private func snapshot(_ choice: String) -> ProjectDiagnostics.RecoverySnapshot? {
        guard let snapshots = review.conflict.snapshots else { return nil }
        return choice == "old" ? snapshots.old : snapshots.new
    }

    private func title(_ choice: String) -> String {
        choice == "old" ? "Предыдущий снимок" : "Новый снимок"
    }

    private var choices: [String] {
        ["old", "new"].filter {
            review.conflict.choices?.contains($0) == true && snapshot($0)?.available == true
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Восстановление после прерванной записи").font(.headline)
            Text("Выберите один проверенный снимок целиком. Смешивать файлы разных снимков нельзя.")
                .foregroundStyle(.secondary)
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    ForEach(["old", "new"], id: \.self) { choice in
                        if let snapshot = snapshot(choice) {
                            VStack(alignment: .leading, spacing: 6) {
                                HStack {
                                    Text(title(choice)).fontWeight(.semibold)
                                    Text(snapshot.available ? "Доступен" : "Недоступен").foregroundStyle(.secondary)
                                    Spacer()
                                    if let path = snapshot.manifestPath {
                                        Button("Показать манифест") { model.revealConflictVersion(path) }
                                    }
                                }
                                if let revision = snapshot.revision { Text("Ревизия: \(revision)") }
                                if let generation = snapshot.storageGeneration { Text("Поколение хранения: \(generation)") }
                                if let hash = snapshot.manifestHash {
                                    Text("SHA-256 манифеста: \(hash)").font(.caption.monospaced()).textSelection(.enabled)
                                }
                                ForEach(snapshot.files ?? []) { file in
                                    HStack {
                                        VStack(alignment: .leading) {
                                            Text(file.path).textSelection(.enabled)
                                            if let hash = file.hash { Text(hash).font(.caption.monospaced()).textSelection(.enabled) }
                                        }
                                        Spacer()
                                        if let path = file.archiveRelativePath {
                                            Button("Показать копию") { model.revealConflictVersion(path) }
                                        }
                                    }
                                }
                            }
                            Divider()
                        }
                    }
                }
            }
            .disabled(model.operation != nil)
            Picker("Восстановить", selection: $take) {
                Text("Выберите снимок").tag("")
                ForEach(choices, id: \.self) { choice in Text(title(choice)).tag(choice) }
            }
            .disabled(model.operation != nil)
            if let error = model.error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
            if let operation = model.operation {
                HStack {
                    ProgressView(operation)
                    if let message = model.cancellationMessage { Text(message).font(.caption) }
                    if model.canCancelOperation {
                        Button("Отменить операцию") { model.cancelOperation() }
                    }
                }
            }
            HStack {
                Spacer()
                Button("Закрыть") { dismiss() }.disabled(model.operation != nil)
                Button("Восстановить…") { confirm = true }
                    .buttonStyle(.borderedProminent)
                    .disabled(!choices.contains(take) || model.operation != nil)
            }
        }
        .padding(24)
        .frame(minWidth: 620, minHeight: 440)
        .interactiveDismissDisabled(model.operation != nil)
        .alert("Восстановить выбранный снимок?", isPresented: $confirm) {
            Button("Восстановить", role: .destructive) {
                model.resolveRecovery(review, take: take) { dismiss() }
            }
            Button("Отмена", role: .cancel) {}
        } message: {
            Text("Будет восстановлен \(title(take).lowercased()) целиком. Перед записью приложение проверит актуальность файлов и сохранит неизвестные версии.")
        }
    }
}

private struct FrameEditor: View {
    let snapshot: Snapshot
    let root: Snapshot.Document.Node
    @ObservedObject var model: WorkspaceModel
    private enum Field: Hashable { case name, width, height }
    @FocusState private var focus: Field?
    init(snapshot: Snapshot, root: Snapshot.Document.Node, model: WorkspaceModel) {
        self.snapshot = snapshot
        self.root = root
        self.model = model
    }

    private func binding(_ field: WritableKeyPath<FrameDraft, String>) -> Binding<String> {
        Binding(
            get: { (model.frameDraft ?? FrameDraft(root))[keyPath: field] },
            set: { model.updateFrameDraft(field, value: $0) }
        )
    }

    var body: some View {
        HStack(alignment: .top, spacing: 24) {
            VStack(alignment: .leading, spacing: 12) {
                Text("Страница 1").font(.title3.bold())
                Text("Корневой фрейм").foregroundStyle(.secondary)
                Text("Показан только контур: элементы, стили и поведение документа пока не отображаются.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                RoundedRectangle(cornerRadius: 14)
                    .fill(Color(nsColor: .controlBackgroundColor))
                    .overlay {
                        VStack(spacing: 4) {
                            Text("Контур фрейма")
                            Text("\(root.layout.w.formatted()) × \(root.layout.h.formatted())")
                        }.foregroundStyle(.secondary)
                    }
                    .aspectRatio(root.layout.w / root.layout.h, contentMode: .fit)
                    .overlay(RoundedRectangle(cornerRadius: 14).stroke(.primary.opacity(0.12)))
                    .padding(30)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 16))
            }
            Form {
                TextField("Название фрейма", text: binding(\.name))
                    .focused($focus, equals: .name)
                    .accessibilityIdentifier("frame-name")
                TextField("Ширина", text: binding(\.width))
                    .focused($focus, equals: .width)
                    .accessibilityIdentifier("frame-width")
                TextField("Высота", text: binding(\.height))
                    .focused($focus, equals: .height)
                    .accessibilityIdentifier("frame-height")
                if let draft = model.frameDraft, draft.dimensions == nil {
                    Text("Укажите название и положительные размеры.")
                        .font(.caption).foregroundStyle(.red)
                }
                if model.frameDraftNeedsReview {
                    Text("Проверьте отложенный черновик и нажмите «Применить».")
                        .font(.caption).foregroundStyle(.orange)
                }
                Button("Применить") { model.commitFrameDraft(reviewed: true) }
                    .accessibilityIdentifier("apply-frame")
                    .buttonStyle(.borderedProminent)
                    .clipShape(Capsule())
                    .disabled(model.writesBlocked || model.deferredFrameDraft != nil || (model.frameDraft.map { $0.dimensions == nil } ?? false))
            }
            .formStyle(.grouped)
            .frame(width: 270)
            .disabled(snapshot.readOnly || model.deferredFrameDraft != nil || (model.writesBlocked && model.frameDraft == nil))
        }
        .padding(24)
        .onChange(of: focus) { previous, next in
            model.setFrameFocused(next != nil)
            if previous != nil && previous != next { model.commitFrameDraft() }
        }
        .onDisappear {
            model.setFrameFocused(false)
            model.commitFrameDraft()
        }
    }
}

private struct SidebarMaterial: View {
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        if reduceTransparency {
            Color(nsColor: .windowBackgroundColor)
        } else if #available(macOS 26.0, *) {
            Color.clear.glassEffect(.regular, in: .rect(cornerRadius: 0))
        } else {
            VisualEffect()
        }
    }
}

private struct VisualEffect: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .sidebar
        view.blendingMode = .behindWindow
        view.state = .active
        return view
    }
    func updateNSView(_ view: NSVisualEffectView, context: Context) {}
}
