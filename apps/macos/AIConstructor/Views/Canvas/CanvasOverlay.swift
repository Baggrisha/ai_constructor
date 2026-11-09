import SwiftUI

/// Maps document coordinates to the canvas view and back. Guides are the
/// edges of neighbouring nodes the dragged selection lines up with (C.4);
/// they are shown while dragging but never snap the result.
struct CanvasGeometry {
    let scale: CGFloat
    let root: CGSize

    func screen(_ rect: CanvasRect) -> CGRect {
        CGRect(x: rect.x * scale, y: rect.y * scale, width: rect.w * scale, height: rect.h * scale)
    }

    func document(_ point: CGPoint) -> CGPoint {
        CGPoint(x: point.x / scale, y: point.y / scale)
    }

    /// The page fills the available width; the canvas view itself scrolls
    /// vertically when the page is taller than the viewport.
    static func scale(viewSize: CGSize, root: CGSize) -> CGFloat {
        guard viewSize.width > 0, root.width > 0 else { return 0 }
        return viewSize.width / root.width
    }
}

struct CanvasGuides {
    static let threshold: Double = 1

    /// Edges of `moving` that coincide with `others` within one point, in
    /// document coordinates, as horizontal and vertical lines (x or y).
    static func lines(moving: CanvasRect, others: [CanvasNodeBounds]) -> (vertical: [Double], horizontal: [Double]) {
        let xs = [moving.x, moving.x + moving.w / 2, moving.x + moving.w]
        let ys = [moving.y, moving.y + moving.h / 2, moving.y + moving.h]
        var vertical: Set<Double> = []
        var horizontal: Set<Double> = []
        for other in others where !other.hidden {
            let otherXs = [other.x, other.x + other.w / 2, other.x + other.w]
            let otherYs = [other.y, other.y + other.h / 2, other.y + other.h]
            for x in xs where otherXs.contains(where: { abs($0 - x) < threshold }) {
                vertical.insert(x.rounded())
            }
            for y in ys where otherYs.contains(where: { abs($0 - y) < threshold }) {
                horizontal.insert(y.rounded())
            }
        }
        return (Array(vertical), Array(horizontal))
    }
}

struct CanvasOverlay: View {
    let content: CanvasContent
    let geometry: CanvasGeometry
    let size: CGSize
    let selection: Set<String>
    let hovered: String?
    let preview: CanvasRect?
    let rotation: Double?
    let levels: [String]
    let dragging: Bool
    let onPress: (CGPoint, CGPoint, NSEvent.ModifierFlags) -> Void
    let onDrag: (CanvasGestureKind, CGPoint) -> Void
    let onEnd: (CanvasGestureKind, CGPoint) -> Void
    let onPlainHover: (String?) -> Void
    let onDeviceHover: (CGPoint) -> Void

    private static let handleSize: CGFloat = 7

    private func rect(_ bounds: CanvasNodeBounds) -> CGRect {
        geometry.screen(CanvasRect(x: bounds.x, y: bounds.y, w: bounds.w, h: bounds.h))
    }

    private var highlighted: [CanvasNodeBounds] {
        guard !dragging, let hovered, !selection.contains(hovered) else { return [] }
        return content.nodes.filter { $0.id == hovered && !$0.hidden }
    }

    private var selected: [CanvasNodeBounds] {
        content.nodes.filter { selection.contains($0.id) && !$0.hidden }
    }

