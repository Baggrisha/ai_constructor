import AppKit
import Foundation

/// Alignment edges of a multi-selection (C.4).
enum AlignmentEdge: String, CaseIterable {
    case left, centerX, right, top, centerY, bottom

    var title: String {
        switch self {
        case .left: return "По левому краю"
        case .centerX: return "По центру по горизонтали"
        case .right: return "По правому краю"
        case .top: return "По верхнему краю"
        case .centerY: return "По центру по вертикали"
        case .bottom: return "По нижнему краю"
        }
    }
}

enum DistributionAxis: String, CaseIterable {
    case horizontal, vertical

    var title: String { self == .horizontal ? "Распределить по горизонтали" : "Распределить по вертикали" }
}

/// Multi-selection, grouping, components, tokens, alignment, layer order,
/// vector points and image replacement (plan stages 9–10). Every call sends
/// at most one transaction, so each action is a single undo step (C.9).
extension WorkspaceModel {
    // MARK: Selection

    /// ⇧/⌘-clicks extend the selection; a plain click replaces it.
    func toggleSelection(_ id: String, extending: Bool) {
        selectNode(id, extending: extending)
    }

    func selectAllNodes() {
        commitDraft()
        guard let snapshot else { return }
        selectedNodeIds = Set(canvas.nodes.filter { $0.id != snapshot.root?.id && !$0.hidden }.map(\.id))
    }

    var selectionParentId: String? {
        guard let first = orderedSelection.first, let node = rawNode(first) else { return nil }
        return node["parentId"] as? String
    }

    // MARK: Grouping

    var canGroup: Bool {
        let ids = orderedSelection
        guard ids.count > 1, let parent = selectionParentId else { return false }
        return ids.allSatisfy { rawNode($0)?["parentId"] as? String == parent }
    }

    var canUngroup: Bool {
        guard let id = inspectorNodeId else { return false }
        return rawNode(id)?["kind"] as? String == "group"
    }

    func groupSelection() {
        guard canGroup else { return }
        let ids = orderedSelection
        sendCoreEdit(scope: ids) { try $0.groupJson(nodeIds: ids, name: nil) }
    }

    func ungroupSelection() {
        guard canUngroup, let id = inspectorNodeId else { return }
        sendCoreEdit(scope: [id]) { try $0.ungroupJson(groupId: id) }
    }

    // MARK: Copy, duplicate, paste, delete

    var canCopy: Bool { !orderedSelection.isEmpty }
    var canPaste: Bool { !writesBlocked && !(NSPasteboard.general.string(forType: .string) ?? "").isEmpty }

