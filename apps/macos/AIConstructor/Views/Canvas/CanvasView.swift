import AppKit
import SwiftUI
import WebKit

/// The editing canvas (C.3): the Rust-rendered page in a WebKit view with a
/// native marker and gesture overlay above it. The pending inspector patches
/// are pushed into the loaded page when the list changes and after every
/// load, because a load starts from the core's HTML again.
struct CanvasView: View {
    @ObservedObject var model: WorkspaceModel
    let content: CanvasContent
    /// Live inspector input, applied to the loaded page without a reload.
    let patches: [CanvasPatch]
    @StoredState private var webViewBridge = CanvasWebViewBridge()

    init(model: WorkspaceModel, content: CanvasContent, patches: [CanvasPatch] = []) {
        self.model = model
        self.content = content
        self.patches = patches
    }

    var body: some View {
        GeometryReader { proxy in
            let scale = max(CanvasGeometry.scale(viewSize: proxy.size, root: content.rootSize), 0.0001)
            let height = content.nodes.isEmpty ? proxy.size.height : max(proxy.size.height, content.rootSize.height * scale + 24)
            ScrollView(.vertical) {
                contentBody(width: proxy.size.width, height: height, scale: scale)
            }
            .scrollDisabled(model.canvasEditing)
        }
        .accessibilityIdentifier("canvas")
    }

    private func contentBody(width: CGFloat, height: CGFloat, scale: CGFloat) -> some View {
        // The page is scaled to fill the available width, so the native
        // overlay and the WebView agree on one coordinate system.
        ZStack(alignment: .topLeading) {
            Color(nsColor: .underPageBackgroundColor)
            if content.nodes.isEmpty {
                placeholder
            } else {
                page(content, scale: scale)
                CanvasOverlay(content: content,
                              geometry: CanvasGeometry(scale: scale, root: content.rootSize),
                              size: CGSize(width: width, height: height),
                              selection: model.selectedNodeIds,
                              hovered: model.canvasHoveredId,
                              preview: model.canvasPreview,
                              rotation: model.canvasRotation,
                              levels: levels(),
                              dragging: model.canvasEditing,
                              onPress: { point, _, flags in
                                  if content.topmost(at: document(point, scale: scale)) == nil {
                                      model.beginMarquee(at: document(point, scale: scale), extending: flags.contains(.shift))
                                  } else {
                                      press(at: point, scale: scale)
                                  }
                              },
                              onDrag: { kind, point in model.updateCanvasGesture(kind, at: document(point, scale: scale)) },
                              onEnd: { kind, point in model.endCanvasGesture(kind, at: document(point, scale: scale)) },
                              onPlainHover: { model.canvasHoveredId = $0 },
                              onDeviceHover: { model.canvasHoveredId = content.topmost(at: document($0, scale: scale))?.id },
                              onDoubleClick: { point in
                                  guard let hit = content.topmost(at: document(point, scale: scale)) else { return }
                                  model.beginTextEditing(hit.id)
                              },
                              onMarquee: marqueeHandler(scale: scale),
                              marquee: model.marquee,
                              onContext: { point, _ in selectForMenu(point, scale: scale) },
                              onMenu: { _ in canvasMenu() },
                              vectorPoints: vectorPoints(scale: scale),
                              vectorDrag: onVectorDrag(scale: scale),
                              vectorNodeId: model.inspectorNodeId ?? "")
                if let editingId = model.textEditingId {
                    TextEditorOverlay(model: model, content: content, scale: scale, editingId: editingId)
                        .id(editingId)
                }
            }
            if model.canvasGenerating && !content.nodes.isEmpty {
                ProgressView().controlSize(.small).padding(10)
            }
            if let notice = model.canvasNotice {
                Text(notice)
                    .font(.caption)
                    .padding(8)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
                    .padding(12)
            }
        }
        .task(id: content.html.hashValue) {
            model.setCanvasScale(scale)
        }
        .task(id: model.snapshot?.document.revision) {
            model.refreshCanvas()
        }
        .onChange(of: patches) {
            CanvasWebView.patch(patches, in: webViewBridge.webView)
        }
        .onDisappear { webViewBridge.webView = nil }
        // The page is scaled to fill the available width, so the native overlay
        // and the WebView agree on one coordinate system; the stack is a fixed
        // frame because unpositioned children accept no click gestures.
        .frame(width: width, height: height, alignment: .topLeading)
    }

