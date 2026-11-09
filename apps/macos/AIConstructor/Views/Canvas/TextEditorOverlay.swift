import AppKit
import SwiftUI

/// Text editing on the canvas (C.5): a native field in place of the node.
/// Enter commits, Shift+Enter breaks the line, Esc cancels, clicking outside
/// commits. The field's font mirrors the node so typing wraps where the page
/// wraps the committed text.
struct TextEditorOverlay: View {
    let model: WorkspaceModel
    let content: CanvasContent
    let scale: Double
    let editingId: String

    var body: some View {
        if let node = content.nodes.first(where: { $0.id == editingId }), let raw = model.rawNode(editingId) {
            let frame = CGRect(x: node.x * scale, y: node.y * scale,
                               width: max(node.w * scale, 24), height: max(node.h * scale, 24))
            let placeholder = InspectorJSON.value(in: raw, path: "/content/placeholder") != nil
            let text = InspectorJSON.value(in: raw, path: placeholder ? "/content/placeholder" : "/content/text") as? String ?? ""
            OverlayTextView(text: text,
                            style: overlaidStyle(raw, scale: scale),
                            onSubmit: { value in
                                model.editProperty(placeholder ? "/content/placeholder" : "/content/text",
                                                   value: value.isEmpty ? nil : value)
                                model.commitDraft()
                                model.endTextEditing()
                            },
                            onCancel: {
                                model.discardDraft()
                                model.endTextEditing()
                            })
            .frame(width: frame.width, height: frame.height)
            .position(x: frame.midX, y: frame.midY)
            .accessibilityIdentifier("canvas-text-editor")
        }
    }

    private func overlaidStyle(_ node: [String: Any], scale: Double) -> OverlayTextStyle {
        func number(_ path: String) -> Double? {
            (InspectorJSON.value(in: node, path: path) as? NSNumber)?.doubleValue
        }
        let family = InspectorJSON.value(in: node, path: "/style/fontFamily") as? String
        let size = (number("/style/fontSize") ?? 16) * scale
        let weight = number("/style/fontWeight") ?? 400
        let align = (InspectorJSON.value(in: node, path: "/style/textAlign") as? String) ?? "start"
        let lineHeight = number("/style/lineHeight").map { $0 * scale } ?? size * 1.2
        let color = (InspectorJSON.value(in: node, path: "/style/color") as? String)
            .flatMap { NSColor(rassvetHex: $0) } ?? .labelColor
        return OverlayTextStyle(size: size, weight: weight, family: family ?? "Helvetica Neue",
                                alignment: OverlayTextStyle.alignment(align), lineHeight: lineHeight, color: color)
    }
}

struct OverlayTextStyle {
    let size: CGFloat
    let weight: Double
    let family: String
    let alignment: NSTextAlignment
    let lineHeight: CGFloat
    let color: NSColor

    static func alignment(_ value: String) -> NSTextAlignment {
        switch value {
        case "center": return .center
        case "end": return .right
        case "justify": return .justified
        default: return .left
        }
    }

    var font: NSFont {
        let system = NSFont.systemFont(ofSize: size, weight: NSFont.Weight(rawWeight: weight))
        return NSFont(name: family, size: size) ?? system
    }

    var paragraph: NSParagraphStyle {
        let style = NSMutableParagraphStyle()
        style.alignment = alignment
        let natural = font.ascender - font.descender + font.leading
        style.minimumLineHeight = lineHeight
        style.maximumLineHeight = max(lineHeight, natural)
        style.lineBreakMode = .byWordWrapping
        return style
    }
}

private extension NSFont.Weight {
    init(rawWeight: Double) {
        switch rawWeight {
        case ..<300: self = .light
        case ..<450: self = .regular
        case ..<600: self = .medium
        case ..<700: self = .semibold
        case ..<800: self = .bold
        default: self = .heavy
        }
    }
}

private struct OverlayTextView: NSViewRepresentable {
    let text: String
    let style: OverlayTextStyle
    let onSubmit: (String) -> Void
    let onCancel: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(parent: self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.hasVerticalScroller = false
        scroll.autohidesScrollers = true
        let view = ShiftEnterTextView()
        view.isRichText = false
        view.drawsBackground = true
        view.backgroundColor = NSColor.textBackgroundColor.withAlphaComponent(0.95)
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.textContainerInset = .zero
        view.textContainer?.lineFragmentPadding = 0
        view.textContainer?.widthTracksTextView = true
        view.delegate = context.coordinator
        view.onShiftEnter = { [weak coordinator = context.coordinator] in coordinator?.onShiftEnter() }
        view.onCommand = { [weak coordinator = context.coordinator] key in coordinator?.handle(key) }
        view.apply(text: text, style: style)
        scroll.documentView = view
        DispatchQueue.main.async {
            view.window?.makeFirstResponder(view)
            view.selectAll(nil)
        }
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.parent = self
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: OverlayTextView
        init(parent: OverlayTextView) { self.parent = parent }

        func textDidChange(_ notification: Notification) {
            guard let view = notification.object as? ShiftEnterTextView else { return }
            view.apply(text: view.string, style: parent.style)
        }

        func onShiftEnter() {
            guard let view = NSApp.keyWindow?.firstResponder as? NSTextView else { return }
            view.insertText("\n", replacementRange: view.selectedRange())
        }

        func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            if selector == #selector(NSResponder.insertNewline(_:)) {
                parent.onSubmit(textView.string)
                return true
            }
            if selector == #selector(NSResponder.cancelOperation(_:)) {
                parent.onCancel()
                return true
            }
            return false
        }

        func insertLineBreak() {
            guard let view = NSApp.keyWindow?.firstResponder as? NSTextView else { return }
            view.insertText("\n", replacementRange: view.selectedRange())
        }

        func handle(_ key: String) {
            if key == "esc" { parent.onCancel() }
        }
    }
}

/// NSTextView variant that reports Shift+Enter as a line break and Esc as a
/// cancel, before AppKit's text commands see them.
private final class ShiftEnterTextView: NSTextView {
    var onShiftEnter: (() -> Void)?
    var onCommand: ((String) -> Void)?

    func apply(text: String, style: OverlayTextStyle) {
        if string != text { string = text }
        typingAttributes = [
            .font: style.font,
            .foregroundColor: style.color,
            .paragraphStyle: style.paragraph
        ]
        font = style.font
        textColor = style.color
        defaultParagraphStyle = style.paragraph
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 36 && event.modifierFlags.contains(.shift) {
            onShiftEnter?()
            return
        }
        if event.keyCode == 53 {
            onCommand?("esc")
            return
        }
        super.keyDown(with: event)
    }
}
