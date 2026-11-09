import AppKit
import SwiftUI

/// The page tree (M1.3): topmost sibling first, indented by depth, with
/// inline rename, visibility, locking, search and drag-and-drop reordering
/// among siblings. Reordering is expressed as a child index of the parent,
/// which is what the core's moveNode takes.
struct LayersPanel: View {
    @ObservedObject var model: WorkspaceModel

    @StoredState private var search = ""
    @StoredState private var collapsed: Set<String> = []
    @StoredState private var renameId: String?
    @StoredState private var renameText = ""
    @StoredState private var draggedId: String?

    var body: some View {
        VStack(spacing: 0) {
            TextField("Поиск слоёв", text: $search)
                .textFieldStyle(.roundedBorder)
                .padding(8)
                .accessibilityIdentifier("layers-search")
            Divider()
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(visibleRows) { row in
                        selfRow(row)
                    }
                }
                .padding(.vertical, 4)
            }
        }
        .accessibilityIdentifier("layers-panel")
        .onChange(of: model.snapshot?.document.revision) {
            if let renameId, model.rawNode(renameId) == nil { cancelRename() }
        }
    }

    // MARK: Rows

    private var rows: [LayerRow] {
        reversed(model.layerRows())
    }

    /// Sibling order reversed at every level: the topmost node comes first.
    private func reversed(_ flat: [LayerRow]) -> [LayerRow] {
        var output: [LayerRow] = []
        var index = 0
        func walk(_ depth: Int) {
            guard index < flat.count else { return }
            var count = 0
            while index + count < flat.count && flat[index + count].depth == depth { count += 1 }
            let level = Array(flat[index..<(index + count)])
            index += count
            for row in level.reversed() {
                output.append(row)
                if row.hasChildren && !collapsed.contains(row.id) { walk(depth + 1) }
            }
        }
        if let first = flat.first { walk(first.depth) }
        return output
    }

    private var visibleRows: [LayerRow] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return rows }
        let all = rows
        var keep = Set(all.filter { $0.name.localizedCaseInsensitiveContains(query) }.map(\.id))
        // Ancestors of a match stay visible so the tree keeps its shape.
        for (position, row) in all.enumerated() where keep.contains(row.id) {
            var depth = row.depth - 1
            var cursor = position - 1
            while depth >= 0 && cursor >= 0 {
                let ancestor = all[cursor]
                if ancestor.depth == depth {
                    keep.insert(ancestor.id)
                    depth -= 1
                }
                cursor -= 1
            }
        }
        return all.filter { keep.contains($0.id) }
    }

    private func selfRow(_ row: LayerRow) -> some View {
        LayersPanelRow(model: model, row: row,
                       collapsed: collapsed.contains(row.id),
                       dragging: draggedId == row.id,
                       renameId: renameId,
                       renaming: renameId == row.id,
                       onBeginRename: { beginRename(row) },
                       onCommitRename: { name in
                           renameId = nil
                           model.renameNode(row.id, to: name)
                       },
                       onToggleChildren: {
                           if collapsed.contains(row.id) { collapsed.remove(row.id) } else { collapsed.insert(row.id) }
                       },
                       onDrag: { draggedId = $0 },
                       onDrop: drop,
                       onOpen: { beginRename(row) })
            .padding(.leading, CGFloat(row.depth) * 14)
            .background(draggedId == row.id ? Color.accentColor.opacity(0.08) : Color.clear)
            .accessibilityIdentifier("layer-\(row.id)")
    }

    // MARK: Rename

    private func beginRename(_ row: LayerRow) {
        renameId = row.id
        renameText = row.name
    }

    private func commitRename() {
        guard let id = renameId else { return }
        renameId = nil
        model.renameNode(id, to: renameText)
    }

    private func cancelRename() {
        renameId = nil
        renameText = ""
    }

    // MARK: Drag and drop

    private func drop(_ id: String, on target: LayerRow) {
        defer { draggedId = nil }
        guard id != target.id, let dragged = model.rawNode(id), let targetNode = model.rawNode(target.id),
              let parentId = targetNode["parentId"] as? String,
              sameParent(dragged, targetNode) else { return }
        let parentChildren = model.rawNode(parentId)?["children"] as? [String] ?? []
        guard let to = parentChildren.firstIndex(of: target.id) else { return }
        // Dropping onto a row puts the node where that row is shown; rows are
        // reversed, so the index is the target's own index among siblings.
        model.reorderNode(id, toIndex: to)
    }

    private func sameParent(_ a: [String: Any], _ b: [String: Any]) -> Bool {
        (a["parentId"] as? String) == (b["parentId"] as? String)
    }
}