    func copySelection() {
        commitDraft()
        let ids = orderedSelection
        guard !ids.isEmpty, let projectId = snapshot?.document.projectId else { return }
        let payload: [String: Any] = ["projectId": projectId, "nodeIds": ids]
        guard let data = try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys]),
              let json = String(data: data, encoding: .utf8) else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(json, forType: .string)
    }

    /// Paste re-issues every identifier in the core; nodes copied from another
    /// project keep the source path so its assets resolve.
    func paste() {
        guard canPaste, let rootId = snapshot?.root?.id,
              let clipboard = NSPasteboard.general.string(forType: .string),
              let data = clipboard.data(using: .utf8),
              let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let ids = object["nodeIds"] as? [String], !ids.isEmpty else { return }
        let parentId = pasteParent(rootId: rootId)
        let sourceProject = object["projectId"] as? String ?? ""
        let sourcePath = library.recents.first { $0.projectId == sourceProject && $0.path != currentPath?.path }?.path
        if sourceProject != snapshot?.document.projectId, let sourcePath,
           FileManager.default.fileExists(atPath: sourcePath) {
            sendCoreEdit(scope: [parentId]) {
                try $0.pasteFromProjectJson(sourcePath: sourcePath, nodeIds: ids, parentId: parentId, index: nil)
            }
        } else {
            sendCoreEdit(scope: [parentId]) {
                try $0.pasteNodesJson(nodeIds: ids, parentId: parentId, index: nil)
            }
        }
    }

    func duplicateSelection() {
        commitDraft()
        let ids = orderedSelection
        guard !ids.isEmpty, let rootId = snapshot?.root?.id else { return }
        let parentId = ids.first.flatMap { rawNode($0)?["parentId"] as? String } ?? rootId
        sendCoreEdit(scope: [parentId]) {
            try $0.pasteNodesJson(nodeIds: ids, parentId: parentId, index: nil)
        }
    }

    func deleteSelection() {
        guard operation == nil, !writesBlocked, let snapshot else { return }
        let ids = Set(orderedSelection)
        // A selected parent takes its selected descendants with it.
        let roots = ids.filter { id in
            var parent = rawNode(id)?["parentId"] as? String
            while let current = parent {
                if ids.contains(current) { return false }
                parent = rawNode(current)?["parentId"] as? String
            }
            return true
        }
        guard !roots.isEmpty else { return }
        let operations: [[String: Any]] = roots.map { ["type": "removeNode", "nodeId": $0] }
        selectedNodeIds = []
        applyCanvasOperations(operations, scope: Array(roots), baseRevision: snapshot.document.revision, baseFingerprint: snapshot.fingerprint)
    }

    /// Where new nodes go: the selected frame or the selection's parent, never
    /// a text or image node.
    private func pasteParent(rootId: String) -> String {
        guard let id = inspectorNodeId ?? orderedSelection.first,
              let node = rawNode(id), let parentId = node["parentId"] as? String else { return rootId }
        if ["frame", "group", "button", "componentInstance"].contains(node["kind"] as? String ?? "") { return id }
        return parentId
    }

    // MARK: Components

    var canCreateComponent: Bool {
        var seen = Set<String>()
        return orderedSelection.contains { id in
            guard let node = rawNode(id), let parentId = node["parentId"] as? String else { return false }
            return seen.insert(parentId).inserted
        } && !orderedSelection.isEmpty
    }

    func createComponentFromSelection() {
        guard canCreateComponent else { return }
        let ids = orderedSelection
        sendCoreEdit(scope: ids) { try $0.createComponentJson(nodeIds: ids, name: nil) }
    }

    struct ComponentItem: Identifiable {
        let id: String
        let name: String
    }

    var components: [ComponentItem] {
        guard let json = try? session?.snapshotJson(),
              let object = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any],
              let document = object["document"] as? [String: Any],
              let list = document["components"] as? [[String: Any]] else { return [] }
        let nodes = rawDocumentNodes
        return list.compactMap { item in
            guard let id = item["id"] as? String, let rootId = item["rootId"] as? String else { return nil }
            let root = nodes.first { $0["id"] as? String == rootId }
            let name = item["name"] as? String ?? root?["name"] as? String ?? "Компонент"
            return ComponentItem(id: id, name: name)
        }
    }

    func insertComponent(_ id: String) {
        guard !writesBlocked, let rootId = snapshot?.root?.id else { return }
        let parentId = pasteParent(rootId: rootId)
        sendCoreEdit(scope: [parentId]) {
            try $0.insertComponentInstanceJson(componentId: id, parentId: parentId, index: nil, x: 80, y: 80)
        }
    }

    var canBreakComponent: Bool {
        guard let id = inspectorNodeId else { return false }
        return rawNode(id)?["kind"] as? String == "componentInstance"
    }

    func breakComponentLink() {
        guard canBreakComponent, let id = inspectorNodeId else { return }
        sendCoreEdit(scope: [id]) { try $0.breakComponentLinkJson(instanceId: id) }
    }

    // MARK: Alignment and distribution

    var canAlign: Bool { orderedSelection.count > 1 }

    /// Selected siblings move against their common box; the result is plain
    /// layout values, so no core call is needed beyond the transaction.
    func align(_ edge: AlignmentEdge) {
        let ids = orderedSelection
        guard ids.count > 1, let parent = selectionParentId,
              ids.allSatisfy({ rawNode($0)?["parentId"] as? String == parent }) else { return }
        let bounds = ids.compactMap { id in canvas.nodes.first { $0.id == id } }
        guard bounds.count == ids.count, let first = bounds.first else { return }
        let left = bounds.map(\.x).min() ?? first.x
        let right = bounds.map { $0.x + $0.w }.max() ?? first.x + first.w
        let top = bounds.map(\.y).min() ?? first.y
        let bottom = bounds.map { $0.y + $0.h }.max() ?? first.y + first.h
        var operations: [[String: Any]] = []
        for node in bounds {
            switch edge {
            case .left: operations.append(setX(node.id, left))
            case .centerX: operations.append(setX(node.id, (left + right) / 2 - node.w / 2))
            case .right: operations.append(setX(node.id, right - node.w))
            case .top: operations.append(setY(node.id, top))
            case .centerY: operations.append(setY(node.id, (top + bottom) / 2 - node.h / 2))
            case .bottom: operations.append(setY(node.id, bottom - node.h))
            }
        }
        applyGroupOperations(operations, scope: ids + [parent])
    }

    /// Equal spacing between siblings, outermost two staying in place.
    func distribute(_ axis: DistributionAxis) {
        let ids = orderedSelection
        guard ids.count > 2, let parent = selectionParentId,
              ids.allSatisfy({ rawNode($0)?["parentId"] as? String == parent }) else { return }
        let nodes = canvas.nodes.filter { ids.contains($0.id) }.sorted {
            axis == .horizontal ? $0.x < $1.x : $0.y < $1.y
        }
        guard nodes.count == ids.count, let first = nodes.first, let last = nodes.last else { return }
        let total = nodes.reduce(0) { $0 + (axis == .horizontal ? $1.w : $1.h) }
        let span = (axis == .horizontal ? last.x + last.w : last.y + last.h) - (axis == .horizontal ? first.x : first.y)
        let gap = (span - total) / Double(nodes.count - 1)
        var cursor = axis == .horizontal ? first.x : first.y
        var operations: [[String: Any]] = []
        for node in nodes {
            operations.append(axis == .horizontal ? setX(node.id, cursor) : setY(node.id, cursor))
            cursor += (axis == .horizontal ? node.w : node.h) + gap
        }
        applyGroupOperations(operations, scope: ids + [parent])
    }

    private func setX(_ id: String, _ value: Double) -> [String: Any] {
        ["type": "setProperty", "nodeId": id, "path": "/layout/x", "value": round(value)]
    }

    private func setY(_ id: String, _ value: Double) -> [String: Any] {
        ["type": "setProperty", "nodeId": id, "path": "/layout/y", "value": round(value)]
    }

    private func applyGroupOperations(_ operations: [[String: Any]], scope: [String]) {
        guard !operations.isEmpty, let snapshot else { return }
        applyCanvasOperations(operations, scope: Array(Set(scope)), baseRevision: snapshot.document.revision, baseFingerprint: snapshot.fingerprint)
    }

    // MARK: Layer order

    var canReorderLayers: Bool {
        guard let id = inspectorNodeId, let node = rawNode(id),
              let parentId = node["parentId"] as? String,
              let siblings = rawNode(parentId)?["children"] as? [String] else { return false }
        return siblings.count > 1
    }

    func bringForward() { shiftLayer(by: 1) }
    func sendBackward() { shiftLayer(by: -1) }
    func bringToFront() { shiftLayer(to: .top) }
    func sendToBack() { shiftLayer(to: .bottom) }

    private enum LayerTarget { case top, bottom }

    private func shiftLayer(to target: LayerTarget) {
        guard let id = inspectorNodeId, let node = rawNode(id),
              let siblings = rawNode(node["parentId"] as? String ?? "")?["children"] as? [String] else { return }
        reorderNode(id, toIndex: target == .top ? siblings.count - 1 : 0)
    }

    private func shiftLayer(by offset: Int) {
        guard let id = inspectorNodeId, let node = rawNode(id),
              let siblings = rawNode(node["parentId"] as? String ?? "")?["children"] as? [String],
              let index = siblings.firstIndex(of: id) else { return }
        reorderNode(id, toIndex: index + offset)
    }

    // MARK: Tokens

    struct TokenRow: Identifiable {
        let id: String
        let name: String
        let value: String
        let isNumber: Bool
        let isBoolean: Bool
    }

    private var documentJSON: [String: Any] {
        guard let json = try? session?.snapshotJson(),
              let object = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any],
              let document = object["document"] as? [String: Any] else { return [:] }
        return document
    }

    var tokenRows: [TokenRow] {
        let tokens = documentJSON["tokens"] as? [[String: Any]] ?? []
        return tokens.compactMap { token -> TokenRow? in
            guard let id = token["id"] as? String, let name = token["name"] as? String,
                  let value = token["value"] as? [String: Any] else { return nil }
            if let number = InspectorJSON.number(value["number"]) {
                return TokenRow(id: id, name: name, value: number.formatted(.number.precision(.fractionLength(0...3))), isNumber: true, isBoolean: false)
            }
            if let flag = InspectorJSON.flag(value["boolean"]) {
                return TokenRow(id: id, name: name, value: flag ? "true" : "false", isNumber: false, isBoolean: true)
            }
            return TokenRow(id: id, name: name, value: value["string"] as? String ?? "", isNumber: false, isBoolean: false)
        }
    }

    /// Nodes bound to the token, listed before a change is applied (C.7).
    func bindings(forToken tokenId: String) -> [String] {
        let nodes = rawDocumentNodes
        var bound: [String] = []
        for node in nodes {
            guard let id = node["id"] as? String else { continue }
            if let bindings = node["bindings"] as? [String: Any],
               bindings.values.contains(where: { ($0 as? [String: Any])?["tokenId"] as? String == tokenId }) {
                bound.append(id)
            }
        }
        return bound
    }

    func updateToken(_ row: TokenRow, value: String) {
        guard operation == nil, !writesBlocked, let snapshot else { return }
        let trimmed = value.trimmingCharacters(in: .whitespaces)
        // Token values are bare JSON scalars, as the core decodes them.
        let scalar: Any
        if row.isBoolean {
            scalar = trimmed == "true"
        } else if row.isNumber, let number = Double(trimmed.replacingOccurrences(of: ",", with: ".")) {
            scalar = number
        } else if !row.isNumber {
            scalar = value
        } else {
            error = "Значение токена «\(row.name)» должно быть числом."
            return
        }
        let token: [String: Any] = ["id": row.id, "name": row.name, "value": scalar]
        let scope = Array(Set([snapshot.root?.id ?? ""] + bindings(forToken: row.id))).filter { !$0.isEmpty }
        applyCanvasOperations([["type": "setToken", "token": token]], scope: scope,
                              baseRevision: snapshot.document.revision, baseFingerprint: snapshot.fingerprint)
    }

    // MARK: Vector points

    struct VectorPoint: Identifiable {
        let id: Int
        let point: CGPoint
    }

    /// Movable points of a vector node relative to its own box.
    func vectorPoints(_ id: String) -> [VectorPoint] {
        guard let node = rawNode(id), let content = node["content"] as? [String: Any],
              let vector = content["vector"] as? [String: Any],
              let paths = vector["paths"] as? [[Any]] else { return [] }
        var points: [VectorPoint] = []
        for path in paths {
            for command in path {
                guard let command = command as? [String: Any],
                      let x = InspectorJSON.number(command["x"]), let y = InspectorJSON.number(command["y"]) else { continue }
                points.append(VectorPoint(id: points.count, point: CGPoint(x: x, y: y)))
            }
        }
        return points
    }

    /// Moves one point and writes the whole vector back: one transaction.
    func moveVectorPoint(_ id: String, index: Int, by delta: CGVector) {
        guard operation == nil, !writesBlocked, let snapshot, let node = rawNode(id),
              var content = node["content"] as? [String: Any],
              var vector = content["vector"] as? [String: Any],
              var paths = vector["paths"] as? [[Any]] else { return }
        var seen = 0
        outer: for pathIndex in paths.indices {
            for commandIndex in paths[pathIndex].indices {
                guard var command = paths[pathIndex][commandIndex] as? [String: Any],
                      let x = InspectorJSON.number(command["x"]), let y = InspectorJSON.number(command["y"]) else { continue }
                if seen == index {
                    command["x"] = x + delta.dx
                    command["y"] = y + delta.dy
                    paths[pathIndex][commandIndex] = command
                    break outer
                }
                seen += 1
            }
        }
        vector["paths"] = paths
        content["vector"] = vector
        let operation: [String: Any] = ["type": "setProperty", "nodeId": id, "path": "/content/vector", "value": vector]
        applyCanvasOperations([operation], scope: [id], baseRevision: snapshot.document.revision, baseFingerprint: snapshot.fingerprint)
    }

    // MARK: Images

    /// Imports the chosen file and points the image at it; the old asset stays
    /// until history drops it.
    func replaceImage(_ id: String) {
        guard operation == nil, !writesBlocked, let session else { return }
        let panel = NSOpenPanel()
        panel.title = "Заменить изображение"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let control = newOperationControl()
        perform("Замена изображения…", control: control, work: {
            _ = try session.importAssetControlled(path: url.path, control: control)
            return try decodeJSON(Snapshot.self, session.snapshotJson())
        }) { (snapshot: Snapshot) in
            self.applySnapshot(snapshot)
            guard let asset = snapshot.document.assets.last else { return }
            self.setNodeProperty(id, "/content/assetId", asset.id)
        }
    }

    func setImageFit(_ id: String, _ fit: String) {
        setNodeProperty(id, "/style/fit", fit)
    }

    // MARK: Restore as text (C.8)

    var canRestoreAsText: Bool {
        guard let id = inspectorNodeId, let node = rawNode(id) else { return false }
        return ["image", "vector"].contains(node["kind"] as? String ?? "")
    }

    /// Swaps an image or vector for a text layer over the same box; the
    /// original stays in history and is one undo away.
    func restoreAsText(_ id: String, text: String, fontFamily: String?) {
        guard operation == nil, !writesBlocked, let snapshot, let node = rawNode(id),
              let parentId = node["parentId"] as? String,
              let layout = node["layout"] as? [String: Any] else { return }
        var style: [String: Any] = ["color": "#111827"]
        if let fontFamily, !fontFamily.isEmpty { style["fontFamily"] = fontFamily }
        let replacement: [String: Any] = [
            "id": id,
            "parentId": parentId,
            "kind": "text",
            "name": node["name"] as? String ?? "Текст",
            "children": [Any](),
            "layout": layout,
            "hidden": node["hidden"] as? Bool ?? false,
            "locked": node["locked"] as? Bool ?? false,
            "style": style,
            "content": ["text": text],
            "a11y": [String: Any](),
            "bindings": [String: Any](),
            "events": [String: Any](),
            "propertyMeta": [String: Any](),
            "sourceBounds": NSNull(),
            "sourceDensity": NSNull(),
            "responsive": [Any]()
        ]
        applyCanvasOperations([["type": "swapNode", "node": replacement]], scope: [id, parentId],
                              baseRevision: snapshot.document.revision, baseFingerprint: snapshot.fingerprint)
    }

    @objc func restoreTextFromMenu(_ sender: NSMenuItem) {
        restoreTextTarget = sender.representedObject as? String
    }

    // MARK: Shared core commits

    /// Editing helpers in the core build and check a transaction for the
    /// current document. The transaction is applied and saved here, so one
    /// gesture stays one undo step.
    func sendCoreEdit(scope: [String], work: @escaping (ProjectSession) throws -> String) {
        guard operation == nil, !writesBlocked, let session else { return }
        let control = newOperationControl()
        let previous = possibleUnsaved
        possibleUnsaved = true
        perform("Сохранение изменений…", control: control, work: {
            let transaction = try work(session)
            return try editAndSaveSnapshot(session) {
                try session.applyTransactionAndSaveControlled(transactionJson: transaction, control: control)
            }
        }, failure: { error in
            if wasCanceled(error) { self.possibleUnsaved = previous }
        }) { result in
            self.accept(result)
            if let failure = result.error {
                self.error = [failure, "Действие не сохранено; холст показывает состояние проекта."].joined(separator: "\n")
            }
            self.refreshCanvas()
        }
    }
}
