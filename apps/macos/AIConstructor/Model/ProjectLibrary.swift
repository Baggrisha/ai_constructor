import AppKit
import CoreServices
import SwiftUI


@MainActor
final class WeakWorkspace {
    weak var value: WorkspaceModel?
    init(_ value: WorkspaceModel) { self.value = value }
}

@MainActor
final class ProjectLibrary: ObservableObject {
    static let shared = ProjectLibrary()
    @Published private(set) var recents: [RecentProject] = []
    @Published private(set) var panels: [String: WorkspacePanel] = [:]
    @Published private(set) var retentionLimit: UInt32?
    @Published private(set) var panelWidths: PanelWidths = .standard
    @Published var error: String?
    private var recentsReadable = true
    private var panelsReadable = true
    private var panelWidthsReadable = true
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
        do { panelWidths = try read("panel-widths.json", default: .standard) }
        catch { panelWidthsReadable = false; self.error = "Не удалось прочитать ширины панелей; файл не будет перезаписан: \(error.localizedDescription)" }
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

    /// Persisted widths of the side panels (C.2).
    func setPanelWidth(sidebar: CGFloat? = nil, inspector: CGFloat? = nil) {
        guard panelWidthsReadable else { return }
        var next = panelWidths
        if let sidebar { next.sidebar = min(max(sidebar, PanelWidths.minSidebar), PanelWidths.maxSidebar) }
        if let inspector { next.inspector = min(max(inspector, PanelWidths.minInspector), PanelWidths.maxInspector) }
        guard next != panelWidths else { return }
        do { try write(next, to: "panel-widths.json"); panelWidths = next }
        catch { self.error = "Не удалось сохранить ширины панелей: \(error.localizedDescription)" }
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

    func firstWorkspace() -> WorkspaceModel? {
        workspaces.compactMap(\.value).first
    }

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

struct RetentionSettingsView: View {
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

func decodeJSON<T: Decodable>(_ type: T.Type, _ text: String) throws -> T {
    try JSONDecoder().decode(type, from: Data(text.utf8))
}

func readableError(_ error: Error) -> String {
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

func wasCanceled(_ error: Error) -> Bool {
    guard let coreError = error as? CoreError, case let .Failure(code, _) = coreError else { return false }
    return code == "CANCELED"
}