/// One layer row; a separate view so the inline rename field can own focus.
private struct LayersPanelRow: View {
    @ObservedObject var model: WorkspaceModel
    let row: LayerRow
    let collapsed: Bool
    let dragging: Bool
    let renameId: String?
    let renaming: Bool
    let onBeginRename: () -> Void
    let onCommitRename: (String) -> Void
    let onToggleChildren: () -> Void
    let onDrag: (String) -> Void
    let onDrop: (String, LayerRow) -> Void
    let onOpen: () -> Void

    @StoredState private var name = ""
    @StoredState private var hovering = false

    private var selected: Bool { model.selectedNodeIds.contains(row.id) }

    var body: some View {
        HStack(spacing: 6) {
            if row.hasChildren {
                Button(action: onToggleChildren) {
                    Image(systemName: collapsed ? "chevron.right" : "chevron.down")
                        .font(.caption2)
                        .frame(width: 12)
                }
                .buttonStyle(.plain)
            } else {
                Color.clear.frame(width: 12, height: 1)
            }
            Image(systemName: row.icon)
                .frame(width: 16)
                .foregroundStyle(.secondary)
            if renaming {
                TextField("Имя", text: $name)
                    .textFieldStyle(.plain)
                    .onSubmit { onCommitRename(name) }
                    .accessibilityIdentifier("layer-rename-\(row.id)")
            } else {
                Text(row.name)
                    .lineLimit(1)
                    .opacity(row.hidden ? 0.5 : 1)
                    .contentShape(Rectangle())
            }
            Spacer(minLength: 4)
            if hovering || row.hidden {
                Button {
                    model.setHidden(row.id, !row.hidden)
                } label: {
                    Image(systemName: row.hidden ? "eye.slash" : "eye")
                }
                .buttonStyle(.plain)
                .help(row.hidden ? "Показать слой" : "Скрыть слой")
                .accessibilityIdentifier("layer-visibility-\(row.id)")
            }
            if hovering || row.locked {
                Button {
                    model.setLocked(row.id, !row.locked)
                } label: {
                    Image(systemName: row.locked ? "lock.fill" : "lock.open")
                }
                .buttonStyle(.plain)
                .help(row.locked ? "Разблокировать слой" : "Заблокировать слой")
                .accessibilityIdentifier("layer-lock-\(row.id)")
            }
        }
        .font(.callout)
        .padding(.vertical, 3)
        .padding(.trailing, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .background(selected ? Color.accentColor.opacity(0.18) : Color.clear)
        .onHover { hovering = $0 }
        .simultaneousGesture(TapGesture(count: 2).onEnded { onBeginRename() })
        .onTapGesture {
            model.selectNode(row.id, extending: NSEvent.modifierFlags.contains(.shift))
        }
        .onAppear { name = row.name }
        .onChange(of: row.name) { name = row.name }
        .onChange(of: renaming) {
            if renaming { name = row.name }
        }
        .accessibilityAddTraits(selected ? [.isSelected] : [])
        .accessibilityAction { model.selectNode(row.id, extending: false) }
        .accessibilityAction(named: "Переименовать", onOpen)
        .onDrag {
            onDrag(row.id)
            return NSItemProvider(object: row.id as NSString)
        }
        .onDrop(of: [.text], isTargeted: nil) { providers in
            guard let provider = providers.first else { return false }
            _ = provider.loadObject(ofClass: NSString.self) { value, _ in
                guard let id = value as? String else { return }
                Task { @MainActor in onDrop(id, row) }
            }
            return true
        }
    }
}
