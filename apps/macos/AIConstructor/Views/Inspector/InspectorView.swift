import AppKit
import SwiftUI

/// The property panel (C.7). Every field writes through the model's inspector
/// draft, so typing becomes one transaction after a pause and losing focus
/// commits at once. Sections planned for M2 are shown disabled, never hidden.
struct InspectorView: View {
    @ObservedObject var model: WorkspaceModel
    let snapshot: Snapshot

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                if let id = model.inspectorNodeId, let node = model.rawNode(id) {
                    InspectorNodeHeader(model: model, node: node)
                    Divider()
                    InspectorDesignSection(model: model, node: node)
                    Divider()
                    InspectorBehaviorSection(model: model, node: node)
                    Divider()
                    InspectorLockSection(model: model, node: node)
                    Divider()
                    DisabledSection(title: "Анимация", note: "Появится в M2")
                    Divider()
                    DisabledSection(title: "Код", note: "Появится в M2")
                } else if model.selectedNodeIds.isEmpty {
                    ContentUnavailableView("Ничего не выбрано", systemImage: "cursorarrow.click",
                                           description: Text("Выберите слой на холсте или в панели «Слои»."))
                        .frame(maxWidth: .infinity)
                        .padding(.top, 40)
                } else {
                    ContentUnavailableView("Выбрано \(model.selectedNodeIds.count) слоя", systemImage: "square.on.square",
                                           description: Text("Инспектор редактирует один слой. Выберите один слой."))
                        .frame(maxWidth: .infinity)
                        .padding(.top, 40)
                }
            }
        }
        .accessibilityIdentifier("inspector")
        // Committing on focus loss keeps ⌘Tab and panel switches from
        // stranding a typed value in a draft.
        .onDisappear { model.commitDraft() }
    }
}

private struct InspectorNodeHeader: View {
    @ObservedObject var model: WorkspaceModel
    let node: [String: Any]
    @StoredState private var typed: String?

    private var id: String { node["id"] as? String ?? "" }
    private var name: String { typed ?? node["name"] as? String ?? "" }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text(kindTitle).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                Spacer()
                Text(String(id.prefix(8))).font(.caption2.monospaced()).foregroundStyle(.tertiary)
            }
            TextField("Имя слоя", text: Binding(get: { name }, set: { typed = $0 }))
                .textFieldStyle(.roundedBorder)
                .onSubmit { model.renameNode(id, to: name) }
                .accessibilityIdentifier("inspector-name")
        }
        .padding(12)
        .onChange(of: model.draftVersion) { typed = nil }
    }

    private var kindTitle: String {
        switch node["kind"] as? String ?? "" {
        case "frame": return "Фрейм"
        case "group": return "Группа"
        case "text": return "Текст"
        case "image": return "Изображение"
        case "vector": return "Вектор"
        case "button": return "Кнопка"
        case "input": return "Поле ввода"
        case "textarea": return "Многострочное поле"
        case "checkbox": return "Флажок"
        case "select": return "Список"
        case "componentInstance": return "Экземпляр компонента"
        case "codeComponent": return "Код-компонент"
        default: return node["kind"] as? String ?? "Слой"
        }
    }
}

/// Field kinds decide the control and whether input is debounced.
private enum FieldKind {
    case number, text, color, toggle
}

private struct FieldSpec: Identifiable {
    let path: String
    let label: String
    let kind: FieldKind
    let debounce: Bool

    var id: String { path }

    init(_ path: String, _ label: String, _ kind: FieldKind, debounce: Bool = false) {
        self.path = path
        self.label = label
        self.kind = kind
        self.debounce = debounce
    }
}

private struct InspectorField: View {
    @ObservedObject var model: WorkspaceModel
    let node: [String: Any]
    let spec: FieldSpec
    /// `nil` means "untouched": the field shows the document value. Only an
    /// edit stores a value, so a state copy can never strand an empty field.
    @StoredState private var typedText: String?
    @StoredState private var typedFlag: Bool?
    @StoredState private var typedColor: Color?