    private var placeholder: some View {
        ContentUnavailableView {
        Label(model.canvasGenerating ? "Строится холст" : "Холст пуст", systemImage: "rectangle.dashed")
        } description: {
        if let notice = model.canvasNotice { Text(notice) }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func page(_ content: CanvasContent, scale: Double) -> some View {
        let size = CGSize(width: content.rootSize.width * scale, height: content.rootSize.height * scale)
        return CanvasWebView(html: content.html, scale: scale, patches: patches,
                             onView: { webView in
                                 if webViewBridge.webView !== webView { webViewBridge.webView = webView }
                             }) { message in
            switch message {
            case .hover(let id):
                model.canvasHoveredId = id.flatMap { id in content.nodes.first { $0.id == id }?.id }
            case .down:
                // The overlay resolves the press against the same computed
                // bounds; the page only contributes hover.
                break
            }
        }
        .frame(width: size.width, height: size.height)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }

    /// Rubber-band selection runs only while it is the active gesture; the
    /// regular drag path is left alone for node moves and handles.
    private func marqueeHandler(scale: Double) -> (CGPoint, CGPoint) -> Void {
        { _, end in
            guard model.marqueeActive else { return }
            let mutating = NSEvent.pressedMouseButtons % 2 == 1
            if mutating {
                model.updateMarquee(at: document(end, scale: scale))
            } else {
                model.endMarquee(at: document(end, scale: scale))
            }
        }
    }

    /// Right-click selects what is under the pointer, then the menu asks.
    /// Real clicks arrive as AppKit events; the Accessibility path only posts
    /// menu items, so the bounds are resolved from the last pointer position.
    private func selectForMenu(_ point: CGPoint, scale: Double) {
        let location = document(point, scale: scale)
        if let hit = content.topmost(at: location) {
            if !model.selectedNodeIds.contains(hit.id) { model.selectNode(hit.id, extending: false) }
            return
        }
        guard let id = model.canvasHoveredId, let bounds = content.nodes.first(where: { $0.id == id }) else { return }
        let inside = location.x >= bounds.x && location.y >= bounds.y
            && location.x < bounds.x + bounds.w && location.y < bounds.y + bounds.h
        if inside, !model.selectedNodeIds.contains(id) { model.selectNode(id, extending: false) }
    }

    /// Right-click on an image or vector offers "restore as text" (C.8).
    @MainActor
    private func canvasMenu() -> NSMenu? {
        guard let id = model.inspectorNodeId, model.canRestoreAsText else { return nil }
        let menu = NSMenu()
        let item = NSMenuItem(title: "Восстановить как текст…", action: nil, keyEquivalent: "")
        item.target = model
        item.action = #selector(WorkspaceModel.restoreTextFromMenu(_:))
        item.representedObject = id
        menu.addItem(item)
        return menu
    }

    /// Points of a single selected vector, in view coordinates, with the
    /// pending drag applied so the handle follows the pointer.
    private func vectorPoints(scale: Double) -> [VectorHandle] {
        guard let id = model.inspectorNodeId,
              let node = content.nodes.first(where: { $0.id == id }), node.kind == "vector" else { return [] }
        let drag = model.vectorDragPreview
        let delta = drag?.id == id ? CGVector(dx: drag?.delta.dx ?? 0, dy: drag?.delta.dy ?? 0) : .zero
        return model.vectorPoints(id).map { point in
            VectorHandle(id: point.id,
                         position: CGPoint(x: (node.x + point.point.x + delta.dx) * scale,
                                           y: (node.y + point.point.y + delta.dy) * scale))
        }
    }

    private func onVectorDrag(scale: Double) -> (VectorHandleEvent) -> Void {
        { event in
            let point = document(event.point, scale: scale)
            switch event.phase {
            case .began: model.beginVectorDrag(event.nodeId, index: event.handle, at: point)
            case .changed: model.updateVectorDrag(at: point)
            case .ended: model.endVectorDrag(at: point)
            }
        }
    }

    private func document(_ point: CGPoint, scale: Double) -> CGPoint {
        CGPoint(x: point.x / scale, y: point.y / scale)
    }

    private func press(at point: CGPoint, scale: Double) {
        let document = document(point, scale: scale)
        let extending = NSEvent.modifierFlags.contains(.shift)
        guard let hit = content.topmost(at: document) else {
            model.clearSelection()
            return
        }
        model.beginCanvasGesture(.move, at: document, hit: CanvasHit(id: hit.id, shift: false, deep: false), extending: extending)
    }

    /// The stack order as shown in the layers panel: page first, then every
    /// ancestor above the selection.
    private func levels() -> [String] {
        var levels: [String] = []
        for id in model.selectedNodeIds {
        guard let node = content.nodes.first(where: { $0.id == id }) else { continue }
        var index = 0
        var parent = node.parentId
        while let current = parent, let currentParent = content.nodes.first(where: { $0.id == current }) {
            index += 1
            parent = currentParent.parentId
        }
        levels.append(String(index))
        }
        return levels
    }
}

/// Holds the WKWebView the canvas created so the view can push inspector
/// patches into the already loaded document between reloads.
@MainActor
@Observable
final class CanvasWebViewBridge {
    weak var webView: WKWebView?
}