    var body: some View {
        ZStack(alignment: .topLeading) {
            Color.white.opacity(0.001)
                .frame(width: size.width, height: size.height)
                .contentShape(Rectangle())
                .modifier(MouseGesture(onPress: onPress, onDrag: onDrag, onEnd: onEnd, onHover: onPlainHover))
                DeviceHoverCatcher(onMove: onDeviceHover)
            if geometry.scale >= 0.5 {
                ForEach(highlighted) { node in
                    if let frame = rotated(rect(node), degrees: node.rotation) {
                        RoundedRectangle(cornerRadius: 1)
                            .stroke(Color.accentColor.opacity(0.9), lineWidth: 1)
                            .frame(width: frame.width, height: frame.height)
                            .rotationEffect(.degrees(node.rotation))
                            .position(x: frame.midX, y: frame.midY)
                            .allowsHitTesting(false)
                    }
                }
                ForEach(Array(selected.enumerated()), id: \.element.id) { index, node in
                    let outer = outerFrame(for: node)
                    RoundedRectangle(cornerRadius: 1)
                        .stroke(Color.accentColor, lineWidth: 1)
                        .frame(width: outer.width, height: outer.height)
                        .position(x: outer.midX, y: outer.midY)
                        .allowsHitTesting(false)
                    Text(levels.indices.contains(index) ? levels[index] : "")
                        .font(.system(size: 10, weight: .medium, design: .rounded))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 4).padding(.vertical, 1)
                        .background(Color.accentColor)
                        .position(x: max(outer.minX + 10, 12), y: max(outer.minY - 9, 8))
                        .allowsHitTesting(false)
                }
                ForEach(selected, id: \.id) { node in
                    ForEach(Array(handlePoints(rect(node)).enumerated()), id: \.offset) { _, point in
                        Rectangle().fill(Color.white)
                            .overlay(Rectangle().stroke(Color.accentColor, lineWidth: 1))
                            .frame(width: Self.handleSize, height: Self.handleSize)
                            .position(x: point.x, y: point.y)
                            .contentShape(Rectangle().size(width: 14, height: 14))
                            .modifier(MouseGesture(kind: .resize, onPress: onPress, onDrag: onDrag, onEnd: onEnd, onHover: onPlainHover))
                    }
                }
                if selected.count == 1, let node = selected.first {
                    let knob = rotateKnob(rect(node))
                    Circle().fill(Color.white)
                        .overlay(Circle().stroke(Color.accentColor, lineWidth: 1))
                        .frame(width: Self.handleSize + 1, height: Self.handleSize + 1)
                        .position(x: knob.x, y: knob.y)
                        .contentShape(Rectangle().size(width: 14, height: 14))
                        .modifier(MouseGesture(kind: .rotate, onPress: onPress, onDrag: onDrag, onEnd: onEnd, onHover: onPlainHover))
                }
            }
            if let preview, dragging, !selected.isEmpty {
                let frame = geometry.screen(preview)
                if rotation == nil {
                    let lines = CanvasGuides.lines(moving: preview, others: content.nodes.filter { !selection.contains($0.id) })
                    CanvasGuideLines(lines: lines, geometry: geometry)
                }
                RoundedRectangle(cornerRadius: 1)
                    .stroke(Color.accentColor.opacity(0.8), style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
                    .frame(width: frame.width, height: frame.height)
                    .position(x: frame.midX, y: frame.midY)
            }
        }
    }

    /// Rotated selection frames are drawn as their bounding square plus a
    /// rotation bar, so the marker stays visible without rotating hit testing.
    private func outerFrame(for node: CanvasNodeBounds) -> CGRect {
        rect(node).insetBy(dx: -0.5, dy: -0.5)
    }

    private func handlePoints(_ frame: CGRect) -> [CGPoint] {
        [CGPoint(x: frame.minX, y: frame.maxY), CGPoint(x: frame.midX, y: frame.maxY), CGPoint(x: frame.maxX, y: frame.maxY),
         CGPoint(x: frame.minX, y: frame.midY), CGPoint(x: frame.maxX, y: frame.midY),
         CGPoint(x: frame.minX, y: frame.minY), CGPoint(x: frame.midX, y: frame.minY), CGPoint(x: frame.maxX, y: frame.minY)]
    }

    private func rotateKnob(_ frame: CGRect) -> CGPoint {
        CGPoint(x: frame.midX, y: frame.maxY + 16)
    }

    /// Bounding square of a rotated frame. A frame rotated by whole quadrants
    /// keeps its own size; other angles fit their rotated corners.
    private func rotated(_ frame: CGRect, degrees: Double) -> CGRect? {
        let angle = degrees.truncatingRemainder(dividingBy: 360)
        if angle == 0 { return frame }
        let radians = angle * .pi / 180
        let c = abs(cos(radians))
        let s = abs(sin(radians))
        let width = frame.width * c + frame.height * s
        let height = frame.width * s + frame.height * c
        return CGRect(x: frame.midX - width / 2, y: frame.midY - height / 2, width: width, height: height)
    }
}

struct CanvasGuideLines: View {
    let lines: (vertical: [Double], horizontal: [Double])
    let geometry: CanvasGeometry

    var body: some View {
        if lines.vertical.isEmpty && lines.horizontal.isEmpty {
            EmptyView()
        } else {
            linesView
        }
    }

    private var linesView: some View {
        Canvas { context, _ in
            for x in lines.vertical {
                let px = x * geometry.scale
                var path = Path()
                path.move(to: CGPoint(x: px, y: 0))
                path.addLine(to: CGPoint(x: px, y: geometry.root.height * geometry.scale))
                context.stroke(path, with: .color(.pink.opacity(0.8)), lineWidth: 1)
            }
            for y in lines.horizontal {
                let py = y * geometry.scale
                var path = Path()
                path.move(to: CGPoint(x: 0, y: py))
                path.addLine(to: CGPoint(x: geometry.root.width * geometry.scale, y: py))
                context.stroke(path, with: .color(.pink.opacity(0.8)), lineWidth: 1)
            }
        }
        .allowsHitTesting(false)
    }
}

