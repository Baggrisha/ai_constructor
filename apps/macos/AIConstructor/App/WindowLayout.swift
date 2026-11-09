import AppKit
import SwiftUI

/// C.2: the editor opens at 1440×900 and never grows past the visible screen.
enum WindowLayout {
    static let defaultWidth: CGFloat = 1440
    static let defaultHeight: CGFloat = 900
    static let minWidth: CGFloat = 960
    static let minHeight: CGFloat = 640

    @MainActor
    static func fitted(_ size: CGSize, in screen: NSScreen?) -> CGSize {
        guard let visible = screen?.visibleFrame.size else { return size }
        return CGSize(
            width: min(size.width, max(minWidth, visible.width)),
            height: min(size.height, max(minHeight, visible.height))
        )
    }
}

@MainActor
final class WindowSizingDelegate: NSObject, NSWindowDelegate {
    private weak var previous: NSWindowDelegate?
    private var sized: ObjectIdentifier?

    func attach(to window: NSWindow) {
        guard window.delegate !== self else { return }
        previous = window.delegate
        window.delegate = self
        guard sized != ObjectIdentifier(window) else { return }
        sized = ObjectIdentifier(window)
        let visible = window.screen?.visibleFrame
        var frame = window.frame
        let fitted = WindowLayout.fitted(frame.size, in: window.screen)
        let origin = NSPoint(
            x: visible.map { $0.midX - fitted.width / 2 } ?? frame.origin.x,
            y: visible.map { $0.midY - fitted.height / 2 } ?? frame.origin.y
        )
        frame = NSRect(origin: origin, size: fitted)
        window.setFrame(frame, display: true)
    }

    override func responds(to selector: Selector!) -> Bool {
        super.responds(to: selector) || (previous?.responds(to: selector) ?? false)
    }

    override func forwardingTarget(for selector: Selector!) -> Any? {
        if previous?.responds(to: selector) == true { return previous }
        return super.forwardingTarget(for: selector)
    }
}

struct WindowSizingView: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async {
            guard let window = view.window else { return }
            detached(context.coordinator).attach(to: window)
        }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        guard let window = view.window else { return }
        detached(context.coordinator).attach(to: window)
    }

    // The window's delegate is weakly held, so the coordinator keeps it alive.
    func makeCoordinator() -> WindowSizingDelegate { WindowSizingDelegate() }

    private func detached(_ delegate: WindowSizingDelegate) -> WindowSizingDelegate {
        delegate
    }
}