    init(model: WorkspaceModel, node: [String: Any], spec: FieldSpec) {
        self.model = model
        self.node = node
        self.spec = spec
    }

    private var value: Any? { model.inspectorValue(node, spec.path) }
    private var text: String { typedText ?? InspectorField.format(value) }
    private var flag: Bool { typedFlag ?? InspectorJSON.flag(value) ?? false }
    private var color: Color { typedColor ?? NSColor(rassvetHex: value as? String ?? "").map(Color.init(nsColor:)) ?? .clear }

    var body: some View {
        HStack(spacing: 8) {
            Text(spec.label).font(.caption).foregroundStyle(.secondary).frame(width: 78, alignment: .leading)
            switch spec.kind {
            case .number:
                TextField("", text: Binding(get: { text }, set: { typedText = $0 }))
                    .textFieldStyle(.roundedBorder)
                    .multilineTextAlignment(.trailing)
                    .onSubmit { commitNumber(text) }
                    .onChange(of: typedText) { _, next in
                        guard spec.debounce, let next else { return }
                        if let parsed = Self.parse(next) {
                            model.editProperty(spec.path, value: parsed)
                        }
                    }
            case .text:
                TextField("", text: Binding(get: { text }, set: { typedText = $0 }))
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { commitText(text) }
                    .onChange(of: typedText) { _, next in
                        guard spec.debounce, let next else { return }
                        model.editProperty(spec.path, value: next.isEmpty ? nil : next)
                    }
            case .color:
                HStack(spacing: 6) {
                    TextField("#RRGGBB", text: Binding(get: { text }, set: { typedText = $0 }))
                        .textFieldStyle(.roundedBorder)
                        .font(.body.monospaced())
                        .onSubmit { commitColor(text) }
                    ColorPicker("", selection: Binding(
                        get: { color },
                        set: { next in
                            typedColor = next
                            guard let hex = NSColor(next).rassvetHex else { return }
                            typedText = hex
                            commitColor(hex)
                        }), supportsOpacity: false)
                        .labelsHidden()
                }
            case .toggle:
                Toggle("", isOn: Binding(
                    get: { flag },
                    set: { next in
                        typedFlag = next
                        model.editProperty(spec.path, value: next)
                    }))
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .labelsHidden()
            }
        }
        .accessibilityIdentifier("inspector-\(spec.path)")
        // A document update (undo, commit) replaces local edits.
        .onChange(of: model.draftVersion) {
            if !isFocused {
                typedText = nil
                typedFlag = nil
                typedColor = nil
            }
        }
        // A new selection starts every field from the document.
        .id(node["id"] as? String ?? "")
    }

    private var isFocused: Bool { NSApp.keyWindow?.firstResponder is NSTextView }

    private func commitNumber(_ text: String) {
        model.editProperty(spec.path, value: Self.parse(text))
    }

    private func commitText(_ text: String) {
        model.editProperty(spec.path, value: text.isEmpty ? nil : text)
    }

    private func commitColor(_ text: String) {
        model.editProperty(spec.path, value: text.trimmingCharacters(in: .whitespaces).isEmpty ? nil : text)
    }

    static func parse(_ text: String) -> Double? {
        Double(text.replacingOccurrences(of: ",", with: ".").trimmingCharacters(in: .whitespaces))
    }

    static func format(_ value: Any?) -> String {
        if let number = value as? NSNumber {
            let value = number.doubleValue
            return value.rounded() == value ? String(format: "%.0f", value) : String(value)
        }
        if let text = value as? String { return text }
        return ""
    }
}

private struct InspectorDesignSection: View {
    @ObservedObject var model: WorkspaceModel
    let node: [String: Any]
    @StoredState private var insertKind = "frame"

