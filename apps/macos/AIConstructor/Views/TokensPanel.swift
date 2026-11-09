import AppKit
import SwiftUI

/// Document tokens (C.7): name, value and the layers bound to it, so a change
/// is reviewed before it is applied. Each edit is one transaction.
struct TokensPanel: View {
    @ObservedObject var model: WorkspaceModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if model.tokenRows.isEmpty {
                ContentUnavailableView("Токенов нет", systemImage: "swatchpalette",
                                       description: Text("Токены описывают цвета, отступы и другие значения, общие для слоёв."))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(model.tokenRows) { row in
                    TokenRowView(model: model, row: row)
                }
                .listStyle(.inset)
            }
        }
        .accessibilityIdentifier("tokens-panel")
    }
}

private struct TokenRowView: View {
    @ObservedObject var model: WorkspaceModel
    let row: WorkspaceModel.TokenRow
    @StoredState private var typed: String?

    init(model: WorkspaceModel, row: WorkspaceModel.TokenRow) {
        self.model = model
        self.row = row
    }

    private var text: String { typed ?? row.value }
    private var bound: [String] { model.bindings(forToken: row.id) }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Text(row.name).font(.body.monospaced()).lineLimit(1)
                Spacer(minLength: 8)
                TextField("", text: Binding(get: { text }, set: { typed = $0 }))
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 120)
                    .disabled(model.writesBlocked || row.isBoolean)
                    .onSubmit { commit() }
                    .onChange(of: model.draftVersion) { typed = nil }
                    .accessibilityIdentifier("token-\(row.name)")
                if row.isBoolean {
                    Toggle("", isOn: Binding(
                        get: { row.value == "true" },
                        set: { model.updateToken(row, value: $0 ? "true" : "false") }))
                        .toggleStyle(.switch)
                        .controlSize(.small)
                        .labelsHidden()
                }
            }
            Text(bound.isEmpty ? "Не используется" : "Слоёв с привязкой: \(bound.count)")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 2)
    }

    private func commit() {
        guard let typed, typed != row.value else { return }
        model.updateToken(row, value: typed)
    }
}

/// "Restore as text" (C.8): asks for the string and writes a text layer over
/// the original box; the image or vector remains in history.
struct RestoreTextSheet: View {
    @ObservedObject var model: WorkspaceModel
    let nodeId: String
    @StoredState private var text = ""
    @StoredState private var font = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Восстановить как текст").font(.headline)
            Text("Изображение заменяется текстовым слоем в тех же границах. Исходный слой можно вернуть отменой.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextField("Текст", text: $text)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("restore-text-content")
            TextField("Шрифт (необязательно)", text: $font)
                .textFieldStyle(.roundedBorder)
            HStack {
                Spacer()
                Button("Отмена", role: .cancel) { model.restoreTextTarget = nil }
                    .keyboardShortcut(.cancelAction)
                Button("Восстановить") {
                    model.restoreAsText(nodeId, text: text, fontFamily: font.isEmpty ? nil : font)
                    model.restoreTextTarget = nil
                }
                .keyboardShortcut(.defaultAction)
                .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityIdentifier("restore-text-confirm")
            }
        }
        .padding(20)
        .frame(width: 420)
    }
}
