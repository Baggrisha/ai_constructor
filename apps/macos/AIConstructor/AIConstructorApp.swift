import AppKit
import SwiftUI

// Use SwiftUI’s property wrapper when the CLT does not include SwiftUIMacros.
private typealias StoredState<Value> = SwiftUI.State<Value>

@main
struct AIConstructorApp: App {
    var body: some Scene {
        WindowGroup("AI Constructor") {
            WorkspaceView()
                .frame(minWidth: 900, minHeight: 620)
        }
        .windowStyle(.hiddenTitleBar)
    }
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
        let projectId: String
        let revision: UInt64
        let pages: [Page]
        let nodes: [Node]

        private enum CodingKeys: String, CodingKey { case projectId, revision, pages, nodes }
        init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            projectId = (try? values.decode(String.self, forKey: .projectId)) ?? ""
            revision = (try? values.decode(UInt64.self, forKey: .revision)) ?? 0
            pages = (try? values.decode([Page].self, forKey: .pages)) ?? []
            nodes = (try? values.decode([Node].self, forKey: .nodes)) ?? []
        }
    }
    let document: Document
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

@MainActor
private final class WorkspaceModel: ObservableObject {
    @Published var snapshot: Snapshot?
    @Published var recents: [RecentProject] = []
    @Published var checkpoints: [Checkpoint] = []
    @Published var error: String?
    @Published var search = ""
    private var session: ProjectSession?
    private var currentPath: URL?

    private var recentsURL: URL {
        let base = ProcessInfo.processInfo.environment["AIC_APPLICATION_SUPPORT"].map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("AIConstructor", isDirectory: true)
            .appendingPathComponent("recent-projects.json")
    }

    init() {
        do {
            let data = try Data(contentsOf: recentsURL)
            recents = try JSONDecoder().decode([RecentProject].self, from: data)
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            recents = []
        } catch {
            self.error = "Не удалось прочитать список проектов: \(error.localizedDescription)"
        }
    }

    private func saveRecents() throws {
        try FileManager.default.createDirectory(at: recentsURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(recents).write(to: recentsURL, options: .atomic)
    }

    private func remember(_ path: URL) throws {
        guard let snapshot else { return }
        recents.removeAll { $0.path == path.path }
        recents.insert(RecentProject(path: path.path, name: snapshot.name, projectId: snapshot.document.projectId, archived: false), at: 0)
        try saveRecents()
    }

    private func update(_ text: String) throws {
        snapshot = try JSONDecoder().decode(Snapshot.self, from: Data(text.utf8))
    }

    private func closeCurrent() throws {
        if snapshot?.dirty == true { try session?.save() }
    }

    func create() {
        do {
            try closeCurrent()
            let defaultFolder = ProcessInfo.processInfo.environment["AIC_PROJECTS_DIRECTORY"].map { URL(fileURLWithPath: $0, isDirectory: true) }
                ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
                    .appendingPathComponent("AI Constructor/Projects", isDirectory: true)
            try FileManager.default.createDirectory(at: defaultFolder, withIntermediateDirectories: true)
            let panel = NSSavePanel()
            panel.title = "Новый проект"
            panel.message = "Выберите имя и расположение папки проекта."
            panel.directoryURL = defaultFolder
            panel.nameFieldStringValue = "Новый проект"
            panel.canCreateDirectories = true
            guard panel.runModal() == .OK, let url = panel.url else { return }
            let next = try createProject(path: url.path, name: url.lastPathComponent)
            let nextSnapshot = try next.snapshotJson()
            session = next
            currentPath = url
            try update(nextSnapshot)
            try refreshCheckpoints()
            try remember(url)
        } catch { self.error = error.localizedDescription }
    }

    func chooseProject() {
        let panel = NSOpenPanel()
        panel.title = "Открыть папку проекта"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        open(url)
    }

    func open(_ url: URL) {
        do {
            try closeCurrent()
            let next = try openProject(path: url.path)
            let nextSnapshot = try next.snapshotJson()
            session = next
            currentPath = url
            try update(nextSnapshot)
            try refreshCheckpoints()
            try remember(url)
        } catch { self.error = error.localizedDescription }
    }

    func locate(_ project: RecentProject) {
        let panel = NSOpenPanel()
        panel.title = "Указать расположение проекта"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            try closeCurrent()
            let next = try openProject(path: url.path)
            let text = try next.snapshotJson()
            let candidate = try JSONDecoder().decode(Snapshot.self, from: Data(text.utf8))
            guard let expected = project.projectId, expected == candidate.document.projectId else {
                throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Идентификатор проекта не совпадает; исходная запись сохранена."])
            }
            session = next
            currentPath = url
            try update(text)
            try refreshCheckpoints()
            recents.removeAll { $0.path == project.path }
            try remember(url)
        } catch { self.error = error.localizedDescription }
    }