    private var kind: String { node["kind"] as? String ?? "" }
    private var isFlowContainer: Bool { ["frame", "group", "button"].contains(kind) && node["layout"].flatMap { ($0 as? [String: Any])?["flow"] } != nil }
    private var hasText: Bool { ["text", "button", "input", "textarea", "checkbox", "select"].contains(kind) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionTitle("Дизайн")
            HStack(spacing: 12) {
                ForEach(positionFields, id: \.self) { path in
                    CompactNumberField(model: model, node: node, path: path, label: path.split(separator: "/").last.map(String.init) ?? path)
                }
            }
            Group {
                InspectorField(model: model, node: node, spec: FieldSpec("/style/fill", "Заливка", .color))
                if kind == "vector" || kind == "text" {
                    ColorSliderRows(model: model, node: node)
                }
                InspectorField(model: model, node: node, spec: FieldSpec("/style/radius", "Радиус", .number, debounce: true))
                InspectorField(model: model, node: node, spec: FieldSpec("/style/opacity", "Прозрачность", .number, debounce: true))
            }
            if hasText { textFields }
            if kind == "image" { imageFields }
            if isFlowContainer { flowFields }
            Toggle("Скрыт", isOn: Binding(
                get: { InspectorJSON.flag(model.inspectorValue(node, "/hidden")) ?? false },
                set: { model.editProperty("/hidden", value: $0) }))
                .toggleStyle(.switch)
                .controlSize(.small)
                .accessibilityIdentifier("inspector-visible")
            InsertMenu(model: model, kind: $insertKind)
        }
        .padding(12)
    }

    private var positionFields: [String] { ["/layout/x", "/layout/y", "/layout/w", "/layout/h"] }

    @ViewBuilder
    private var textFields: some View {
        Text("Текст").font(.caption.weight(.semibold)).foregroundStyle(.secondary).padding(.top, 4)
        if kind == "text" || kind == "button" || kind == "checkbox" || kind == "select" {
            InspectorField(model: model, node: node, spec: FieldSpec("/content/text", "Содержимое", .text, debounce: true))
        }
        if kind == "input" || kind == "textarea" || kind == "select" {
            InspectorField(model: model, node: node, spec: FieldSpec("/content/placeholder", "Placeholder", .text, debounce: true))
        }
        InspectorField(model: model, node: node, spec: FieldSpec("/style/fontFamily", "Шрифт", .text))
        FontSizeField(model: model, node: node)
        InspectorField(model: model, node: node, spec: FieldSpec("/style/fontWeight", "Насыщенность", .number))
        InspectorField(model: model, node: node, spec: FieldSpec("/style/lineHeight", "Межстрочный", .number, debounce: true))
        InspectorField(model: model, node: node, spec: FieldSpec("/style/letterSpacing", "Трекинг", .number, debounce: true))
        InspectorField(model: model, node: node, spec: FieldSpec("/style/color", "Цвет текста", .color))
        InspectorField(model: model, node: node, spec: FieldSpec("/style/maxLines", "Макс. строк", .number))
    }

    @ViewBuilder
    private var imageFields: some View {
        Text("Изображение").font(.caption.weight(.semibold)).foregroundStyle(.secondary).padding(.top, 4)
        InspectorField(model: model, node: node, spec: FieldSpec("/style/fit", "Вписывание", .text))
        Label("Кадрирование недоступно в M1", systemImage: "nosign")
            .font(.caption2).foregroundStyle(.secondary)
    }

    @ViewBuilder
    private var flowFields: some View {
        Text("Автолейаут").font(.caption.weight(.semibold)).foregroundStyle(.secondary).padding(.top, 4)
        InspectorField(model: model, node: node, spec: FieldSpec("/layout/flow/direction", "Направление", .text))
        InspectorField(model: model, node: node, spec: FieldSpec("/layout/flow/gap", "Отступ", .number, debounce: true))
        InspectorField(model: model, node: node, spec: FieldSpec("/layout/flow/align", "Выравнивание", .text))
        InspectorField(model: model, node: node, spec: FieldSpec("/layout/flow/padding/top", "Отступ сверху", .number, debounce: true))
        InspectorField(model: model, node: node, spec: FieldSpec("/layout/flow/padding/bottom", "Отступ снизу", .number, debounce: true))
        InspectorField(model: model, node: node, spec: FieldSpec("/layout/flow/padding/left", "Отступ слева", .number, debounce: true))
        InspectorField(model: model, node: node, spec: FieldSpec("/layout/flow/padding/right", "Отступ справа", .number, debounce: true))
    }
}

