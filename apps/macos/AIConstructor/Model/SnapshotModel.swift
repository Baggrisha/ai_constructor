import AppKit
import CoreServices
import SwiftUI


final class ProjectFileWatcher {
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

struct Snapshot: Decodable {
    struct Document: Decodable {
        struct Page: Decodable {
            let id: String
            let rootId: String
        }
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

struct RecentProject: Codable, Identifiable {
    var id: String { path }
    var path: String
    var name: String
    var projectId: String?
    var archived: Bool?
}

struct Checkpoint: Decodable, Identifiable {
    let id: String
    let revision: UInt64
}

struct Chat: Decodable, Identifiable {
    let id: String
    let title: String
    let mode: String
    let archived: Bool
}

struct ChatMessage: Decodable, Identifiable {
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

struct ChatSearchHit: Decodable, Identifiable {
    let chat: Chat
    let message: ChatMessage
    var id: String { "\(chat.id):\(message.id)" }
}

struct ProjectDiagnostics: Decodable {
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

struct PanelWidths: Codable, Equatable {
    var sidebar: Double
    var inspector: Double

    static let standard = PanelWidths(sidebar: 260, inspector: 320)
    static let minSidebar: Double = 200
    static let maxSidebar: Double = 420
    static let minInspector: Double = 260
    static let maxInspector: Double = 480

    private enum CodingKeys: String, CodingKey { case schemaVersion, sidebar, inspector }

    init(sidebar: Double, inspector: Double) {
        self.sidebar = sidebar
        self.inspector = inspector
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        let standard = PanelWidths.standard
        sidebar = min(max((try? values.decode(Double.self, forKey: .sidebar)) ?? standard.sidebar, PanelWidths.minSidebar), PanelWidths.maxSidebar)
        inspector = min(max((try? values.decode(Double.self, forKey: .inspector)) ?? standard.inspector, PanelWidths.minInspector), PanelWidths.maxInspector)
    }

    func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(1, forKey: .schemaVersion)
        try values.encode(sidebar, forKey: .sidebar)
        try values.encode(inspector, forKey: .inspector)
    }
}

enum WorkspacePanel: String, CaseIterable, Codable {
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

struct RetentionSettings: Codable {
    let schemaVersion: Int
    let limit: UInt32?
    let riskAccepted: Bool
}

struct ProjectIdentity: Decodable {
    let projectId: String
}

struct CanvasPage: Equatable {
    let id: String
    let rootId: String
}

struct CanvasNodeBounds: Decodable, Identifiable {
    let id: String
    let x: Double
    let y: Double
    let w: Double
    let h: Double
    let hidden: Bool
    let kind: String
    let inFlow: Bool
    let parentId: String?
    let rotation: Double

    private enum CodingKeys: String, CodingKey { case id, x, y, w, h, hidden, kind, inFlow, parentId, rotation }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        id = try values.decode(String.self, forKey: .id)
        x = try values.decode(Double.self, forKey: .x)
        y = try values.decode(Double.self, forKey: .y)
        w = try values.decode(Double.self, forKey: .w)
        h = try values.decode(Double.self, forKey: .h)
        hidden = (try? values.decode(Bool.self, forKey: .hidden)) ?? false
        kind = (try? values.decode(String.self, forKey: .kind)) ?? ""
        inFlow = (try? values.decode(Bool.self, forKey: .inFlow)) ?? false
        parentId = try? values.decode(String.self, forKey: .parentId)
        rotation = (try? values.decode(Double.self, forKey: .rotation)) ?? 0
    }
}

struct CanvasContent {
    let html: String
    let nodes: [CanvasNodeBounds]
    var root: CanvasNodeBounds? { nodes.first }

    static let empty = CanvasContent(html: "", nodes: [])

    init(html: String, nodes: [CanvasNodeBounds]) {
        self.html = html
        self.nodes = nodes
    }

