import AppKit
import CoreServices
import SwiftUI


@main
struct AIConstructorApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        WindowGroup("AI Constructor") {
            WorkspaceView()
                .frame(minWidth: WindowLayout.minWidth, minHeight: WindowLayout.minHeight)
        }
        .defaultSize(width: WindowLayout.defaultWidth, height: WindowLayout.defaultHeight)
        .windowStyle(.hiddenTitleBar)
        .commands {
            CommandGroup(replacing: .undoRedo) {
                Button("Отменить") { WorkspaceCommands.shared.undo() }
                    .keyboardShortcut("z", modifiers: .command)
                Button("Повторить") { WorkspaceCommands.shared.redo() }
                    .keyboardShortcut("z", modifiers: [.command, .shift])
            }
            CommandGroup(replacing: .saveItem) {
                Button("Сохранить") { WorkspaceCommands.shared.save() }
                    .keyboardShortcut("s", modifiers: .command)
            }
            CommandGroup(replacing: .textEditing) {
                Button("Найти и заменить…") { WorkspaceCommands.shared.search = true }
                    .keyboardShortcut("f", modifiers: .command)
            }

            CommandMenu("Правка") {
                Button("Дублировать") { WorkspaceCommands.shared.duplicate() }
                    .keyboardShortcut("d", modifiers: .command)
                Button("Копировать") { WorkspaceCommands.shared.copy() }
                    .keyboardShortcut("c", modifiers: .command)
                Button("Вставить") { WorkspaceCommands.shared.paste() }
                    .keyboardShortcut("v", modifiers: .command)
                Button("Удалить") { WorkspaceCommands.shared.deleteSelection() }
                    .keyboardShortcut(.delete, modifiers: [])
                Button("Выделить всё") { WorkspaceCommands.shared.selectAll() }
                    .keyboardShortcut("a", modifiers: .command)
                Divider()
                Button("Группировать") { WorkspaceCommands.shared.group() }
                    .keyboardShortcut("g", modifiers: .command)
                Button("Разгруппировать") { WorkspaceCommands.shared.ungroup() }
                    .keyboardShortcut("g", modifiers: [.command, .shift])
                Divider()
                Button("Восстановить как текст…") { WorkspaceCommands.shared.restoreText() }
                    .disabled(WorkspaceCommands.shared.model?.canRestoreAsText != true)
            }
            CommandMenu("Вставка") {
                ForEach(WorkspaceCommands.insertKinds, id: \.self) { kind in
                    Button(WorkspaceCommands.title(kind)) { WorkspaceCommands.shared.insert(kind: kind) }
                }
            }
            CommandMenu("Слой") {
                Button("На передний план") { WorkspaceCommands.shared.bringToFront() }
                    .keyboardShortcut("]", modifiers: [.command, .shift])
                Button("Выше") { WorkspaceCommands.shared.bringForward() }
                    .keyboardShortcut("]", modifiers: .command)
                Button("Ниже") { WorkspaceCommands.shared.sendBackward() }
                    .keyboardShortcut("[", modifiers: .command)
                Button("На задний план") { WorkspaceCommands.shared.sendToBack() }
                    .keyboardShortcut("[", modifiers: [.command, .shift])
            }
            CommandMenu("Выравнивание") {
                ForEach(AlignmentEdge.allCases, id: \.self) { edge in
                    Button(edge.title) { WorkspaceCommands.shared.align(edge) }
                }
                Divider()
                ForEach(DistributionAxis.allCases, id: \.self) { axis in
                    Button(axis.title) { WorkspaceCommands.shared.distribute(axis) }
                }
            }
            CommandMenu("Компонент") {
                Button("Создать из выделения") { WorkspaceCommands.shared.createComponent() }
                Menu("Вставить экземпляр") {
                    ForEach(WorkspaceCommands.shared.model?.components ?? []) { component in
                        Button(component.name) { WorkspaceCommands.shared.insertComponent(component.id) }
                    }
                }
                Button("Разорвать связь") { WorkspaceCommands.shared.breakComponent() }
            }
        }
        Settings {
            RetentionSettingsView()
        }
    }
}

/// Menu commands reach the front window's workspace through this registry:
/// the model is owned by the view, the menus live outside it.
@MainActor
final class WorkspaceCommands {
    static let shared = WorkspaceCommands()
    static let insertKinds = InsertedNodeTemplate.kinds

    weak var model: WorkspaceModel?
    var search = false

    func undo() { model?.undo() }
    func redo() { model?.redo() }
    func save() { model?.save() }
    func insert(kind: String) { model?.insertNode(kind) }
    func duplicate() { model?.duplicateSelection() }
    func deleteSelection() { model?.deleteSelection() }
    func selectAll() { model?.selectAllNodes() }
    func copy() { model?.copySelection() }
    func paste() { model?.paste() }
    func restoreText() {
        guard let model, model.canRestoreAsText else { return }
        model.restoreTextTarget = model.inspectorNodeId
    }
    func group() { model?.groupSelection() }
    func ungroup() { model?.ungroupSelection() }
    func bringToFront() { model?.bringToFront() }
    func bringForward() { model?.bringForward() }
    func sendBackward() { model?.sendBackward() }
    func sendToBack() { model?.sendToBack() }
    func align(_ edge: AlignmentEdge) { model?.align(edge) }
    func distribute(_ axis: DistributionAxis) { model?.distribute(axis) }
    func createComponent() { model?.createComponentFromSelection() }
    func insertComponent(_ id: String) { model?.insertComponent(id) }
    func breakComponent() { model?.breakComponentLink() }

    static func title(_ kind: String) -> String {
        switch kind {
        case "frame": return "Фрейм"
        case "text": return "Текст"
        case "image": return "Изображение"
        case "vector": return "Вектор"
        case "button": return "Кнопка"
        case "input": return "Поле ввода"
        case "textarea": return "Многострочное поле"
        case "checkbox": return "Флажок"
        case "select": return "Список"
        default: return kind
        }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        // Test hooks duplicate macOS *scripts*: never enabled in a normal launch.
        let debug = UserDefaults.standard.bool(forKey: "AICTestHooks")
        let canvas = ProcessInfo.processInfo.environment["AIC_TEST_CANVAS"] == "1"
        guard debug || canvas else { return }
        // The window's workspace is registered on appear; test hooks wait for it.
        if let path = ProcessInfo.processInfo.environment["AIC_TEST_OPEN"], let url = URL(string: "file://" + path) {
            afterWorkspace { $0.openPath(url) }
        }
        if let path = ProcessInfo.processInfo.environment["AIC_TEST_CREATE"], let url = URL(string: "file://" + path) {
            afterWorkspace { $0.createAt(url) }
        }
        if canvas {
            afterWorkspace { $0.openFirstRecent() }
        }
    }

    @MainActor
    private func afterWorkspace(_ action: @escaping @MainActor (WorkspaceModel) -> Void) {
        Task { @MainActor in
            for _ in 0..<100 {
                if let workspace = ProjectLibrary.shared.firstWorkspace() {
                    action(workspace)
                    return
                }
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
        }
    }

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
final class WindowCloseDelegate: NSObject, NSWindowDelegate {
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
final class WindowCloseGuardView: NSView {
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

struct WindowCloseGuard: NSViewRepresentable {
    let model: WorkspaceModel
    func makeNSView(context: Context) -> WindowCloseGuardView {
        let view = WindowCloseGuardView()
        view.model = model
        return view
    }
    func updateNSView(_ view: WindowCloseGuardView, context: Context) { view.model = model }
}