private struct InspectorBehaviorSection: View {
    @ObservedObject var model: WorkspaceModel
    let node: [String: Any]
    @StoredState private var typedAlign: String?
    @StoredState private var typedWrap: String?

    init(model: WorkspaceModel, node: [String: Any]) {
        self.model = model
        self.node = node
    }

    private var align: String { typedAlign ?? model.inspectorValue(node, "/style/textAlign") as? String ?? "" }
    private var wrap: String { typedWrap ?? model.inspectorValue(node, "/style/textWrap") as? String ?? "" }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionTitle("Поведение")
            HStack(spacing: 8) {
                Text("Выравнивание").font(.caption).foregroundStyle(.secondary).frame(width: 78, alignment: .leading)
                Picker("", selection: Binding(
                    get: { align },
                    set: { next in
                        typedAlign = next
                        model.editProperty("/style/textAlign", value: next.isEmpty ? nil : next)
                    })) {
                    Text("Не задано").tag("")
                    Text("По началу").tag("start")
                    Text("По центру").tag("center")
                    Text("По концу").tag("end")
                    Text("По ширине").tag("justify")
                }
                .labelsHidden()
            }
            HStack(spacing: 8) {
                Text("Перенос").font(.caption).foregroundStyle(.secondary).frame(width: 78, alignment: .leading)
                Picker("", selection: Binding(
                    get: { wrap },
                    set: { next in
                        typedWrap = next
                        model.editProperty("/style/textWrap", value: next.isEmpty ? nil : next)
                    })) {
                    Text("Не задано").tag("")
                    Text("Переносить").tag("word")
                    Text("Обрезать").tag("none")
                }
                .labelsHidden()
            }
            Label("Несколько стилей в одном тексте недоступны в M1", systemImage: "nosign")
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(12)
        .onChange(of: model.draftVersion) {
            typedAlign = nil
            typedWrap = nil
        }
    }
}

private struct InspectorLockSection: View {
    @ObservedObject var model: WorkspaceModel
    let node: [String: Any]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionTitle("Агент")
            Toggle("Агенту можно менять позицию и стиль", isOn: Binding(
                get: { model.agentUnlocked(node, "/layout") && model.agentUnlocked(node, "/style") },
                set: { unlocked in
                    model.setAgentLock(node["id"] as? String ?? "", path: "/layout", unlocked: unlocked)
                    model.setAgentLock(node["id"] as? String ?? "", path: "/style", unlocked: unlocked)
                }))
                .toggleStyle(.switch)
                .controlSize(.small)
                .help("Свойства, изменённые вручную, по умолчанию защищены от агента (C.9).")
                .accessibilityIdentifier("inspector-agent-unlocked")
        }
        .padding(12)
    }
}

private struct CompactNumberField: View {
    @ObservedObject var model: WorkspaceModel
    let node: [String: Any]
    let path: String
    let label: String
    @StoredState private var typed: String?

    init(model: WorkspaceModel, node: [String: Any], path: String, label: String) {
        self.model = model
        self.node = node
        self.path = path
        self.label = label
    }

    private var text: String { typed ?? InspectorField.format(model.inspectorValue(node, path)) }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.uppercased()).font(.caption2).foregroundStyle(.secondary)
            TextField("", text: Binding(get: { text }, set: { typed = $0 }))
                .textFieldStyle(.roundedBorder)
                .multilineTextAlignment(.trailing)
                .onSubmit { model.editProperty(path, value: InspectorField.parse(text)) }
                .onChange(of: typed) { _, next in
                    guard let next, let value = InspectorField.parse(next) else { return }
                    model.editProperty(path, value: value)
                }
        }
        .frame(minWidth: 48)
        .onChange(of: model.draftVersion) { typed = nil }
    }
}