/// Node-level hover from the page bridge would need a transparent overlay
/// above it, which would also take clicks; hovering is resolved from device
/// coordinates against the computed bounds instead.
struct DeviceHoverCatcher: NSViewRepresentable {
    let onMove: (CGPoint) -> Void

    func makeNSView(context: Context) -> HoverView { HoverView(onMove: onMove) }

    func updateNSView(_ view: HoverView, context: Context) { view.onMove = onMove }

    final class HoverView: NSView {
        var onMove: ((CGPoint) -> Void)?
        private var monitor: Any?
        init(onMove: @escaping (CGPoint) -> Void) {
            self.onMove = onMove
            super.init(frame: .zero)
        }
        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
        deinit {
            if let monitor { NSEvent.removeMonitor(monitor) }
        }
        override var isFlipped: Bool { true }
        override func viewDidMoveToWindow() {
            if let monitor {
                NSEvent.removeMonitor(monitor)
                self.monitor = nil
            }
            guard window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: [.mouseMoved, .leftMouseDragged, .rightMouseDragged, .otherMouseDragged]) { [weak self] event in
                guard let self, event.window === self.window else { return event }
                self.onMove?(self.convert(event.locationInWindow, from: nil))
                return event
            }
        }
    }
}

/// Press, drag and release without SwiftUI drag sequencing, so a gesture that
/// starts on a handle and ends elsewhere still reports its own end. Point and
/// window coordinates are SwiftUI local (top-left origin) and window space.
struct MouseGesture: ViewModifier {
    var kind: CanvasGestureKind = .move
    let onPress: @MainActor (CGPoint, CGPoint, NSEvent.ModifierFlags) -> Void
    let onDrag: @MainActor (CanvasGestureKind, CGPoint) -> Void
    let onEnd: @MainActor (CanvasGestureKind, CGPoint) -> Void
    let onHover: @MainActor (String?) -> Void

    func body(content: Content) -> some View {
        content.background(MouseEventCatcher(kind: kind, onPress: onPress, onDrag: onDrag, onEnd: onEnd, onHover: onHover))
    }
}

struct MouseEventCatcher: NSViewRepresentable {
    let kind: CanvasGestureKind
    let onPress: (CGPoint, CGPoint, NSEvent.ModifierFlags) -> Void
    let onDrag: (CanvasGestureKind, CGPoint) -> Void
    let onEnd: (CanvasGestureKind, CGPoint) -> Void
    let onHover: (String?) -> Void

    func makeNSView(context: Context) -> MouseCatcherView {
        let view = MouseCatcherView()
        view.configure(kind: kind, onPress: onPress, onDrag: onDrag, onEnd: onEnd, onHover: onHover)
        return view
    }

    func updateNSView(_ view: MouseCatcherView, context: Context) {
        view.configure(kind: kind, onPress: onPress, onDrag: onDrag, onEnd: onEnd, onHover: onHover)
    }
}

final class MouseCatcherView: NSView {
    private(set) var kind: CanvasGestureKind = .move
    var onPress: ((CGPoint, CGPoint, NSEvent.ModifierFlags) -> Void)?
    var onDrag: ((CanvasGestureKind, CGPoint) -> Void)?
    var onEnd: ((CanvasGestureKind, CGPoint) -> Void)?
    var onHover: ((String?) -> Void)?

    private var pressed = false
    private var dragging = false
    private var start: CGPoint = .zero
    private static let threshold: CGFloat = 3

    override var isFlipped: Bool { true }

    func configure(kind: CanvasGestureKind,
                   onPress: @escaping @MainActor (CGPoint, CGPoint, NSEvent.ModifierFlags) -> Void,
                   onDrag: @escaping @MainActor (CanvasGestureKind, CGPoint) -> Void,
                   onEnd: @escaping @MainActor (CanvasGestureKind, CGPoint) -> Void,
                   onHover: @escaping @MainActor (String?) -> Void) {
        self.kind = kind
        self.onPress = onPress
        self.onDrag = onDrag
        self.onEnd = onEnd
        self.onHover = onHover
    }

    private func point(_ event: NSEvent) -> CGPoint {
        convert(event.locationInWindow, from: nil)
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseDown(with event: NSEvent) {
        pressed = true
        dragging = false
        start = point(event)
        onPress?(start, event.locationInWindow, event.modifierFlags)
    }

    override func mouseDragged(with event: NSEvent) {
        guard pressed else { return }
        let current = point(event)
        if !dragging, hypot(current.x - start.x, current.y - start.y) < Self.threshold { return }
        dragging = true
        onDrag?(kind, current)
    }

    override func mouseUp(with event: NSEvent) {
        guard pressed else { return }
        pressed = false
        if dragging { onEnd?(kind, point(event)) }
        dragging = false
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .arrow)
    }
}