    func setArchived(_ archived: Bool, project: RecentProject) {
        do {
            if archived && currentPath?.path == project.path { try closeCurrent() }
            guard let index = recents.firstIndex(where: { $0.path == project.path }) else { return }
            let previous = recents
            recents[index].archived = archived
            do { try saveRecents() } catch { recents = previous; throw error }
            if archived && currentPath?.path == project.path {
                session = nil; snapshot = nil; currentPath = nil; checkpoints = []
            }
        } catch { self.error = error.localizedDescription }
    }

    func archiveCurrent() {
        guard let currentPath, let project = recents.first(where: { $0.path == currentPath.path }) else { return }
        setArchived(true, project: project)
    }

    func removeRecent(_ project: RecentProject) {
        do {
            recents.removeAll { $0.id == project.id }
            try saveRecents()
        } catch { self.error = error.localizedDescription }
    }

    func apply(name: String, width: Double, height: Double) {
        guard let session, let snapshot, let root = snapshot.root else { return }
        let transaction: [String: Any] = [
            "transactionId": UUID().uuidString.lowercased(),
            "baseRevision": snapshot.document.revision,
            "baseFingerprint": snapshot.fingerprint,
            "origin": "user",
            "scopeNodeIds": [root.id],
            "operations": [
                ["type": "setProperty", "nodeId": root.id, "path": "/name", "value": name],
                ["type": "setViewport", "nodeId": root.id, "width": width, "height": height]
            ]
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: transaction, options: [.sortedKeys])
            guard let json = String(data: data, encoding: .utf8) else { throw CocoaError(.fileWriteInapplicableStringEncoding) }
            try update(session.applyTransaction(transactionJson: json))
            save()
        } catch { self.error = error.localizedDescription }
    }

    func save() {
        do {
            try session?.save()
            if let session { try update(session.snapshotJson()) }
        } catch {
            self.error = "Не сохранено: \(error.localizedDescription)\nИзменения остаются в памяти. Не закрывайте окно, пока не устраните причину и не сохраните проект."
        }
    }

    func undo() {
        do { if let session { try update(session.undo()); save() } }
        catch { self.error = error.localizedDescription }
    }

    func redo() {
        do { if let session { try update(session.redo()); save() } }
        catch { self.error = error.localizedDescription }
    }

    private func refreshCheckpoints() throws {
        guard let session, snapshot?.readOnly == false else { checkpoints = []; return }
        checkpoints = try JSONDecoder().decode([Checkpoint].self, from: Data(session.checkpointsJson().utf8))
    }

    func createCheckpoint() {
        do {
            guard let session else { return }
            _ = try session.createCheckpoint()
            try update(session.snapshotJson())
            try refreshCheckpoints()
        } catch { self.error = error.localizedDescription }
    }

    func restoreCheckpoint(_ checkpoint: Checkpoint) {
        do {
            guard let session else { return }
            try update(session.restoreCheckpoint(id: checkpoint.id))
            save()
        } catch { self.error = error.localizedDescription }
    }

    func duplicate() {
        do {
            guard let session, let snapshot else { return }
            let panel = NSSavePanel()
            panel.title = "Дублировать проект"
            panel.directoryURL = currentPath?.deletingLastPathComponent()
            panel.nameFieldStringValue = "\(snapshot.name) — копия"
            panel.canCreateDirectories = true
            guard panel.runModal() == .OK, let url = panel.url else { return }
            let next = try session.duplicateTo(destination: url.path, name: url.lastPathComponent)
            let nextSnapshot = try next.snapshotJson()
            self.session = next
            currentPath = url
            try update(nextSnapshot)
            try refreshCheckpoints()
            try remember(url)
        } catch { self.error = error.localizedDescription }
    }

    func rename(_ name: String) {
        do {
            guard let session else { return }
            try session.rename(name: name)
            try update(session.snapshotJson())
            save()
            if let currentPath { try remember(currentPath) }
        } catch { self.error = error.localizedDescription }
    }
}

