import AppKit
import CoreServices
import SwiftUI

// Use SwiftUI’s property wrapper when the CLT does not include SwiftUIMacros.
typealias StoredState<Value> = SwiftUI.State<Value>

struct FrameEditor: View {
    let snapshot: Snapshot
    let root: Snapshot.Document.Node
    @ObservedObject var model: WorkspaceModel
    let editorId: UUID
    private enum Field: Hashable { case name, width, height }
    @FocusState private var focus: Field?
    init(snapshot: Snapshot, root: Snapshot.Document.Node, model: WorkspaceModel, editorId: UUID) {
        self.snapshot = snapshot
        self.root = root
        self.model = model
        self.editorId = editorId
    }

    private func binding(_ field: WritableKeyPath<FrameDraft, String>) -> Binding<String> {
        Binding(
            get: { (model.frameDraft ?? FrameDraft(root))[keyPath: field] },
            set: { if model.frameEditorId == editorId { model.updateFrameDraft(field, value: $0) } }
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
            guard model.frameEditorId == editorId else { return }
            model.setFrameFocused(next != nil)
            if previous != nil && previous != next { model.commitFrameDraft() }
        }
        .onDisappear {
            guard model.frameEditorId == editorId else { return }
            model.setFrameFocused(false)
            model.commitFrameDraft()
        }
    }
}

struct SidebarMaterial: View {
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

struct VisualEffect: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .sidebar
        view.blendingMode = .behindWindow
        view.state = .active
        return view
    }
    func updateNSView(_ view: NSVisualEffectView, context: Context) {}
}