    /// Injects the canvas styles and the inspection channel ahead of the
    /// preview's own styles. The stored document HTML itself stays script-free.
    /// The page is painted at `scale` document points per pixel, so the native
    /// overlay and the WebView agree on one coordinate system.
    static func prepared(html: String, nodes: [CanvasNodeBounds], bridge: String, scale: Double) throws -> CanvasContent {
        guard let head = html.range(of: "</head>") else {
            throw NSError(domain: "AIConstructor", code: 1, userInfo: [NSLocalizedDescriptionKey: "Превью документа не содержит заголовка страницы."])
        }
        var styled = html
        styled.replaceSubrange(head, with: "<style>\(canvasCSS)\(css(scale: scale))</style><script>\(bridge)</script></head>")
        return CanvasContent(html: styled, nodes: nodes)
    }

    static let canvasCSS = "html,body{background:transparent!important;overflow:hidden!important;}#canvas{margin:0;}\n"

    static func css(scale: Double) -> String {
        let value = String(format: "%.6f", scale)
        return "#canvas{transform:scale(\(value));transform-origin:0 0;}\n"
    }

    func topmost(at point: CGPoint) -> CanvasNodeBounds? {
        nodes.last { node in
            !node.hidden && point.x >= node.x && point.y >= node.y && point.x < node.x + node.w && point.y < node.y + node.h
        }
    }

    var rootSize: CGSize { root.map { CGSize(width: $0.w, height: $0.h) } ?? .zero }
}

struct CanvasHit: Equatable {
    let id: String
    let shift: Bool
    let deep: Bool
}

enum CanvasGestureKind: String {
    case move, resize, rotate
}

struct CanvasRect: Equatable {
    var x: Double
    var y: Double
    var w: Double
    var h: Double
}

struct CanvasGesture {
    let kind: CanvasGestureKind
    let ids: [String]
    let start: CGPoint
    let bounds: [CanvasNodeBounds]
    let baseRevision: UInt64
    let baseFingerprint: String
    let inFlow: Bool
    let rotation: Double
}

struct CanvasTaskRequest {
    let session: ProjectSession
    let page: CanvasPage
    let scale: Double
}

@MainActor
final class CanvasTaskState {
    let request: CanvasTaskRequest
    var task: Task<Void, Never>?
    var queued = false
    init(request: CanvasTaskRequest) { self.request = request }
}

struct LoadedProject {
    let session: ProjectSession
    let snapshot: Snapshot
}

func loadProject(_ session: ProjectSession, at url: URL) throws -> LoadedProject {
    var snapshot = try decodeJSON(Snapshot.self, session.snapshotJson())
    if snapshot.document.projectId.isEmpty,
       let data = try? Data(contentsOf: url.appendingPathComponent("project.json")),
       let identity = try? JSONDecoder().decode(ProjectIdentity.self, from: data) {
        snapshot.document.projectId = identity.projectId
    }
    return LoadedProject(session: session, snapshot: snapshot)
}

struct SaveResult {
    let snapshot: Snapshot
    let error: String?
    let errorCode: String?
}

func editAndSaveSnapshot(_ session: ProjectSession, work: () throws -> String) throws -> SaveResult {
    do {
        return SaveResult(snapshot: try decodeJSON(Snapshot.self, work()), error: nil, errorCode: nil)
    } catch {
        let operationError = error
        let code: String?
        if let coreError = operationError as? CoreError, case let .Failure(errorCode, _) = coreError {
            code = errorCode
        } else {
            code = nil
        }
        do {
            return SaveResult(snapshot: try decodeJSON(Snapshot.self, session.snapshotJson()), error: readableError(operationError), errorCode: code)
        } catch {
            throw NSError(domain: "AIConstructor", code: 1, userInfo: [
                NSLocalizedDescriptionKey: "\(readableError(operationError))\nНе удалось получить состояние проекта: \(readableError(error))"
            ])
        }
    }
}

func saveSnapshot(_ session: ProjectSession, control: OperationControl) throws -> SaveResult {
    try editAndSaveSnapshot(session) { try session.saveAndSnapshotControlled(control: control) }
}

struct FrameDraft: Equatable {
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

struct DeferredFrameDraft {
    let draft: FrameDraft
    let base: FrameDraft
    let rootId: String
}