private struct WorkspaceView: View {
    @StateObject private var model = WorkspaceModel()
    @StoredState private var renameProject = false
    @StoredState private var projectName = ""

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
                    ForEach(model.recents.filter { $0.archived != true && (model.search.isEmpty || $0.name.localizedCaseInsensitiveContains(model.search)) }) { project in
                        Button {
                            model.open(URL(fileURLWithPath: project.path))
                        } label: {
                            VStack(alignment: .leading) {
                                Text(project.name)
                                Text(project.path).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                        }
                        .buttonStyle(.plain)
                        .contextMenu {
                            Button("Указать расположение…") { model.locate(project) }
                            Button("В архив") { model.setArchived(true, project: project) }
                            Button("Убрать из списка") { model.removeRecent(project) }
                        }
                    }
                    let archived = model.recents.filter { $0.archived == true && (model.search.isEmpty || $0.name.localizedCaseInsensitiveContains(model.search)) }
                    if !archived.isEmpty {
                        Section("Архив") {
                            ForEach(archived) { project in
                                Button(project.name) { model.open(URL(fileURLWithPath: project.path)) }
                                    .contextMenu {
                                        Button("Вернуть из архива") { model.setArchived(false, project: project) }
                                        Button("Указать расположение…") { model.locate(project) }
                                        Button("Убрать из списка") { model.removeRecent(project) }
                                    }
                            }
                        }
                    }
                }
                .listStyle(.sidebar)
            }
            .padding(.horizontal, 16)
            .background(SidebarMaterial())
            .navigationSplitViewColumnWidth(min: 210, ideal: 226, max: 260)
        } detail: {
            if let snapshot = model.snapshot {
                VStack(spacing: 0) {
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(snapshot.name).font(.headline)
                            Text(snapshot.readOnly ? "Только чтение" : snapshot.dirty ? "Не сохранено" : "Сохранено")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Отменить", systemImage: "arrow.uturn.backward") { model.undo() }.disabled(snapshot.readOnly)
                        Button("Повторить", systemImage: "arrow.uturn.forward") { model.redo() }.disabled(snapshot.readOnly)
                        Button("Сохранить", systemImage: "square.and.arrow.down") { model.save() }.disabled(snapshot.readOnly)
                        Menu {
                            Button("Переименовать проект…") { projectName = snapshot.name; renameProject = true }
                                .disabled(snapshot.readOnly)
                            Button("В архив") { model.archiveCurrent() }
                            Button("Дублировать проект…") { model.duplicate() }
                                .disabled(snapshot.readOnly)
                            Button("Создать контрольную точку") { model.createCheckpoint() }
                                .disabled(snapshot.readOnly)
                            Menu("Восстановить контрольную точку") {
                                if model.checkpoints.isEmpty {
                                    Text("Нет контрольных точек")
                                } else {
                                    ForEach(model.checkpoints) { checkpoint in
                                        Button("Ревизия \(checkpoint.revision) · \(checkpoint.id.prefix(8))") {
                                            model.restoreCheckpoint(checkpoint)
                                        }
                                    }
                                }
                            }
                            .disabled(snapshot.readOnly || model.checkpoints.isEmpty)
                        } label: { Image(systemName: "ellipsis.circle") }
                    }
                    .padding(20)
                    Divider()
                    if let reason = snapshot.readOnlyReason {
                        Text(reason).frame(maxWidth: .infinity, alignment: .leading).padding().background(.orange.opacity(0.12))
                    }
                    if let root = snapshot.root {
                        FrameEditor(snapshot: snapshot, root: root, apply: model.apply)
                            .id("\(snapshot.document.projectId)-\(snapshot.document.revision)")
                    } else {
                        ContentUnavailableView("Нет корневого фрейма", systemImage: "exclamationmark.triangle")
                    }
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
                        .clipShape(Capsule())
                    Button("Открыть папку") { model.chooseProject() }
                }
            }
        }
        .alert("Ошибка", isPresented: Binding(get: { model.error != nil }, set: { if !$0 { model.error = nil } })) {
            Button("OK") { model.error = nil }
        } message: { Text(model.error ?? "") }
        .sheet(isPresented: $renameProject) {
            VStack(alignment: .leading, spacing: 16) {
                Text("Переименовать проект").font(.headline)
                TextField("Название", text: $projectName)
                HStack {
                    Spacer()
                    Button("Отмена") { renameProject = false }
                    Button("Сохранить") { model.rename(projectName); renameProject = false }
                        .keyboardShortcut(.defaultAction)
                }
            }.padding(24).frame(width: 360)
        }
    }
}

private struct FrameEditor: View {
    let snapshot: Snapshot
    let root: Snapshot.Document.Node
    let apply: (String, Double, Double) -> Void
    @StoredState private var name: String
    @StoredState private var width: Double
    @StoredState private var height: Double

    init(snapshot: Snapshot, root: Snapshot.Document.Node, apply: @escaping (String, Double, Double) -> Void) {
        self.snapshot = snapshot; self.root = root; self.apply = apply
        _name = State(initialValue: root.name)
        _width = State(initialValue: root.layout.w)
        _height = State(initialValue: root.layout.h)
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
                TextField("Название фрейма", text: $name).accessibilityIdentifier("frame-name")
                TextField("Ширина", value: $width, format: .number).accessibilityIdentifier("frame-width")
                TextField("Высота", value: $height, format: .number).accessibilityIdentifier("frame-height")
                Button("Применить") { apply(name, width, height) }
                    .accessibilityIdentifier("apply-frame")
                    .buttonStyle(.borderedProminent)
                    .clipShape(Capsule())
                    .disabled(snapshot.readOnly || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || width <= 0 || height <= 0)
            }
            .formStyle(.grouped)
            .frame(width: 270)
        }
        .padding(24)
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