private struct FontSizeField: View {
    @ObservedObject var model: WorkspaceModel
    let node: [String: Any]
    @StoredState private var typed: String?

    init(model: WorkspaceModel, node: [String: Any]) {
        self.model = model
        self.node = node
    }

    private var size: String { typed ?? InspectorField.format(model.inspectorValue(node, "/style/fontSize")) }

    var body: some View {
        HStack(spacing: 8) {
            Text("Размер").font(.caption).foregroundStyle(.secondary).frame(width: 78, alignment: .leading)
            TextField("", text: Binding(get: { size }, set: { typed = $0 }))
                .textFieldStyle(.roundedBorder)
                .multilineTextAlignment(.trailing)
                .onSubmit { model.editProperty("/style/fontSize", value: InspectorField.parse(size)) }
                .onChange(of: typed) { _, next in
                    guard let next, let value = InspectorField.parse(next) else { return }
                    model.editProperty("/style/fontSize", value: value)
                }
        }
        .onChange(of: model.draftVersion) { typed = nil }
    }
}

private struct ColorSliderRows: View {
    @ObservedObject var model: WorkspaceModel
    let node: [String: Any]

    var body: some View {
        InspectorField(model: model, node: node, spec: FieldSpec("/style/stroke/color", "Обводка", .color))
        InspectorField(model: model, node: node, spec: FieldSpec("/style/stroke/width", "Толщина", .number, debounce: true))
        InspectorField(model: model, node: node, spec: FieldSpec("/style/rotate", "Поворот", .number, debounce: true))
    }
}

private struct InsertMenu: View {
    @ObservedObject var model: WorkspaceModel
    @Binding var kind: String

    var body: some View {
        HStack(spacing: 8) {
            Picker("Вставить", selection: $kind) {
                ForEach(InsertedNodeTemplate.kinds, id: \.self) { value in
                    Text(title(value)).tag(value)
                }
            }
            .labelsHidden()
            .accessibilityIdentifier("inspector-insert-kind")
            Button("Вставить слой") { model.insertNode(kind) }
                .disabled(model.writesBlocked)
                .accessibilityIdentifier("inspector-insert")
        }
        .padding(.top, 4)
    }

    private func title(_ kind: String) -> String {
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

private struct SectionTitle: View {
    let text: String
    init(_ text: String) { self.text = text }
    var body: some View {
        Text(text).font(.caption.weight(.bold)).textCase(.uppercase).foregroundStyle(.secondary)
    }
}

private struct DisabledSection: View {
    let title: String
    let note: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            SectionTitle(title)
            Text(note).font(.caption).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .disabled(true)
    }
}

extension NSColor {
    convenience init?(rassvetHex hex: String) {
        var value = hex.trimmingCharacters(in: .whitespacesAndNewlines)
        if value.hasPrefix("#") { value.removeFirst() }
        guard (value.count == 6 || value.count == 8), let number = UInt64(value, radix: 16) else { return nil }
        let hasAlpha = value.count == 8
        let r = CGFloat((number >> (hasAlpha ? 24 : 16)) & 0xFF) / 255
        let g = CGFloat((number >> (hasAlpha ? 16 : 8)) & 0xFF) / 255
        let b = CGFloat((number >> (hasAlpha ? 8 : 0)) & 0xFF) / 255
        let a = hasAlpha ? CGFloat(number & 0xFF) / 255 : 1
        self.init(srgbRed: r, green: g, blue: b, alpha: a)
    }

    var rassvetHex: String? {
        guard let srgb = usingColorSpace(.sRGB) else { return nil }
        return String(format: "#%02X%02X%02X",
                      Int((srgb.redComponent * 255).rounded()),
                      Int((srgb.greenComponent * 255).rounded()),
                      Int((srgb.blueComponent * 255).rounded()))
    }
}

