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
        Settings {
            RetentionSettingsView()
        }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
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
